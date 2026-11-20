//! `hopwyn run`: the handoff loop.

use std::net::SocketAddr;
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use hopwyn_core::{Action, Config, Engine, Input, State};
use hopwyn_discovery::{Discovery, DiscoveryEvent};
use hopwyn_input::InputEvent;
use hopwyn_input::capture::CaptureEvent;
use hopwyn_input::linux::LinuxEmulator;
use hopwyn_input::linux_capture::LinuxCapture;
use hopwyn_pair::{DeviceId, Identity};
use hopwyn_proto::{Edge, Message};
use hopwyn_transport::{Node, PeerSender, TrustStore};
use tokio::sync::mpsc;

use crate::store;

const DEFAULT_PORT: u16 = 4242;
const RETRY: Duration = Duration::from_secs(2);
/// Time for the desktop to tear down a remote-control session before
/// capture is re-armed.
const SESSION_SETTLE: Duration = Duration::from_millis(150);
/// How often the engine checks its timeouts.
const TICK: Duration = Duration::from_millis(100);
/// Far enough to cross any screen in one move.
const PUSH: f32 = 20_000.0;

pub struct Options {
    edge: Edge,
    peer: DeviceId,
    connect: Option<SocketAddr>,
    port: u16,
}

impl Options {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut edge = None;
        let mut peer = None;
        let mut connect = None;
        let mut port = DEFAULT_PORT;
        let mut it = args.iter();
        while let Some(flag) = it.next() {
            let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
            match flag.as_str() {
                "--edge" => {
                    edge = Some(match value()?.as_str() {
                        "left" => Edge::Left,
                        "right" => Edge::Right,
                        "top" => Edge::Top,
                        "bottom" => Edge::Bottom,
                        other => return Err(format!("unknown edge {other:?}")),
                    });
                }
                "--trust" => {
                    peer = Some(
                        store::parse_hex_id(value()?)
                            .ok_or("--trust needs a 64-character key from `hopwyn id`")?,
                    );
                }
                "--connect" => {
                    connect = Some(value()?.parse().map_err(|_| "--connect needs ip:port")?)
                }
                "--port" => port = value()?.parse().map_err(|_| "--port needs a number")?,
                other => return Err(format!("unknown option {other:?}")),
            }
        }
        Ok(Self {
            edge: edge.ok_or("--edge is required")?,
            peer: peer.ok_or("--trust is required")?,
            connect,
            port,
        })
    }
}

/// Everything the loop reacts to.
enum Event {
    Capture(CaptureEvent),
    Connected(PeerSender, mpsc::UnboundedReceiver<Message>),
    PeerMessage(Message),
    PeerGone,
}

pub fn run(opts: Options) -> Result<(), String> {
    let identity = store::load_or_create_identity()?;
    let me = identity.id();
    if me == opts.peer {
        return Err("--trust is this device's own key".into());
    }
    let wins_ties = me.as_bytes() < opts.peer.as_bytes();
    eprintln!("This device: {} ({me})", identity.name());
    eprintln!("Peer beyond the {:?} edge: {}", opts.edge, opts.peer);

    // Both desktop permission dialogs come first, before any networking.
    // The remote-control session is closed straight away: GNOME disables
    // edge capture while one is open, so it is reopened (silently, with the
    // restore token) only while the peer is driving this device.
    // Capture is asked for first: on GNOME its dialog fails to appear when
    // requested right after a remote-control session closes.
    eprintln!("Asking the desktop for permission to capture input (dialog 1 of 2)...");
    let (capture, capture_rx) = LinuxCapture::start(opts.edge).map_err(|e| e.to_string())?;
    eprintln!("Asking the desktop for permission to control input (dialog 2 of 2)...");
    let token = {
        let emu = LinuxEmulator::connect(None).map_err(|e| e.to_string())?;
        emu.restore_token().map(str::to_owned)
    };
    if token.is_none() {
        eprintln!("Note: the desktop gave no restore token, so it may ask again on each handoff.");
    }
    // The remote-control session just closed; make sure capture is armed.
    std::thread::sleep(SESSION_SETTLE);
    let _ = capture.enable();

    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(event_loop(
        identity, opts, wins_ties, token, capture, capture_rx,
    ))
}

async fn event_loop(
    identity: Identity,
    opts: Options,
    wins_ties: bool,
    mut token: Option<String>,
    capture: LinuxCapture,
    capture_rx: std_mpsc::Receiver<CaptureEvent>,
) -> Result<(), String> {
    let trust = TrustStore::new();
    trust.add(opts.peer);
    let bind: SocketAddr = ([0, 0, 0, 0], opts.port).into();
    let node = std::sync::Arc::new(Node::bind(&identity, bind, trust).map_err(|e| e.to_string())?);
    let port = node.local_addr().map_err(|e| e.to_string())?.port();
    let (_discovery, found) = Discovery::start(&identity, port).map_err(|e| e.to_string())?;
    eprintln!("Listening on UDP port {port}.");

    let (tx, mut events) = mpsc::unbounded_channel::<Event>();

    // Capture events arrive on a std channel; bridge them in.
    let cap_tx = tx.clone();
    std::thread::spawn(move || {
        while let Ok(ev) = capture_rx.recv() {
            if cap_tx.send(Event::Capture(ev)).is_err() {
                break;
            }
        }
    });

    // Incoming connections (always), from the trusted peer only.
    let accept_tx = tx.clone();
    let accept_node = node.clone();
    let peer = opts.peer;
    tokio::spawn(async move {
        while let Some(result) = accept_node.accept().await {
            match result {
                Ok(conn) if conn.peer() == peer => {
                    if hand_over(conn, &accept_tx).is_err() {
                        break;
                    }
                }
                Ok(conn) => conn.close(),
                Err(e) => eprintln!("Rejected a connection: {e}"),
            }
        }
    });

    // Outgoing connections: one side dials, so there is never a duplicate.
    let dials = opts.connect.is_some() || !wins_ties;
    let (want_dial_tx, mut want_dial) = mpsc::unbounded_channel::<()>();
    if dials {
        let dial_tx = tx.clone();
        let dial_node = node.clone();
        let fixed = opts.connect;
        let (addr_tx, addr_rx) = std_mpsc::channel::<SocketAddr>();
        std::thread::spawn(move || {
            while let Ok(ev) = found.recv() {
                if let DiscoveryEvent::Found(d) = ev
                    && d.id == peer
                {
                    // The node is bound to IPv4, and IPv6 link-local
                    // addresses need an interface scope mDNS doesn't
                    // give us, so dial the peer's IPv4 address.
                    if let Some(a) = d.addrs.into_iter().find(SocketAddr::is_ipv4) {
                        let _ = addr_tx.send(a);
                    }
                }
            }
        });
        tokio::spawn(async move {
            let mut last_seen: Option<SocketAddr> = fixed;
            loop {
                while let Ok(a) = addr_rx.try_recv() {
                    if fixed.is_none() {
                        last_seen = Some(a);
                    }
                }
                if let Some(addr) = last_seen {
                    match dial_node.connect(addr).await {
                        Ok(conn) if conn.peer() == peer => {
                            if hand_over(conn, &dial_tx).is_err() {
                                return;
                            }
                            // Wait until the session ends before dialing again.
                            if want_dial.recv().await.is_none() {
                                return;
                            }
                            continue;
                        }
                        Ok(conn) => conn.close(),
                        Err(e) => eprintln!("Could not reach the peer at {addr}: {e}"),
                    }
                } else {
                    eprintln!("Looking for the peer on the home network...");
                }
                tokio::time::sleep(RETRY).await;
            }
        });
    } else {
        drop(found);
        eprintln!("Waiting for the peer to connect...");
    }

    let mut engine = Engine::new(Config {
        edge: opts.edge,
        wins_ties,
    });
    let mut sender: Option<PeerSender> = None;
    // Open only while the peer drives this device (see `run`).
    let mut emulator: Option<LinuxEmulator> = None;
    let mut last_state = engine.state();
    let mut tick = tokio::time::interval(TICK);

    loop {
        let input = tokio::select! {
            _ = tick.tick() => Input::Tick,
            ev = events.recv() => match ev {
                None => break,
                Some(Event::Capture(c)) => {
                    if debug() && !matches!(c, CaptureEvent::Input(_)) {
                        eprintln!("[debug {}] capture: {c:?}", stamp());
                    }
                    Input::Capture(c)
                }
                Some(Event::PeerMessage(m)) => {
                    if debug() && matches!(m, Message::Enter { .. } | Message::Leave) {
                        eprintln!("[debug {}] peer: {m:?}", stamp());
                    }
                    Input::Peer(m)
                }
                Some(Event::Connected(s, mut rx)) => {
                    eprintln!("Connected to {}.", s.peer_name());
                    if let Some(old) = sender.replace(s) {
                        old.close();
                    }
                    let fwd = tx.clone();
                    tokio::spawn(async move {
                        while let Some(m) = rx.recv().await {
                            if fwd.send(Event::PeerMessage(m)).is_err() {
                                return;
                            }
                        }
                        let _ = fwd.send(Event::PeerGone);
                    });
                    Input::PeerConnected
                }
                Some(Event::PeerGone) => {
                    eprintln!("Disconnected from the peer.");
                    sender = None;
                    if dials {
                        let _ = want_dial_tx.send(());
                    }
                    Input::PeerDisconnected
                }
            },
        };

        let mut lost = false;
        for action in engine.handle(input, Instant::now()) {
            match action {
                Action::Send(m) => {
                    if let Some(s) = sender.as_mut()
                        && s.send(&m).await.is_err()
                    {
                        lost = true;
                    }
                }
                Action::Inject(ev) => {
                    if let Some(emu) = emulator.as_ref() {
                        let _ = emu.send(ev);
                    }
                }
                Action::ReleaseCapture { position } => {
                    let _ = match position {
                        Some(p) => capture.release_at(p),
                        None => capture.release(),
                    };
                }
                Action::PushToEdge => {
                    if emulator.is_none() {
                        let restore = token.clone();
                        match tokio::task::block_in_place(|| {
                            LinuxEmulator::connect(restore.as_deref())
                        }) {
                            Ok(e) => {
                                if let Some(t) = e.restore_token() {
                                    token = Some(t.to_owned());
                                }
                                emulator = Some(e);
                            }
                            Err(e) => eprintln!("Could not take control of this desktop: {e}"),
                        }
                    }
                    let (dx, dy) = match opts.edge {
                        Edge::Right => (PUSH, 0.0),
                        Edge::Left => (-PUSH, 0.0),
                        Edge::Top => (0.0, -PUSH),
                        Edge::Bottom => (0.0, PUSH),
                    };
                    if let Some(emu) = emulator.as_ref() {
                        let _ = emu.send(InputEvent::Motion { dx, dy });
                    }
                }
            }
        }
        if lost {
            if let Some(s) = sender.take() {
                s.close();
            }
            for action in engine.handle(Input::PeerDisconnected, Instant::now()) {
                if let Action::ReleaseCapture { .. } = action {
                    let _ = capture.release();
                }
            }
        }

        // No longer driven by the peer: close the remote-control session so
        // the desktop allows edge capture again, then re-arm capture.
        if !engine.needs_injection() && emulator.is_some() {
            emulator = None;
            tokio::time::sleep(SESSION_SETTLE).await;
            let _ = capture.enable();
        }

        let state = engine.state();
        if std::mem::discriminant(&state) != std::mem::discriminant(&last_state) {
            eprintln!(
                "{}",
                match state {
                    State::Idle => "This device is active (own input).",
                    State::Sending => "This device is inactive: input goes to the peer.",
                    State::Entering { .. } => "The peer handed over control; placing the cursor.",
                    State::Receiving => "This device is active: driven by the peer.",
                }
            );
            last_state = state;
        }
    }
    Ok(())
}

/// Splits a fresh connection and starts its reader task.
fn hand_over(
    conn: hopwyn_transport::PeerConnection,
    tx: &mpsc::UnboundedSender<Event>,
) -> Result<(), ()> {
    let (sender, mut receiver) = conn.split();
    let (msg_tx, msg_rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok(m) = receiver.recv().await {
            if msg_tx.send(m).is_err() {
                return;
            }
        }
    });
    tx.send(Event::Connected(sender, msg_rx)).map_err(|_| ())
}

/// Extra logging with `HOPWYN_DEBUG=1`: state changes and edge events only,
/// never which keys were typed.
fn debug() -> bool {
    std::env::var_os("HOPWYN_DEBUG").is_some()
}

/// Wall-clock time of day (UTC) with milliseconds, for debug logs.
fn stamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() % 86_400;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60,
        now.subsec_millis()
    )
}
