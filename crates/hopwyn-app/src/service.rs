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
use hopwyn_input::capture::CaptureEvent;
use hopwyn_input::linux::LinuxEmulator;
use hopwyn_input::linux_capture::LinuxCapture;
use hopwyn_input::{InputError, InputEvent};
use hopwyn_pair::{DeviceId, Identity, PairedPeer};
use hopwyn_proto::{Edge, Message};
use hopwyn_transport::{Node, PeerConnection, PeerSender, TrustStore};
use tokio::sync::{mpsc, oneshot, watch};

use crate::config::{self, MAX_PARTNERS, Partner, Store};
use crate::pairing_net::{self, PAIRING_PORT, PAIRING_WINDOW, PairNetError};

const LINK_PORT: u16 = 4242;

/// Test mode: `HOPWYN_NO_INPUT=1` skips the desktop's input portals, so
/// several copies can run on one machine (each with its own
/// `XDG_CONFIG_HOME` and ports) to test discovery, pairing and the UI.
fn no_input() -> bool {
    std::env::var_os("HOPWYN_NO_INPUT").is_some()
}

fn env_port(name: &str, default: u16) -> u16 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|p| *p != 0)
        .unwrap_or(default)
}

fn link_port() -> u16 {
    env_port("HOPWYN_LINK_PORT", LINK_PORT)
}

fn pairing_port() -> u16 {
    env_port("HOPWYN_PAIRING_PORT", PAIRING_PORT)
}

/// Screen-edge capture, or nothing in test mode.
struct Capture(Option<LinuxCapture>);

impl Capture {
    fn release(&self) -> Result<(), InputError> {
        self.0.as_ref().map_or(Ok(()), LinuxCapture::release)
    }

    fn release_at(&self, position: f32) -> Result<(), InputError> {
        self.0.as_ref().map_or(Ok(()), |c| c.release_at(position))
    }

    fn enable(&self) -> Result<(), InputError> {
        self.0.as_ref().map_or(Ok(()), LinuxCapture::enable)
    }

    fn set_edges(&self, edges: Vec<Edge>) -> Result<(), InputError> {
        self.0.as_ref().map_or(Ok(()), |c| c.set_edges(edges))
    }
}
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
    /// Open the pairing window; nearby computers join on their own.
    AddDevice,
    CancelPairing,
    /// The user's answer to "do the codes match?" (true = Pair).
    ConfirmPairing(bool),
    /// Join a nearby computer that has its pairing window open.
    JoinDevice(DeviceId),
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
    pub id: DeviceId,
    pub name: String,
    pub ip: IpAddr,
    pub paired: bool,
    /// It has "Add computer" open.
    pub open: bool,
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
    /// "Add computer" is open, waiting for another computer to join.
    PairingOpen,
    /// Both sides reached the code comparison: show the code, ask the user.
    PairingConfirm {
        name: String,
        code: String,
    },
    /// The pairing window closed without pairing (cancelled here).
    PairingClosed,
    PairingDone(String),
    PairingFailed(String),
    Error(String),
    /// The service stopped; commands no longer work.
    Fatal(String),
}

/// How long to wait for the window (to put permission dialogs on top of)
/// before asking without one, as when Hopwyn starts hidden at login.
const PARENT_WAIT: Duration = Duration::from_secs(3);

/// Starts the service on its own thread. `parent` delivers the window the
/// desktop's permission dialogs should sit on (see `LinuxCapture`).
pub fn spawn(
    ui: async_channel::Sender<UiEvent>,
    parent: std::sync::mpsc::Receiver<Option<String>>,
) -> mpsc::UnboundedSender<Command> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name("hopwyn-service".into())
        .spawn(move || {
            let parent = parent.recv_timeout(PARENT_WAIT).ok().flatten();
            if debug_on() {
                eprintln!("[hopwyn {}] dialogs attach to window: {parent:?}", stamp());
            }
            if let Err(e) = run(&ui, rx, parent) {
                let _ = ui.send_blocking(UiEvent::Fatal(e));
            }
        })
        .ok();
    tx
}

fn run(
    ui: &async_channel::Sender<UiEvent>,
    cmds: mpsc::UnboundedReceiver<Command>,
    parent: Option<String>,
) -> Result<(), String> {
    let store = Store::open_default()?;
    let identity = Arc::new(store.identity(&config::device_name())?);
    let partners = store.partners();

    let (capture, capture_rx, token) = if no_input() {
        let (_tx, rx) = std::sync::mpsc::channel();
        (Capture(None), rx, None)
    } else {
        let (capture, rx, token) = start_input(ui, &store, &partners, parent)?;
        (Capture(Some(capture)), rx, token)
    };

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

/// Asks for the desktop's input permissions: capture, then remote control.
fn start_input(
    ui: &async_channel::Sender<UiEvent>,
    store: &Store,
    partners: &[Partner],
    parent: Option<String>,
) -> Result<
    (
        LinuxCapture,
        std::sync::mpsc::Receiver<CaptureEvent>,
        Option<String>,
    ),
    String,
> {
    // Capture first: on GNOME its dialog fails to appear if requested right
    // after a remote-control session closes.
    let _ = ui.send_blocking(UiEvent::Waiting(
        "Allow Hopwyn to capture input in the desktop's dialog".into(),
    ));
    let (capture, capture_rx) = LinuxCapture::start_with_token(
        capture_edges(partners),
        store.capture_token(),
        parent.clone(),
    )
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
        let emu = LinuxEmulator::connect_with_parent(None, parent).map_err(|e| e.to_string())?;
        token = emu.restore_token().map(str::to_owned);
        if let Some(t) = &token {
            let _ = store.save_restore_token(t);
        }
    }
    std::thread::sleep(SESSION_SETTLE);
    let _ = capture.enable();
    Ok((capture, capture_rx, token))
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
    Nearby(HashMap<DeviceId, Seen>),
    /// A pairing reached the code comparison; the sender takes the answer.
    Compare {
        name: String,
        code: String,
        answer: oneshot::Sender<bool>,
    },
    /// A pairing finished; `true` if this computer was the host.
    Paired(Result<PairedPeer, PairNetError>, bool),
}

/// A computer seen on the network: name, link addresses, pairing token.
#[derive(Debug, Clone)]
struct Seen {
    name: String,
    /// Every IPv4 address it announced (some may be unreachable, such as a
    /// virtual machine bridge), so each gets tried.
    addrs: Vec<SocketAddr>,
    pairing: Option<String>,
    pairing_port: u16,
}

/// A pairing in progress.
struct Pairing {
    task: tokio::task::JoinHandle<()>,
    /// Set while this computer hosts and no guest has joined yet.
    hosting: bool,
    /// Takes the user's answer once the codes are shown.
    answer: Option<oneshot::Sender<bool>>,
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
    capture: Capture,
    trust: TrustStore,
    tx: mpsc::UnboundedSender<Internal>,
    rx: mpsc::UnboundedReceiver<Internal>,
    discovery: Discovery,
    emulator: Option<LinuxEmulator>,
    nearby: HashMap<DeviceId, Seen>,
    pairing: Option<Pairing>,
    /// Pairing windows already tried as a guest, so each prompts once.
    tried: HashSet<(DeviceId, String)>,
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
        capture: Capture,
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
                SocketAddr::from(([0, 0, 0, 0], link_port())),
                trust.clone(),
            )
            .map_err(|e| format!("could not open UDP port {}: {e}", link_port()))?,
        );
        let (discovery, found) =
            Discovery::start(&identity, link_port()).map_err(|e| e.to_string())?;
        let (partners_tx, partners_rx) = watch::channel(partners.clone());
        let addresses: Arc<Mutex<HashMap<DeviceId, Vec<SocketAddr>>>> = Arc::default();
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
            let mut seen: HashMap<DeviceId, Seen> = HashMap::new();
            while let Ok(ev) = found.recv() {
                match ev {
                    DiscoveryEvent::Found(d) => {
                        let v4: Vec<SocketAddr> = d
                            .addrs
                            .iter()
                            .copied()
                            .filter(SocketAddr::is_ipv4)
                            .collect();
                        if !v4.is_empty() {
                            if let Ok(mut m) = disc_addrs.lock() {
                                m.insert(d.id, v4.clone());
                            }
                            seen.insert(
                                d.id,
                                Seen {
                                    name: d.name,
                                    addrs: v4,
                                    pairing: d.pairing,
                                    pairing_port: d.pairing_port.unwrap_or(PAIRING_PORT),
                                },
                            );
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
            // The address that last worked for each computer goes first.
            let mut good: HashMap<DeviceId, SocketAddr> = HashMap::new();
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
                    if up {
                        continue;
                    }
                    let mut addrs = addresses
                        .lock()
                        .ok()
                        .and_then(|m| m.get(&id).cloned())
                        .unwrap_or_default();
                    if let Some(g) = good.get(&id) {
                        addrs.sort_by_key(|a| a != g);
                    }
                    for addr in addrs {
                        let Ok(Ok(conn)) =
                            tokio::time::timeout(DIAL_TIMEOUT, node.connect(addr)).await
                        else {
                            continue;
                        };
                        if conn.peer() != id {
                            conn.close();
                            continue;
                        }
                        good.insert(id, addr);
                        if let Ok(mut c) = dial_connected.lock() {
                            c.insert(id);
                        }
                        if hand_over(conn, &dial_tx).is_err() {
                            return;
                        }
                        break;
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
            discovery,
            emulator: None,
            nearby: HashMap::new(),
            pairing: None,
            tried: HashSet::new(),
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
                if debug_on() {
                    for seen in map.values() {
                        eprintln!(
                            "[hopwyn {}] nearby: {} at {:?} pairing={:?}",
                            stamp(),
                            seen.name,
                            seen.addrs,
                            seen.pairing
                        );
                    }
                }
                self.nearby = map;
                self.publish_nearby();
                self.join_open_windows();
            }
            Internal::Compare { name, code, answer } => {
                if let Some(p) = self.pairing.as_mut() {
                    p.hosting = false;
                    p.answer = Some(answer);
                }
                // The window is no longer needed: one guest at a time.
                let _ = self.discovery.set_pairing(None, 0);
                self.send_ui(UiEvent::PairingConfirm { name, code });
            }
            Internal::Paired(result, host) => {
                self.pairing = None;
                let _ = self.discovery.set_pairing(None, 0);
                match result {
                    Ok(peer) => self.adopt_partner(peer, host),
                    Err(PairNetError::Cancelled) => self.send_ui(UiEvent::PairingClosed),
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
                Action::PushToEdge => {
                    self.push_to_edge(edge);
                    if let Some(p) = self.peers.get_mut(i) {
                        p.engine.handle(Input::Pushed, Instant::now());
                    }
                }
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
            Command::AddDevice => self.start_hosting(),
            Command::CancelPairing => self.cancel_pairing(),
            Command::ConfirmPairing(yes) => {
                if let Some(answer) = self.pairing.as_mut().and_then(|p| p.answer.take()) {
                    let _ = answer.send(yes);
                }
            }
            Command::JoinDevice(id) => {
                if let Some(seen) = self.nearby.get(&id).cloned() {
                    if let Some(t) = &seen.pairing {
                        self.tried.insert((id, t.clone()));
                    }
                    self.start_guest(id, &seen);
                }
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

    /// "Add computer": listen for a guest and announce the open window.
    fn start_hosting(&mut self) {
        if self.peers.len() >= MAX_PARTNERS {
            self.send_ui(UiEvent::PairingFailed(full_message()));
            return;
        }
        self.cancel_pairing();
        let token = pairing_token(&self.identity);
        if let Err(e) = self.discovery.set_pairing(Some(&token), pairing_port()) {
            self.send_ui(UiEvent::PairingFailed(e.to_string()));
            return;
        }
        self.send_ui(UiEvent::PairingOpen);
        let identity = self.identity.clone();
        let tx = self.tx.clone();
        let task = tokio::spawn(async move {
            let r = async {
                let listener = tokio::net::TcpListener::bind(("0.0.0.0", pairing_port()))
                    .await
                    .map_err(|e| PairNetError::Io(e.to_string()))?;
                let pending = tokio::time::timeout(
                    PAIRING_WINDOW,
                    pairing_net::host_accept(&listener, &identity),
                )
                .await
                .map_err(|_| PairNetError::Timeout)??;
                drop(listener);
                let (answer, local) = oneshot::channel();
                let _ = tx.send(Internal::Compare {
                    name: pending.compared.peer().name.clone(),
                    code: pending.compared.code().to_owned(),
                    answer,
                });
                pairing_net::finish(pending, local).await
            }
            .await;
            let _ = tx.send(Internal::Paired(r, true));
        });
        self.pairing = Some(Pairing {
            task,
            hosting: true,
            answer: None,
        });
    }

    /// Joins a nearby computer's open pairing window.
    fn start_guest(&mut self, id: DeviceId, seen: &Seen) {
        if self.peers.len() >= MAX_PARTNERS && self.peer_index(&id).is_none() {
            self.send_ui(UiEvent::PairingFailed(full_message()));
            return;
        }
        self.cancel_pairing();
        let _ = self.discovery.set_pairing(None, 0);
        let addrs: Vec<SocketAddr> = seen
            .addrs
            .iter()
            .map(|a| SocketAddr::new(a.ip(), seen.pairing_port))
            .collect();
        self.debug(&format!(
            "joining {}'s pairing window at {addrs:?}",
            seen.name
        ));
        let identity = self.identity.clone();
        let tx = self.tx.clone();
        let task = tokio::spawn(async move {
            let r = async {
                let pending = pairing_net::guest_connect_any(&addrs, &identity).await?;
                let (answer, local) = oneshot::channel();
                let _ = tx.send(Internal::Compare {
                    name: pending.compared.peer().name.clone(),
                    code: pending.compared.code().to_owned(),
                    answer,
                });
                pairing_net::finish(pending, local).await
            }
            .await;
            let _ = tx.send(Internal::Paired(r, false));
        });
        self.pairing = Some(Pairing {
            task,
            hosting: false,
            answer: None,
        });
    }

    /// Ends any pairing in progress. Codes already shown get a Cancel sent
    /// to the other side.
    fn cancel_pairing(&mut self) {
        if let Some(mut p) = self.pairing.take() {
            match p.answer.take() {
                // finish() sends the cancel and reports back.
                Some(answer) => {
                    let _ = answer.send(false);
                    self.pairing = Some(p);
                    return;
                }
                None => p.task.abort(),
            }
        }
        let _ = self.discovery.set_pairing(None, 0);
    }

    /// Joins, on its own, a nearby computer that just opened "Add computer",
    /// once per window, unless the two are already paired and linked. If
    /// both opened it at once, the lower key joins.
    fn join_open_windows(&mut self) {
        let me = self.identity.id();
        let candidate = self.nearby.iter().find_map(|(id, seen)| {
            let token = seen.pairing.clone()?;
            let fresh = !self.tried.contains(&(*id, token.clone()));
            // Join unless already paired and linked; a computer that forgot
            // this one can't link, so it gets to pair again.
            let linked = self
                .peer_index(id)
                .and_then(|i| self.peers.get(i))
                .is_some_and(|p| p.sender.is_some());
            let room = self.peers.len() < MAX_PARTNERS || self.peer_index(id).is_some();
            (fresh && !linked && room).then(|| (*id, token, seen.clone()))
        });
        let Some((id, token, seen)) = candidate else {
            return;
        };
        match &self.pairing {
            None => {}
            Some(p) if p.hosting && me.as_bytes() < id.as_bytes() => {}
            Some(_) => return,
        }
        self.tried.insert((id, token));
        self.start_guest(id, &seen);
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
        if self.emulator.is_none() && self.capture.0.is_some() {
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
            .filter_map(|(id, seen)| {
                Some(Nearby {
                    id: *id,
                    name: seen.name.clone(),
                    ip: seen.addrs.first()?.ip(),
                    paired: self.peer_index(id).is_some(),
                    open: seen.pairing.is_some(),
                })
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

/// A new token for each pairing window (not secret: it only lets other
/// computers prompt once per window).
fn pairing_token(identity: &Identity) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut x = u64::try_from(nanos & u128::from(u64::MAX)).unwrap_or(0);
    for b in identity.id().as_bytes() {
        x = x.rotate_left(5) ^ u64::from(*b);
    }
    format!("{x:016x}")
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
