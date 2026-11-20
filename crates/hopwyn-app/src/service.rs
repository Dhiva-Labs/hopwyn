//! The Hopwyn service: capture, injection, discovery, pairing and the
//! encrypted links, driven by one handoff engine per paired computer. Runs
//! on its own thread; the UI talks to it with [`Command`]s and hears back
//! through [`UiEvent`]s.
//!
//! Up to four computers can be paired, one beyond each screen edge. Only
//! one of them takes part in a handoff at a time: while the cursor is on
//! (or coming from) one neighbour, the others are told to keep their own.

use std::collections::{HashMap, HashSet};
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

use crate::config::{self, MAX_PARTNERS, Partner, Store};
use crate::pairing_net::{self, PAIRING_PORT, PairNetError};

const LINK_PORT: u16 = 4242;
const TICK: Duration = Duration::from_millis(100);
const RETRY: Duration = Duration::from_secs(2);
const DIAL_TIMEOUT: Duration = Duration::from_secs(4);
const SESSION_SETTLE: Duration = Duration::from_millis(150);
/// Far enough to cross any screen in one move.
const PUSH: f32 = 20_000.0;
const ALL_EDGES: [Edge; 4] = [Edge::Right, Edge::Left, Edge::Top, Edge::Bottom];

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
    /// Where a paired computer's screen sits relative to this one. If
    /// another paired computer is on that side, the two swap.
    SetEdge {
        id: DeviceId,
        edge: Edge,
    },
    /// Forget a paired computer.
    Forget(DeviceId),
    Quit,
}

/// Who controls what, for the status card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// This computer's own mouse and keyboard drive it.
    Local,
    /// This computer's mouse and keyboard drive that paired computer.
    Controlling(DeviceId),
    /// That paired computer's mouse and keyboard drive this one.
    ControlledBy(DeviceId),
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
    /// Which paired computers are connected, and who is in control.
    Status {
        connected: Vec<DeviceId>,
        control: Control,
    },
    Partners(Vec<Partner>),
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
    let partners = store.partners();

    // Capture first: on GNOME its dialog fails to appear if requested right
    // after a remote-control session closes.
    let _ = ui.send_blocking(UiEvent::Waiting(
        "Allow Hopwyn to capture input in the desktop's dialog".into(),
    ));
    let (capture, capture_rx) =
        LinuxCapture::start_with_token(capture_edges(&partners), store.capture_token())
            .map_err(|e| e.to_string())?;
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
            partners,
            token,
            capture,
            capture_rx,
        )?
        .run(cmds)
        .await
    })
}

/// The edges to put barriers on. With nothing paired, keep one on the
/// right so the capture session stays valid; hits there are released.
fn capture_edges(partners: &[Partner]) -> Vec<Edge> {
    if partners.is_empty() {
        vec![Edge::Right]
    } else {
        partners.iter().map(|p| p.edge).collect()
    }
}

/// Everything the main loop reacts to besides UI commands.
enum Internal {
    Capture(CaptureEvent),
    /// A link to a paired computer is up.
    Connected(DeviceId, PeerSender, mpsc::UnboundedReceiver<Message>),
    /// A message on a link; the number tells a link's messages apart from
    /// those of the link it replaced.
    PeerMessage(DeviceId, u64, Message),
    PeerGone(DeviceId, u64),
    Nearby(HashMap<DeviceId, (String, SocketAddr)>),
    Paired(
        Result<PairedPeer, PairNetError>,
        bool,
        Option<PairingLimiter>,
    ),
}

/// One paired computer and the state of the handoff with it.
struct Peer {
    partner: Partner,
    engine: Engine,
    sender: Option<PeerSender>,
    generation: u64,
}

struct Service {
    ui: async_channel::Sender<UiEvent>,
    store: Store,
    identity: Arc<Identity>,
    peers: Vec<Peer>,
    partners_tx: watch::Sender<Vec<Partner>>,
    connected: Arc<Mutex<HashSet<DeviceId>>>,
    token: Option<String>,
    capture: LinuxCapture,
    trust: TrustStore,
    tx: mpsc::UnboundedSender<Internal>,
    rx: mpsc::UnboundedReceiver<Internal>,
    _discovery: Discovery,
    emulator: Option<LinuxEmulator>,
    nearby: HashMap<DeviceId, (String, SocketAddr)>,
    pairing: Option<tokio::task::JoinHandle<()>>,
    limiter: PairingLimiter,
    next_generation: u64,
    last_status: Option<(Vec<DeviceId>, Control)>,
}

impl Service {
    fn new(
        ui: async_channel::Sender<UiEvent>,
        store: Store,
        identity: Arc<Identity>,
        partners: Vec<Partner>,
        token: Option<String>,
        capture: LinuxCapture,
        capture_rx: std::sync::mpsc::Receiver<CaptureEvent>,
    ) -> Result<Self, String> {
        let (tx, rx) = mpsc::unbounded_channel();
        let trust = TrustStore::new();
        for p in &partners {
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
        let (partners_tx, partners_rx) = watch::channel(partners.clone());
        let addresses: Arc<Mutex<HashMap<DeviceId, SocketAddr>>> = Arc::default();
        let connected: Arc<Mutex<HashSet<DeviceId>>> = Arc::default();

        // Capture events: std channel -> loop.
        let cap_tx = tx.clone();
        std::thread::spawn(move || {
            while let Ok(ev) = capture_rx.recv() {
                if cap_tx.send(Internal::Capture(ev)).is_err() {
                    break;
                }
            }
        });

        // Discovery: keep the nearby list and paired computers' addresses current.
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

        // Incoming links, from paired computers only.
        let acc_tx = tx.clone();
        let acc_node = node.clone();
        let acc_partners = partners_rx.clone();
        tokio::spawn(async move {
            while let Some(result) = acc_node.accept().await {
                match result {
                    Ok(conn) if acc_partners.borrow().iter().any(|p| p.id == conn.peer()) => {
                        if hand_over(conn, &acc_tx).is_err() {
                            break;
                        }
                    }
                    Ok(conn) => conn.close(),
                    Err(_) => {}
                }
            }
        });

        // Outgoing links: for each pair of computers the one with the higher
        // key dials, so there is never a duplicate connection.
        let dial_tx = tx.clone();
        let me = identity.id();
        let mut dial_partners = partners_rx;
        let dial_connected = connected.clone();
        tokio::spawn(async move {
            loop {
                let targets: Vec<DeviceId> = dial_partners
                    .borrow()
                    .iter()
                    .map(|p| p.id)
                    .filter(|id| me.as_bytes() > id.as_bytes())
                    .collect();
                for id in targets {
                    let up = dial_connected
                        .lock()
                        .map(|c| c.contains(&id))
                        .unwrap_or(true);
                    let addr = addresses.lock().ok().and_then(|m| m.get(&id).copied());
                    let Some(addr) = addr.filter(|_| !up) else {
                        continue;
                    };
                    if let Ok(Ok(conn)) =
                        tokio::time::timeout(DIAL_TIMEOUT, node.connect(addr)).await
                    {
                        if conn.peer() != id {
                            conn.close();
                            continue;
                        }
                        if let Ok(mut c) = dial_connected.lock() {
                            c.insert(id);
                        }
                        if hand_over(conn, &dial_tx).is_err() {
                            return;
                        }
                    }
                }
                tokio::select! {
                    _ = tokio::time::sleep(RETRY) => {}
                    r = dial_partners.changed() => if r.is_err() { return },
                }
            }
        });

        let peers = partners
            .into_iter()
            .map(|partner| Peer {
                engine: Engine::new(engine_config(&identity, &partner)),
                partner,
                sender: None,
                generation: 0,
            })
            .collect();
        Ok(Self {
            ui,
            store,
            identity,
            peers,
            partners_tx,
            connected,
            token,
            capture,
            trust,
            tx,
            rx,
            _discovery: discovery,
            emulator: None,
            nearby: HashMap::new(),
            pairing: None,
            limiter: PairingLimiter::new(),
            next_generation: 1,
            last_status: None,
        })
    }

    fn send_ui(&self, ev: UiEvent) {
        let _ = self.ui.send_blocking(ev);
    }

    fn partners(&self) -> Vec<Partner> {
        self.peers.iter().map(|p| p.partner.clone()).collect()
    }

    fn peer_index(&self, id: &DeviceId) -> Option<usize> {
        self.peers.iter().position(|p| &p.partner.id == id)
    }

    /// The peer taking part in a handoff right now, if any.
    fn busy(&self) -> Option<usize> {
        self.peers
            .iter()
            .position(|p| p.engine.state() != State::Idle)
    }

    async fn run(mut self, mut cmds: mpsc::UnboundedReceiver<Command>) -> Result<(), String> {
        self.send_ui(UiEvent::Partners(self.partners()));
        self.publish_status();
        let mut tick = tokio::time::interval(TICK);
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    for i in 0..self.peers.len() {
                        self.step(i, Input::Tick).await;
                    }
                }
                cmd = cmds.recv() => match cmd {
                    None | Some(Command::Quit) => break,
                    Some(c) => self.on_command(c),
                },
                ev = self.rx.recv() => match ev {
                    None => break,
                    Some(ev) => self.on_internal(ev).await,
                },
            }
            self.close_idle_emulator().await;
            self.publish_status();
        }
        for p in &self.peers {
            if let Some(s) = &p.sender {
                s.close();
            }
        }
        Ok(())
    }

    async fn on_internal(&mut self, ev: Internal) {
        match ev {
            Internal::Capture(c) => self.on_capture(c).await,
            Internal::PeerMessage(id, generation, m) => {
                let Some(i) = self.peer_index(&id) else {
                    return;
                };
                if self.peers.get(i).map(|p| p.generation) != Some(generation) {
                    return;
                }
                // Someone else has the cursor: this one keeps its own.
                if matches!(m, Message::Enter { .. }) && self.busy().is_some_and(|b| b != i) {
                    self.debug(&format!("refusing Enter from {}", self.name(i)));
                    if let Some(s) = self.peers.get_mut(i).and_then(|p| p.sender.as_mut()) {
                        let _ = s.send(&Message::Leave).await;
                    }
                    return;
                }
                self.step(i, Input::Peer(m)).await;
            }
            Internal::Connected(id, sender, mut rx) => {
                let Some(i) = self.peer_index(&id) else {
                    sender.close();
                    if let Ok(mut c) = self.connected.lock() {
                        c.remove(&id);
                    }
                    return;
                };
                let generation = self.next_generation;
                self.next_generation += 1;
                if let Ok(mut c) = self.connected.lock() {
                    c.insert(id);
                }
                if let Some(peer) = self.peers.get_mut(i) {
                    if let Some(old) = peer.sender.replace(sender) {
                        old.close();
                    }
                    peer.generation = generation;
                }
                let fwd = self.tx.clone();
                tokio::spawn(async move {
                    while let Some(m) = rx.recv().await {
                        if fwd.send(Internal::PeerMessage(id, generation, m)).is_err() {
                            return;
                        }
                    }
                    let _ = fwd.send(Internal::PeerGone(id, generation));
                });
                self.debug(&format!("connected to {}", self.name(i)));
                self.step(i, Input::PeerConnected).await;
            }
            Internal::PeerGone(id, generation) => {
                let Some(i) = self.peer_index(&id) else {
                    return;
                };
                if self.peers.get(i).map(|p| p.generation) != Some(generation) {
                    return;
                }
                self.disconnect(i).await;
            }
            Internal::Nearby(map) => {
                self.nearby = map;
                self.publish_nearby();
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
            }
        }
    }

    /// Routes a capture event to the engine it belongs to.
    async fn on_capture(&mut self, ev: CaptureEvent) {
        let target = match (ev, self.busy()) {
            (CaptureEvent::Activated { edge, .. }, Some(b)) => {
                let state = self.peers.get(b).map(|p| p.engine.state());
                let facing = self.peers.get(b).map(|p| p.partner.edge) == Some(edge);
                match state {
                    // Placement, or the cursor coming back to that neighbour.
                    Some(State::Entering { .. }) => Some(b),
                    Some(State::Receiving) if facing => Some(b),
                    // Already sending: nothing new can start.
                    Some(State::Sending) => None,
                    // Controlled by one neighbour, the cursor hit another's
                    // edge: keep it here. (Passing it on is not supported yet.)
                    _ => {
                        let _ = self.capture.release();
                        None
                    }
                }
            }
            (CaptureEvent::Activated { edge, .. }, None) => {
                let i = self.peers.iter().position(|p| p.partner.edge == edge);
                if i.is_none() {
                    let _ = self.capture.release();
                }
                i
            }
            (_, busy) => busy,
        };
        if let Some(i) = target {
            self.step(i, Input::Capture(ev)).await;
        }
    }

    /// Feeds one engine and carries out what it asks for.
    async fn step(&mut self, i: usize, input: Input) {
        let now = Instant::now();
        let Some(peer) = self.peers.get_mut(i) else {
            return;
        };
        let before = peer.engine.state();
        let actions = peer.engine.handle(input.clone(), now);
        if debug_on()
            && !matches!(
                input,
                Input::Tick
                    | Input::Capture(CaptureEvent::Input(_))
                    | Input::Peer(Message::PointerMotion { .. })
            )
        {
            eprintln!(
                "[hopwyn {}] {}: {input:?} ({before:?} -> {:?})",
                stamp(),
                peer.partner.name,
                peer.engine.state()
            );
        }
        let edge = peer.partner.edge;
        let mut lost = false;
        for action in actions {
            if debug_on()
                && !matches!(
                    action,
                    Action::Inject(_) | Action::Send(Message::PointerMotion { .. })
                )
            {
                eprintln!("[hopwyn {}]   do: {action:?}", stamp());
            }
            match action {
                Action::Send(m) => {
                    if let Some(s) = self.peers.get_mut(i).and_then(|p| p.sender.as_mut())
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
                Action::PushToEdge => self.push_to_edge(edge),
            }
        }
        if lost {
            Box::pin(self.disconnect(i)).await;
        }
    }

    /// The link to peer `i` is gone: forget its sender, put its engine back.
    async fn disconnect(&mut self, i: usize) {
        let Some(peer) = self.peers.get_mut(i) else {
            return;
        };
        if let Some(s) = peer.sender.take() {
            s.close();
        }
        peer.generation = 0;
        let id = peer.partner.id;
        if let Ok(mut c) = self.connected.lock() {
            c.remove(&id);
        }
        self.debug(&format!("lost the link to {}", self.name(i)));
        self.step(i, Input::PeerDisconnected).await;
    }

    /// Not driven by any neighbour any more: close the remote-control
    /// session and re-arm capture.
    async fn close_idle_emulator(&mut self) {
        let needed = self.peers.iter().any(|p| p.engine.needs_injection());
        if !needed && self.emulator.is_some() {
            self.emulator = None;
            tokio::time::sleep(SESSION_SETTLE).await;
            let _ = self.capture.enable();
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
                if self.peers.len() >= MAX_PARTNERS {
                    self.send_ui(UiEvent::PairingFailed(full_message()));
                    return;
                }
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
            Command::SetEdge { id, edge } => {
                let Some(i) = self.peer_index(&id) else {
                    return;
                };
                let Some(old) = self.peers.get(i).map(|p| p.partner.edge) else {
                    return;
                };
                if old == edge {
                    return;
                }
                // Whoever was on that side moves to this one's old side.
                if let Some(j) = self.peers.iter().position(|p| p.partner.edge == edge) {
                    if let Some(p) = self.peers.get_mut(j) {
                        p.partner.edge = old;
                    }
                    self.reset_engine(j);
                }
                if let Some(p) = self.peers.get_mut(i) {
                    p.partner.edge = edge;
                }
                self.reset_engine(i);
                self.partners_changed();
            }
            Command::Forget(id) => {
                let Some(i) = self.peer_index(&id) else {
                    return;
                };
                if self
                    .peers
                    .get(i)
                    .is_some_and(|p| p.engine.state() == State::Sending)
                {
                    let _ = self.capture.release();
                }
                let peer = self.peers.remove(i);
                self.trust.remove(&peer.partner.id);
                if let Some(s) = peer.sender {
                    s.close();
                }
                if let Ok(mut c) = self.connected.lock() {
                    c.remove(&id);
                }
                self.partners_changed();
                self.publish_nearby();
            }
            Command::Quit => {}
        }
    }

    /// Saves the partner list and tells capture, the dialer and the UI.
    fn partners_changed(&mut self) {
        let partners = self.partners();
        let _ = self.store.save_partners(&partners);
        let _ = self.capture.set_edges(capture_edges(&partners));
        let _ = self.partners_tx.send(partners.clone());
        self.send_ui(UiEvent::Partners(partners));
    }

    fn start_pairing(&mut self) {
        if self.peers.len() >= MAX_PARTNERS {
            self.send_ui(UiEvent::PairingFailed(full_message()));
            return;
        }
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
        // Pairing again with a known computer just refreshes its name.
        if let Some(i) = self.peer_index(&peer.id) {
            if let Some(p) = self.peers.get_mut(i) {
                p.partner.name.clone_from(&peer.name);
            }
            self.partners_changed();
            self.send_ui(UiEvent::PairingDone(peer.name));
            return;
        }
        // Default layout: the code-showing computer has the newcomer on its
        // right, the newcomer has it on its left; otherwise the first free
        // side. Either can change it.
        let preferred = if initiator { Edge::Right } else { Edge::Left };
        let taken: Vec<Edge> = self.peers.iter().map(|p| p.partner.edge).collect();
        let Some(edge) = std::iter::once(preferred)
            .chain(ALL_EDGES)
            .find(|e| !taken.contains(e))
        else {
            self.send_ui(UiEvent::PairingFailed(full_message()));
            return;
        };
        let partner = Partner {
            id: peer.id,
            name: peer.name.clone(),
            edge,
        };
        self.trust.add(peer.id);
        self.peers.push(Peer {
            engine: Engine::new(engine_config(&self.identity, &partner)),
            partner,
            sender: None,
            generation: 0,
        });
        self.partners_changed();
        self.send_ui(UiEvent::PairingDone(peer.name));
        self.publish_nearby();
    }

    /// A fresh engine for peer `i` (its side changed).
    fn reset_engine(&mut self, i: usize) {
        let identity = self.identity.clone();
        let Some(peer) = self.peers.get_mut(i) else {
            return;
        };
        if peer.engine.state() == State::Sending {
            let _ = self.capture.release();
        }
        peer.engine = Engine::new(engine_config(&identity, &peer.partner));
        if peer.sender.is_some() {
            peer.engine.handle(Input::PeerConnected, Instant::now());
        }
    }

    fn push_to_edge(&mut self, edge: Edge) {
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
                Err(e) => {
                    self.debug(&format!("remote control failed: {e}"));
                    self.send_ui(UiEvent::Error(format!(
                        "could not take control of this desktop: {e}"
                    )));
                }
            }
        }
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
        let connected: Vec<DeviceId> = self
            .peers
            .iter()
            .filter(|p| p.sender.is_some())
            .map(|p| p.partner.id)
            .collect();
        let control = self
            .peers
            .iter()
            .find_map(|p| match p.engine.state() {
                State::Idle => None,
                State::Sending => Some(Control::Controlling(p.partner.id)),
                State::Entering { .. } | State::Receiving => {
                    Some(Control::ControlledBy(p.partner.id))
                }
            })
            .unwrap_or(Control::Local);
        let status = (connected, control);
        if self.last_status.as_ref() != Some(&status) {
            self.last_status = Some(status.clone());
            self.send_ui(UiEvent::Status {
                connected: status.0,
                control: status.1,
            });
        }
    }

    fn publish_nearby(&self) {
        let mut list: Vec<Nearby> = self
            .nearby
            .iter()
            .map(|(id, (name, addr))| Nearby {
                name: name.clone(),
                ip: addr.ip(),
                paired: self.peer_index(id).is_some(),
            })
            .collect();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        self.send_ui(UiEvent::Nearby(list));
    }

    fn name(&self, i: usize) -> String {
        self.peers
            .get(i)
            .map(|p| p.partner.name.clone())
            .unwrap_or_default()
    }

    fn debug(&self, text: &str) {
        if debug_on() {
            eprintln!("[hopwyn {}] {text}", stamp());
        }
    }
}

fn full_message() -> String {
    format!("Hopwyn is paired with {MAX_PARTNERS} computers, one on each side. Forget one first.")
}

fn engine_config(identity: &Identity, partner: &Partner) -> Config {
    Config {
        edge: partner.edge,
        wins_ties: identity.id().as_bytes() < partner.id.as_bytes(),
    }
}

fn hand_over(conn: PeerConnection, tx: &mpsc::UnboundedSender<Internal>) -> Result<(), ()> {
    let id = conn.peer();
    let (sender, mut receiver) = conn.split();
    let (msg_tx, msg_rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok(m) = receiver.recv().await {
            if msg_tx.send(m).is_err() {
                return;
            }
        }
    });
    tx.send(Internal::Connected(id, sender, msg_rx))
        .map_err(|_| ())
}

fn debug_on() -> bool {
    std::env::var_os("HOPWYN_DEBUG").is_some()
}

/// Wall-clock time of day, for comparing logs from two computers.
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
