//! The Hopwyn service: capture, injection, discovery, pairing and the
//! encrypted link, driven by the handoff engine. Runs on its own thread;
//! the UI talks to it with [`Command`]s and hears back through [`UiEvent`]s.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hopwyn_core::{Action, Config, Engine, Input, State};
use hopwyn_discovery::{Discovery, DiscoveryEvent};
use hopwyn_input::InputEvent;
use hopwyn_input::capture::CaptureEvent;
use hopwyn_input::linux::LinuxEmulator;
use hopwyn_input::linux_capture::LinuxCapture;
use hopwyn_pair::{DeviceId, Identity, PairedPeer, PairingCode, PairingLimiter};
use hopwyn_proto::{Edge, Message};
use hopwyn_transport::{Node, PeerConnection, PeerSender, TrustStore};
use tokio::sync::{mpsc, watch};

use crate::config::{self, Partner, Store};
use crate::pairing_net::{self, PAIRING_PORT, PairNetError};

const LINK_PORT: u16 = 4242;
const TICK: Duration = Duration::from_millis(100);
const RETRY: Duration = Duration::from_secs(2);
const SESSION_SETTLE: Duration = Duration::from_millis(150);
/// Far enough to cross any screen in one move.
const PUSH: f32 = 20_000.0;

/// What the UI asks for.
#[derive(Debug, Clone)]
pub enum Command {
    /// Open the pairing window and show a code.
    AddDevice,
    CancelPairing,
    /// Pair with a nearby device by typing the code it shows.
    JoinDevice {
        ip: IpAddr,
        code: String,
    },
    /// Where the partner's screen sits relative to this one.
    SetEdge(Edge),
    /// Forget the partner device.
    Forget,
    Quit,
}

/// Who controls what, for the status card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// This computer's own mouse and keyboard drive it.
    Local,
    /// This computer's mouse and keyboard drive the partner.
    ControllingPartner,
    /// The partner's mouse and keyboard drive this computer.
    ControlledByPartner,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    NoPartner,
    Searching,
    Connected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nearby {
    pub name: String,
    pub ip: IpAddr,
    pub paired: bool,
}

/// What the UI hears.
#[derive(Debug, Clone)]
pub enum UiEvent {
    /// A setup step that may need the user (a desktop permission dialog).
    Waiting(String),
    Ready {
        name: String,
        short_id: String,
    },
    Status {
        link: Link,
        control: Control,
    },
    Partner(Option<Partner>),
    Nearby(Vec<Nearby>),
    PairingCode(String),
    PairingDone(String),
    PairingFailed(String),
    Error(String),
    /// The service stopped; commands no longer work.
    Fatal(String),
}

/// Starts the service on its own thread.
pub fn spawn(ui: async_channel::Sender<UiEvent>) -> mpsc::UnboundedSender<Command> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name("hopwyn-service".into())
        .spawn(move || {
            if let Err(e) = run(&ui, rx) {
                let _ = ui.send_blocking(UiEvent::Fatal(e));
            }
        })
        .ok();
    tx
}

fn run(
    ui: &async_channel::Sender<UiEvent>,
    cmds: mpsc::UnboundedReceiver<Command>,
) -> Result<(), String> {
    let store = Store::open_default()?;
    let identity = Arc::new(store.identity(&config::device_name())?);
    let partner = store.partner();

    // Capture first: on GNOME its dialog fails to appear if requested right
    // after a remote-control session closes.
    let _ = ui.send_blocking(UiEvent::Waiting(
        "Allow Hopwyn to capture input in the desktop's dialog".into(),
    ));
    let edge = partner.as_ref().map_or(Edge::Right, |p| p.edge);
    let (capture, capture_rx) =
        LinuxCapture::start_with_token(edge, store.capture_token()).map_err(|e| e.to_string())?;
    if let Some(t) = capture.restore_token() {
        let _ = store.save_capture_token(t);
    }

    // The remote-control permission: ask once, then reuse the desktop's token.
    let mut token = store.restore_token();
    if token.is_none() {
        let _ = ui.send_blocking(UiEvent::Waiting(
            "Allow Hopwyn to control input in the desktop's dialog".into(),
        ));
        let emu = LinuxEmulator::connect(None).map_err(|e| e.to_string())?;
        token = emu.restore_token().map(str::to_owned);
        if let Some(t) = &token {
            let _ = store.save_restore_token(t);
        }
    }
    std::thread::sleep(SESSION_SETTLE);
    let _ = capture.enable();

    let _ = ui.send_blocking(UiEvent::Ready {
        name: identity.name().to_owned(),
        short_id: identity.id().to_string(),
    });

    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    // Service::new opens sockets, so it must run inside the runtime.
    rt.block_on(async move {
        Service::new(
            ui.clone(),
            store,
            identity,
            partner,
            token,
            capture,
            capture_rx,
        )?
        .run(cmds)
        .await
    })
}

/// Everything the main loop reacts to besides UI commands.
enum Internal {
    Capture(CaptureEvent),
    Connected(PeerSender, mpsc::UnboundedReceiver<Message>),
    PeerMessage(Message),
    PeerGone,
    Nearby(HashMap<DeviceId, (String, SocketAddr)>),
    Paired(
        Result<PairedPeer, PairNetError>,
        bool,
        Option<PairingLimiter>,
    ),
}

struct Service {
    ui: async_channel::Sender<UiEvent>,
    store: Store,
    identity: Arc<Identity>,
    partner: Option<Partner>,
    partner_tx: watch::Sender<Option<Partner>>,
    token: Option<String>,
    capture: LinuxCapture,
    trust: TrustStore,
    tx: mpsc::UnboundedSender<Internal>,
    rx: mpsc::UnboundedReceiver<Internal>,
    _discovery: Discovery,
    engine: Engine,
    sender: Option<PeerSender>,
    emulator: Option<LinuxEmulator>,
    redial_tx: mpsc::UnboundedSender<()>,
    nearby: HashMap<DeviceId, (String, SocketAddr)>,
    pairing: Option<tokio::task::JoinHandle<()>>,
    limiter: PairingLimiter,
    last_status: Option<(Link, Control)>,
}

impl Service {
    fn new(
        ui: async_channel::Sender<UiEvent>,
        store: Store,
        identity: Arc<Identity>,
        partner: Option<Partner>,
        token: Option<String>,
        capture: LinuxCapture,
        capture_rx: std::sync::mpsc::Receiver<CaptureEvent>,
    ) -> Result<Self, String> {
        let (tx, rx) = mpsc::unbounded_channel();
        let trust = TrustStore::new();
        if let Some(p) = &partner {
            trust.add(p.id);
        }
        let node = Arc::new(
            Node::bind(
                &identity,
                SocketAddr::from(([0, 0, 0, 0], LINK_PORT)),
                trust.clone(),
            )
            .map_err(|e| format!("could not open UDP port {LINK_PORT}: {e}"))?,
        );
        let (discovery, found) =
            Discovery::start(&identity, LINK_PORT).map_err(|e| e.to_string())?;
        let (partner_tx, partner_rx) = watch::channel(partner.clone());
        let addresses: Arc<Mutex<HashMap<DeviceId, SocketAddr>>> = Arc::default();

        // Capture events: std channel -> loop.
        let cap_tx = tx.clone();
        std::thread::spawn(move || {
            while let Ok(ev) = capture_rx.recv() {
                if cap_tx.send(Internal::Capture(ev)).is_err() {
                    break;
                }
            }
        });

        // Discovery: keep the nearby list and the partner's address current.
        let disc_tx = tx.clone();
        let disc_addrs = addresses.clone();
        std::thread::spawn(move || {
            let mut seen: HashMap<DeviceId, (String, SocketAddr)> = HashMap::new();
            while let Ok(ev) = found.recv() {
                match ev {
                    DiscoveryEvent::Found(d) => {
                        if let Some(a) = d.addrs.iter().copied().find(SocketAddr::is_ipv4) {
                            seen.insert(d.id, (d.name, a));
                            if let Ok(mut m) = disc_addrs.lock() {
                                m.insert(d.id, a);
                            }
                        }
                    }
                    DiscoveryEvent::Lost(id) => {
                        seen.remove(&id);
                    }
                }
                if disc_tx.send(Internal::Nearby(seen.clone())).is_err() {
                    break;
                }
            }
        });

        // Incoming links, from the current partner only.
        let acc_tx = tx.clone();
        let acc_node = node.clone();
        let acc_partner = partner_rx.clone();
        tokio::spawn(async move {
            while let Some(result) = acc_node.accept().await {
                let want = acc_partner.borrow().as_ref().map(|p| p.id);
                match result {
                    Ok(conn) if Some(conn.peer()) == want => {
                        if hand_over(conn, &acc_tx).is_err() {
                            break;
                        }
                    }
                    Ok(conn) => conn.close(),
                    Err(_) => {}
                }
            }
        });

        // Outgoing links: the device with the higher key dials, so there is
        // never a duplicate connection.
        let (redial_tx, mut redial_rx) = mpsc::unbounded_channel::<()>();
        let dial_tx = tx.clone();
        let me = identity.id();
        let mut dial_partner = partner_rx;
        tokio::spawn(async move {
            loop {
                let target = dial_partner.borrow().clone();
                let Some(p) = target.filter(|p| me.as_bytes() > p.id.as_bytes()) else {
                    if dial_partner.changed().await.is_err() {
                        return;
                    }
                    continue;
                };
                let addr = addresses.lock().ok().and_then(|m| m.get(&p.id).copied());
                if let Some(addr) = addr
                    && let Ok(conn) = node.connect(addr).await
                {
                    if conn.peer() == p.id {
                        if hand_over(conn, &dial_tx).is_err() {
                            return;
                        }
                        if redial_rx.recv().await.is_none() {
                            return;
                        }
                        continue;
                    }
                    conn.close();
                }
                tokio::select! {
                    _ = tokio::time::sleep(RETRY) => {}
                    r = dial_partner.changed() => if r.is_err() { return },
                }
            }
        });

        let engine = Engine::new(engine_config(&identity, partner.as_ref()));
        Ok(Self {
            ui,
            store,
            identity,
            partner,
            partner_tx,
            token,
            capture,
            trust,
            tx,
            rx,
            _discovery: discovery,
            engine,
            sender: None,
            emulator: None,
            redial_tx,
            nearby: HashMap::new(),
            pairing: None,
            limiter: PairingLimiter::new(),
            last_status: None,
        })
    }

    fn send_ui(&self, ev: UiEvent) {
        let _ = self.ui.send_blocking(ev);
    }

    async fn run(mut self, mut cmds: mpsc::UnboundedReceiver<Command>) -> Result<(), String> {
        self.send_ui(UiEvent::Partner(self.partner.clone()));
        self.publish_status();
        let mut tick = tokio::time::interval(TICK);
        loop {
            let input = tokio::select! {
                _ = tick.tick() => Some(Input::Tick),
                cmd = cmds.recv() => match cmd {
                    None | Some(Command::Quit) => break,
                    Some(c) => { self.on_command(c); None }
                },
                ev = self.rx.recv() => match ev {
                    None => break,
                    Some(ev) => self.on_internal(ev),
                },
            };
            if let Some(input) = input {
                self.step(input).await;
            }
            self.publish_status();
        }
        Ok(())
    }

    fn on_internal(&mut self, ev: Internal) -> Option<Input> {
        match ev {
            Internal::Capture(c) => Some(Input::Capture(c)),
            Internal::PeerMessage(m) => Some(Input::Peer(m)),
            Internal::Connected(s, mut rx) => {
                if let Some(old) = self.sender.replace(s) {
                    old.close();
                }
                let fwd = self.tx.clone();
                tokio::spawn(async move {
                    while let Some(m) = rx.recv().await {
                        if fwd.send(Internal::PeerMessage(m)).is_err() {
                            return;
                        }
                    }
                    let _ = fwd.send(Internal::PeerGone);
                });
                Some(Input::PeerConnected)
            }
            Internal::PeerGone => {
                self.sender = None;
                let _ = self.redial_tx.send(());
                Some(Input::PeerDisconnected)
            }
            Internal::Nearby(map) => {
                self.nearby = map;
                self.publish_nearby();
                None
            }
            Internal::Paired(result, initiator, limiter) => {
                self.pairing = None;
                if let Some(l) = limiter {
                    self.limiter = l;
                }
                match result {
                    Ok(peer) => self.adopt_partner(peer, initiator),
                    Err(e) => self.send_ui(UiEvent::PairingFailed(e.to_string())),
                }
                None
            }
        }
    }

    fn on_command(&mut self, cmd: Command) {
        match cmd {
            Command::AddDevice => self.start_pairing(),
            Command::CancelPairing => {
                if let Some(h) = self.pairing.take() {
                    h.abort();
                }
                self.limiter.close();
            }
            Command::JoinDevice { ip, code } => {
                let Some(code) = PairingCode::parse(&code) else {
                    self.send_ui(UiEvent::PairingFailed(
                        "enter the 6 digits shown on the other device".into(),
                    ));
                    return;
                };
                if let Some(h) = self.pairing.take() {
                    h.abort();
                }
                let identity = self.identity.clone();
                let tx = self.tx.clone();
                self.pairing = Some(tokio::spawn(async move {
                    let r = pairing_net::run_joiner(
                        SocketAddr::new(ip, PAIRING_PORT),
                        &identity,
                        &code,
                    )
                    .await;
                    let _ = tx.send(Internal::Paired(r, false, None));
                }));
            }
            Command::SetEdge(edge) => {
                if let Some(p) = self.partner.as_mut() {
                    p.edge = edge;
                    let p = p.clone();
                    let _ = self.store.save_partner(Some(&p));
                    let _ = self.capture.set_edge(edge);
                    self.reset_engine();
                    self.send_ui(UiEvent::Partner(Some(p)));
                }
            }
            Command::Forget => {
                if let Some(p) = self.partner.take() {
                    self.trust.remove(&p.id);
                }
                let _ = self.store.save_partner(None);
                if let Some(s) = self.sender.take() {
                    s.close();
                }
                let _ = self.partner_tx.send(None);
                self.reset_engine();
                self.send_ui(UiEvent::Partner(None));
                self.publish_nearby();
            }
            Command::Quit => {}
        }
    }

    fn start_pairing(&mut self) {
        if let Some(h) = self.pairing.take() {
            h.abort();
        }
        let mut limiter = std::mem::take(&mut self.limiter);
        if let Err(e) = limiter.check(Instant::now())
            && !matches!(e, hopwyn_pair::LimiterError::WindowClosed)
        {
            self.limiter = limiter;
            self.send_ui(UiEvent::PairingFailed(e.to_string()));
            return;
        }
        limiter.open(Instant::now());
        let code = PairingCode::generate();
        self.send_ui(UiEvent::PairingCode(code.as_str().to_owned()));
        let identity = self.identity.clone();
        let tx = self.tx.clone();
        self.pairing = Some(tokio::spawn(async move {
            let listener = match tokio::net::TcpListener::bind(("0.0.0.0", PAIRING_PORT)).await {
                Ok(l) => l,
                Err(e) => {
                    let _ = tx.send(Internal::Paired(
                        Err(PairNetError::Io(e.to_string())),
                        true,
                        Some(limiter),
                    ));
                    return;
                }
            };
            let r = pairing_net::run_initiator(listener, &identity, &code, &mut limiter).await;
            let _ = tx.send(Internal::Paired(r, true, Some(limiter)));
        }));
    }

    fn adopt_partner(&mut self, peer: PairedPeer, initiator: bool) {
        if let Some(old) = self.partner.take() {
            self.trust.remove(&old.id);
        }
        // Default layout: the code-showing device has the newcomer on its
        // right; the newcomer has it on the left. Either can change it.
        let edge = if initiator { Edge::Right } else { Edge::Left };
        let partner = Partner {
            id: peer.id,
            name: peer.name.clone(),
            edge,
        };
        self.trust.add(peer.id);
        let _ = self.store.save_partner(Some(&partner));
        let _ = self.capture.set_edge(edge);
        self.partner = Some(partner.clone());
        let _ = self.partner_tx.send(Some(partner.clone()));
        self.reset_engine();
        self.send_ui(UiEvent::PairingDone(peer.name));
        self.send_ui(UiEvent::Partner(Some(partner)));
        self.publish_nearby();
    }

    fn reset_engine(&mut self) {
        if matches!(self.engine.state(), State::Sending) {
            let _ = self.capture.release();
        }
        self.engine = Engine::new(engine_config(&self.identity, self.partner.as_ref()));
        if self.sender.is_some() {
            self.engine.handle(Input::PeerConnected, Instant::now());
        }
    }

    async fn step(&mut self, input: Input) {
        if self.partner.is_none() && matches!(input, Input::Capture(CaptureEvent::Activated { .. }))
        {
            let _ = self.capture.release();
            return;
        }
        let mut lost = false;
        for action in self.engine.handle(input, Instant::now()) {
            match action {
                Action::Send(m) => {
                    if let Some(s) = self.sender.as_mut()
                        && s.send(&m).await.is_err()
                    {
                        lost = true;
                    }
                }
                Action::Inject(ev) => {
                    if let Some(emu) = self.emulator.as_ref() {
                        let _ = emu.send(ev);
                    }
                }
                Action::ReleaseCapture { position } => {
                    let _ = match position {
                        Some(p) => self.capture.release_at(p),
                        None => self.capture.release(),
                    };
                }
                Action::PushToEdge => self.push_to_edge(),
            }
        }
        if lost {
            if let Some(s) = self.sender.take() {
                s.close();
            }
            for action in self.engine.handle(Input::PeerDisconnected, Instant::now()) {
                if let Action::ReleaseCapture { .. } = action {
                    let _ = self.capture.release();
                }
            }
        }
        // Not driven by the partner any more: close the remote-control
        // session and re-arm capture.
        if !self.engine.needs_injection() && self.emulator.is_some() {
            self.emulator = None;
            tokio::time::sleep(SESSION_SETTLE).await;
            let _ = self.capture.enable();
        }
    }

    fn push_to_edge(&mut self) {
        if self.emulator.is_none() {
            let restore = self.token.clone();
            match tokio::task::block_in_place(|| LinuxEmulator::connect(restore.as_deref())) {
                Ok(e) => {
                    if let Some(t) = e.restore_token()
                        && self.token.as_deref() != Some(t)
                    {
                        let _ = self.store.save_restore_token(t);
                        self.token = Some(t.to_owned());
                    }
                    self.emulator = Some(e);
                }
                Err(e) => self.send_ui(UiEvent::Error(format!(
                    "could not take control of this desktop: {e}"
                ))),
            }
        }
        let edge = self.partner.as_ref().map_or(Edge::Right, |p| p.edge);
        let (dx, dy) = match edge {
            Edge::Right => (PUSH, 0.0),
            Edge::Left => (-PUSH, 0.0),
            Edge::Top => (0.0, -PUSH),
            Edge::Bottom => (0.0, PUSH),
        };
        if let Some(emu) = self.emulator.as_ref() {
            let _ = emu.send(InputEvent::Motion { dx, dy });
        }
    }

    fn publish_status(&mut self) {
        let link = match (&self.partner, &self.sender) {
            (None, _) => Link::NoPartner,
            (Some(_), None) => Link::Searching,
            (Some(_), Some(_)) => Link::Connected,
        };
        let control = match self.engine.state() {
            State::Idle => Control::Local,
            State::Sending => Control::ControllingPartner,
            State::Entering { .. } | State::Receiving => Control::ControlledByPartner,
        };
        if self.last_status.as_ref() != Some(&(link.clone(), control)) {
            self.last_status = Some((link.clone(), control));
            self.send_ui(UiEvent::Status { link, control });
        }
    }

    fn publish_nearby(&self) {
        let paired = self.partner.as_ref().map(|p| p.id);
        let mut list: Vec<Nearby> = self
            .nearby
            .iter()
            .map(|(id, (name, addr))| Nearby {
                name: name.clone(),
                ip: addr.ip(),
                paired: Some(*id) == paired,
            })
            .collect();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        self.send_ui(UiEvent::Nearby(list));
    }
}

fn engine_config(identity: &Identity, partner: Option<&Partner>) -> Config {
    match partner {
        Some(p) => Config {
            edge: p.edge,
            wins_ties: identity.id().as_bytes() < p.id.as_bytes(),
        },
        None => Config {
            edge: Edge::Right,
            wins_ties: true,
        },
    }
}

fn hand_over(conn: PeerConnection, tx: &mpsc::UnboundedSender<Internal>) -> Result<(), ()> {
    let (sender, mut receiver) = conn.split();
    let (msg_tx, msg_rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok(m) = receiver.recv().await {
            if msg_tx.send(m).is_err() {
                return;
            }
        }
    });
    tx.send(Internal::Connected(sender, msg_rx)).map_err(|_| ())
}
