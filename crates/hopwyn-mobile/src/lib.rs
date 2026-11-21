//! The shared core of Hopwyn's phone app.
//!
//! The phone is a **remote**: a trackpad and keyboard for the computers it
//! is paired with. It pairs the same way computers do (nothing to type),
//! links to each paired computer over the same encrypted QUIC transport,
//! and takes control of one with `Enter`; the computer then injects what
//! the phone sends, exactly as if another computer's mouse had crossed
//! over. It tells computers it is a remote, so they keep no screen edge
//! for it.
//!
//! The app talks to this crate through [`Mobile`]: commands in, events out
//! as text lines (see [`Event::to_line`]), which suits the JNI bridge.

mod jni_api;
pub mod keys;
#[path = "../../hopwyn-app/src/pairing_net.rs"]
#[allow(dead_code)]
mod pairing_net;

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hopwyn_discovery::{Discovery, DiscoveryEvent};
use hopwyn_pair::{DeviceId, Identity, PairedPeer};
use hopwyn_proto::{Button, DecodeError, Edge, Message};
use hopwyn_transport::{Node, PeerConnection, PeerSender, TransportError, TrustStore};
use tokio::sync::{mpsc, oneshot};

use crate::pairing_net::{PAIRING_PORT, PAIRING_WINDOW, PairNetError};

pub const LINK_PORT: u16 = 4242;
const RETRY: Duration = Duration::from_secs(2);
const DIAL_TIMEOUT: Duration = Duration::from_secs(4);

/// What the app asks for.
#[derive(Debug, Clone)]
pub enum Command {
    /// Open a pairing window; nearby computers join on their own.
    AddComputer,
    CancelPairing,
    /// The user's answer to "do the codes match?".
    Confirm(bool),
    /// Join a nearby computer's open pairing window.
    Join(DeviceId),
    /// Start driving this paired computer.
    Control(DeviceId),
    /// Stop driving; the computer gets its own mouse back.
    Release,
    /// Pointer movement in pixels (fractions are kept for the next move).
    Motion(f32, f32),
    Button(Button, bool),
    /// Scroll in lines.
    Scroll(f32, f32),
    /// One HID key.
    Key(u16, bool),
    /// Type this text, as a US keyboard would.
    Text(String),
    Forget(DeviceId),
    Stop,
}

/// What the app hears.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Ready {
        name: String,
        id: String,
    },
    /// Paired computers: id, name, connected.
    Partners(Vec<(DeviceId, String, bool)>),
    /// Nearby computers: id, name, paired, pairing window open.
    Nearby(Vec<(DeviceId, String, bool, bool)>),
    PairingOpen,
    PairingConfirm {
        name: String,
        code: String,
    },
    PairingDone(String),
    PairingFailed(String),
    PairingClosed,
    /// Driving this computer now, or none.
    Controlling(Option<DeviceId>),
    Error(String),
}

/// Unit and record separators: never in names (they are cleaned).
const UNIT: char = '\u{1f}';
const RECORD: char = '\u{1e}';

fn hex(id: &DeviceId) -> String {
    id.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

/// Parses a device key from 64 hex digits.
pub fn parse_id(s: &str) -> Option<DeviceId> {
    let s = s.trim();
    if s.len() != 64 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (slot, pair) in out.iter_mut().zip(s.as_bytes().as_chunks::<2>().0) {
        *slot = u8::from_str_radix(std::str::from_utf8(pair.as_slice()).ok()?, 16).ok()?;
    }
    Some(DeviceId::from_bytes(out))
}

fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

impl Event {
    /// One line for the app: `kind␟field␟field…` (U+001F between fields).
    /// Lists are `kind␟record␞record…` (U+001E between records), with a
    /// record's fields joined by U+001D. Names are cleaned of control
    /// characters, so the separators can't appear in them.
    pub fn to_line(&self) -> String {
        const FIELD: char = '\u{1d}';
        let b = |v: bool| if v { "1" } else { "0" };
        match self {
            Event::Ready { name, id } => format!("ready{UNIT}{}{UNIT}{id}", clean(name)),
            Event::Partners(list) => {
                let recs: Vec<String> = list
                    .iter()
                    .map(|(id, name, up)| {
                        format!("{}{FIELD}{}{FIELD}{}", hex(id), clean(name), b(*up))
                    })
                    .collect();
                format!("partners{UNIT}{}", recs.join(&RECORD.to_string()))
            }
            Event::Nearby(list) => {
                let recs: Vec<String> = list
                    .iter()
                    .map(|(id, name, paired, open)| {
                        format!(
                            "{}{FIELD}{}{FIELD}{}{FIELD}{}",
                            hex(id),
                            clean(name),
                            b(*paired),
                            b(*open)
                        )
                    })
                    .collect();
                format!("nearby{UNIT}{}", recs.join(&RECORD.to_string()))
            }
            Event::PairingOpen => "pairing-open".into(),
            Event::PairingConfirm { name, code } => {
                format!("pairing-confirm{UNIT}{}{UNIT}{code}", clean(name))
            }
            Event::PairingDone(name) => format!("pairing-done{UNIT}{}", clean(name)),
            Event::PairingFailed(why) => format!("pairing-failed{UNIT}{}", clean(why)),
            Event::PairingClosed => "pairing-closed".into(),
            Event::Controlling(id) => {
                format!(
                    "controlling{UNIT}{}",
                    id.as_ref().map(hex).unwrap_or_default()
                )
            }
            Event::Error(e) => format!("error{UNIT}{}", clean(e)),
        }
    }
}

/// A running core. Dropping it does not stop it; send [`Command::Stop`].
pub struct Mobile {
    cmd: mpsc::UnboundedSender<Command>,
    events: Mutex<std_mpsc::Receiver<Event>>,
}

/// Ports, overridable for tests (several cores on one machine).
#[derive(Debug, Clone, Copy)]
pub struct Ports {
    pub link: u16,
    pub pairing: u16,
}

impl Default for Ports {
    fn default() -> Self {
        Self {
            link: LINK_PORT,
            pairing: PAIRING_PORT,
        }
    }
}

impl Mobile {
    /// Starts the core, keeping its key and pairings in `dir`.
    pub fn start(dir: PathBuf, name: &str, ports: Ports) -> Result<Self, String> {
        let store = Store::open(dir)?;
        let identity = Arc::new(store.identity(name)?);
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (ev_tx, ev_rx) = std_mpsc::channel();
        let (ready_tx, ready_rx) = std_mpsc::channel();
        std::thread::Builder::new()
            .name("hopwyn-mobile".into())
            .spawn(move || {
                let rt = match tokio::runtime::Runtime::new() {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.to_string()));
                        return;
                    }
                };
                rt.block_on(async move {
                    match Core::new(store, identity, ports, ev_tx) {
                        Ok(core) => {
                            let _ = ready_tx.send(Ok(()));
                            core.run(cmd_rx).await;
                        }
                        Err(e) => {
                            let _ = ready_tx.send(Err(e));
                        }
                    }
                });
            })
            .map_err(|e| e.to_string())?;
        ready_rx.recv().map_err(|e| e.to_string())??;
        Ok(Self {
            cmd: cmd_tx,
            events: Mutex::new(ev_rx),
        })
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.cmd.send(cmd);
    }

    /// The next event, waiting up to `timeout`.
    pub fn next_event(&self, timeout: Duration) -> Option<Event> {
        self.events.lock().ok()?.recv_timeout(timeout).ok()
    }
}

/// The phone's key and pairings, in the app's private files.
struct Store {
    dir: PathBuf,
}

impl Store {
    fn open(dir: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        Ok(Self { dir })
    }

    fn identity(&self, name: &str) -> Result<Identity, String> {
        let path = self.dir.join("identity");
        match std::fs::read(&path) {
            Ok(bytes) => {
                let secret: [u8; 32] = bytes
                    .try_into()
                    .map_err(|_| "the stored key is corrupt".to_owned())?;
                Ok(Identity::from_secret_bytes(&secret, name))
            }
            Err(_) => {
                let id = Identity::generate(name);
                std::fs::write(&path, id.secret_bytes()).map_err(|e| e.to_string())?;
                Ok(id)
            }
        }
    }

    fn partners(&self) -> Vec<(DeviceId, String)> {
        let text = std::fs::read_to_string(self.dir.join("partners")).unwrap_or_default();
        text.lines()
            .filter_map(|l| {
                let (id, name) = l.split_once(' ')?;
                Some((parse_id(id)?, name.to_owned()))
            })
            .collect()
    }

    fn save_partners(&self, list: &[(DeviceId, String)]) {
        let text: String = list
            .iter()
            .map(|(id, name)| format!("{} {}\n", hex(id), clean(name)))
            .collect();
        let _ = std::fs::write(self.dir.join("partners"), text);
    }
}

#[derive(Debug, Clone)]
struct Seen {
    name: String,
    addrs: Vec<SocketAddr>,
    pairing: Option<String>,
    pairing_port: u16,
}

struct Pairing {
    task: tokio::task::JoinHandle<()>,
    answer: Option<oneshot::Sender<bool>>,
    auto: bool,
}

enum Internal {
    Connected(DeviceId, PeerSender, mpsc::UnboundedReceiver<Message>),
    Message(DeviceId, u64, Message),
    Gone(DeviceId, u64),
    Nearby(HashMap<DeviceId, Seen>),
    Compare {
        name: String,
        code: String,
        answer: oneshot::Sender<bool>,
    },
    Paired(Result<PairedPeer, PairNetError>),
}

struct Link {
    sender: PeerSender,
    generation: u64,
}

struct Core {
    store: Store,
    identity: Arc<Identity>,
    ports: Ports,
    events: std_mpsc::Sender<Event>,
    partners: Vec<(DeviceId, String)>,
    links: HashMap<DeviceId, Link>,
    connected: Arc<Mutex<HashSet<DeviceId>>>,
    partners_tx: tokio::sync::watch::Sender<Vec<DeviceId>>,
    trust: TrustStore,
    discovery: Discovery,
    nearby: HashMap<DeviceId, Seen>,
    pairing: Option<Pairing>,
    tried: HashSet<(DeviceId, String)>,
    controlling: Option<DeviceId>,
    carry: (f32, f32),
    tx: mpsc::UnboundedSender<Internal>,
    rx: mpsc::UnboundedReceiver<Internal>,
    next_generation: u64,
}

impl Core {
    fn new(
        store: Store,
        identity: Arc<Identity>,
        ports: Ports,
        events: std_mpsc::Sender<Event>,
    ) -> Result<Self, String> {
        let partners = store.partners();
        let trust = TrustStore::new();
        for (id, _) in &partners {
            trust.add(*id);
        }
        let node = Arc::new(
            Node::bind(
                &identity,
                SocketAddr::from(([0, 0, 0, 0], ports.link)),
                trust.clone(),
            )
            .map_err(|e| format!("could not open UDP port {}: {e}", ports.link))?,
        );
        let (discovery, found) =
            Discovery::start(&identity, ports.link).map_err(|e| e.to_string())?;
        let (tx, rx) = mpsc::unbounded_channel();
        let (partners_tx, partners_rx) =
            tokio::sync::watch::channel(partners.iter().map(|(id, _)| *id).collect::<Vec<_>>());
        let addresses: Arc<Mutex<HashMap<DeviceId, Vec<SocketAddr>>>> = Arc::default();
        let connected: Arc<Mutex<HashSet<DeviceId>>> = Arc::default();

        // Discovery.
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
                    return;
                }
            }
        });

        // Incoming links from paired computers.
        let acc_tx = tx.clone();
        let acc_node = node.clone();
        let acc_partners = partners_rx.clone();
        tokio::spawn(async move {
            while let Some(r) = acc_node.accept().await {
                match r {
                    Ok(conn) if acc_partners.borrow().contains(&conn.peer()) => {
                        if hand_over(conn, &acc_tx).is_err() {
                            return;
                        }
                    }
                    Ok(conn) => conn.close(),
                    Err(_) => {}
                }
            }
        });

        // Outgoing links: the higher key dials.
        let dial_tx = tx.clone();
        let me = identity.id();
        let mut dial_partners = partners_rx;
        let dial_connected = connected.clone();
        tokio::spawn(async move {
            loop {
                let targets: Vec<DeviceId> = dial_partners
                    .borrow()
                    .iter()
                    .copied()
                    .filter(|id| me.as_bytes() > id.as_bytes())
                    .collect();
                for id in targets {
                    if dial_connected
                        .lock()
                        .map(|c| c.contains(&id))
                        .unwrap_or(true)
                    {
                        continue;
                    }
                    let addrs = addresses
                        .lock()
                        .ok()
                        .and_then(|m| m.get(&id).cloned())
                        .unwrap_or_default();
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

        let _ = events.send(Event::Ready {
            name: identity.name().to_owned(),
            id: identity.id().to_string(),
        });
        Ok(Self {
            store,
            identity,
            ports,
            events,
            partners,
            links: HashMap::new(),
            connected,
            partners_tx,
            trust,
            discovery,
            nearby: HashMap::new(),
            pairing: None,
            tried: HashSet::new(),
            controlling: None,
            carry: (0.0, 0.0),
            tx,
            rx,
            next_generation: 1,
        })
    }

    fn emit(&self, ev: Event) {
        let _ = self.events.send(ev);
    }

    async fn run(mut self, mut cmds: mpsc::UnboundedReceiver<Command>) {
        self.publish_partners();
        loop {
            tokio::select! {
                cmd = cmds.recv() => match cmd {
                    None | Some(Command::Stop) => break,
                    Some(c) => self.on_command(c).await,
                },
                ev = self.rx.recv() => match ev {
                    None => break,
                    Some(ev) => self.on_internal(ev).await,
                },
            }
        }
        self.release().await;
        for link in self.links.values() {
            link.sender.close();
        }
    }

    fn publish_partners(&self) {
        let list = self
            .partners
            .iter()
            .map(|(id, name)| (*id, name.clone(), self.links.contains_key(id)))
            .collect();
        self.emit(Event::Partners(list));
    }

    fn publish_nearby(&self) {
        let mut list: Vec<(DeviceId, String, bool, bool)> = self
            .nearby
            .iter()
            .map(|(id, s)| {
                (
                    *id,
                    s.name.clone(),
                    self.partners.iter().any(|(p, _)| p == id),
                    s.pairing.is_some(),
                )
            })
            .collect();
        list.sort_by(|a, b| a.1.cmp(&b.1));
        self.emit(Event::Nearby(list));
    }

    async fn send_to(&mut self, id: DeviceId, msg: &Message) -> bool {
        match self.links.get_mut(&id) {
            Some(link) => link.sender.send(msg).await.is_ok(),
            None => false,
        }
    }

    async fn on_internal(&mut self, ev: Internal) {
        match ev {
            Internal::Connected(id, sender, mut rx) => {
                if !self.partners.iter().any(|(p, _)| *p == id) {
                    sender.close();
                    return;
                }
                let generation = self.next_generation;
                self.next_generation += 1;
                if let Ok(mut c) = self.connected.lock() {
                    c.insert(id);
                }
                if let Some(old) = self.links.insert(id, Link { sender, generation }) {
                    old.sender.close();
                }
                let fwd = self.tx.clone();
                tokio::spawn(async move {
                    while let Some(m) = rx.recv().await {
                        if fwd.send(Internal::Message(id, generation, m)).is_err() {
                            return;
                        }
                    }
                    let _ = fwd.send(Internal::Gone(id, generation));
                });
                // Tell it we're a remote, so it keeps no screen edge for us.
                self.send_to(id, &Message::Remote).await;
                self.publish_partners();
            }
            Internal::Message(id, generation, m) => {
                if self.links.get(&id).map(|l| l.generation) != Some(generation) {
                    return;
                }
                match m {
                    // A computer trying to drive the phone: not supported yet.
                    Message::Enter { .. } => {
                        self.send_to(id, &Message::Leave).await;
                    }
                    // It took its own mouse back (its panic key, or it gave up).
                    Message::Leave | Message::LeaveAt { .. } if self.controlling == Some(id) => {
                        self.controlling = None;
                        self.emit(Event::Controlling(None));
                    }
                    _ => {}
                }
            }
            Internal::Gone(id, generation) => {
                if self.links.get(&id).map(|l| l.generation) == Some(generation) {
                    self.links.remove(&id);
                    if let Ok(mut c) = self.connected.lock() {
                        c.remove(&id);
                    }
                    if self.controlling == Some(id) {
                        self.controlling = None;
                        self.emit(Event::Controlling(None));
                    }
                    self.publish_partners();
                }
            }
            Internal::Nearby(map) => {
                self.nearby = map;
                self.publish_nearby();
                self.join_open_windows();
            }
            Internal::Compare { name, code, answer } => {
                if let Some(p) = self.pairing.as_mut() {
                    p.answer = Some(answer);
                    p.auto = false;
                }
                let _ = self.discovery.set_pairing(None, 0);
                self.emit(Event::PairingConfirm { name, code });
            }
            Internal::Paired(result) => {
                let auto = self.pairing.take().is_some_and(|p| p.auto);
                let _ = self.discovery.set_pairing(None, 0);
                match result {
                    Ok(peer) => {
                        if !self.partners.iter().any(|(p, _)| *p == peer.id) {
                            self.partners.push((peer.id, peer.name.clone()));
                        }
                        self.trust.add(peer.id);
                        self.store.save_partners(&self.partners);
                        let _ = self
                            .partners_tx
                            .send(self.partners.iter().map(|(id, _)| *id).collect());
                        self.emit(Event::PairingDone(peer.name));
                        self.publish_partners();
                        self.publish_nearby();
                    }
                    Err(PairNetError::Cancelled) => self.emit(Event::PairingClosed),
                    Err(_) if auto => {}
                    Err(e) => self.emit(Event::PairingFailed(e.to_string())),
                }
            }
        }
    }

    async fn on_command(&mut self, cmd: Command) {
        match cmd {
            Command::AddComputer => self.start_hosting(),
            Command::CancelPairing => {
                if let Some(mut p) = self.pairing.take() {
                    match p.answer.take() {
                        Some(a) => {
                            let _ = a.send(false);
                            self.pairing = Some(p);
                        }
                        None => p.task.abort(),
                    }
                }
                let _ = self.discovery.set_pairing(None, 0);
            }
            Command::Confirm(yes) => {
                if let Some(a) = self.pairing.as_mut().and_then(|p| p.answer.take()) {
                    let _ = a.send(yes);
                }
            }
            Command::Join(id) => {
                if let Some(seen) = self.nearby.get(&id).cloned() {
                    self.start_guest(&seen, false);
                }
            }
            Command::Control(id) => {
                if self.controlling == Some(id) {
                    return;
                }
                self.release().await;
                // The computer places its cursor mid-edge and starts
                // injecting what we send.
                let enter = Message::Enter {
                    edge: Edge::Left,
                    position: 0.5,
                };
                if self.send_to(id, &enter).await {
                    self.controlling = Some(id);
                    self.carry = (0.0, 0.0);
                    self.emit(Event::Controlling(Some(id)));
                } else {
                    self.emit(Event::Error("that computer isn't connected".into()));
                }
            }
            Command::Release => self.release().await,
            Command::Motion(dx, dy) => {
                let (x, y) = (self.carry.0 + dx, self.carry.1 + dy);
                let (ix, iy) = (to_i32(x), to_i32(y));
                #[allow(clippy::cast_precision_loss)]
                {
                    self.carry = (x - ix as f32, y - iy as f32);
                }
                if ix != 0 || iy != 0 {
                    self.drive(&Message::PointerMotion { dx: ix, dy: iy }).await;
                }
            }
            Command::Button(button, pressed) => {
                self.drive(&Message::PointerButton { button, pressed })
                    .await;
            }
            Command::Scroll(dx, dy) => {
                if dx.is_finite() && dy.is_finite() {
                    self.drive(&Message::Scroll { dx, dy }).await;
                }
            }
            Command::Key(code, pressed) => {
                self.drive(&Message::Key { code, pressed }).await;
            }
            Command::Text(text) => {
                for c in text.chars() {
                    let Some((code, shift)) = keys::for_char(c) else {
                        continue;
                    };
                    if shift {
                        self.drive(&Message::Key {
                            code: keys::SHIFT,
                            pressed: true,
                        })
                        .await;
                    }
                    self.drive(&Message::Key {
                        code,
                        pressed: true,
                    })
                    .await;
                    self.drive(&Message::Key {
                        code,
                        pressed: false,
                    })
                    .await;
                    if shift {
                        self.drive(&Message::Key {
                            code: keys::SHIFT,
                            pressed: false,
                        })
                        .await;
                    }
                }
            }
            Command::Forget(id) => {
                if self.controlling == Some(id) {
                    self.release().await;
                }
                self.partners.retain(|(p, _)| *p != id);
                self.trust.remove(&id);
                if let Some(link) = self.links.remove(&id) {
                    link.sender.close();
                }
                if let Ok(mut c) = self.connected.lock() {
                    c.remove(&id);
                }
                self.store.save_partners(&self.partners);
                let _ = self
                    .partners_tx
                    .send(self.partners.iter().map(|(id, _)| *id).collect());
                self.publish_partners();
                self.publish_nearby();
            }
            Command::Stop => {}
        }
    }

    /// Sends input to the computer being driven, if any.
    async fn drive(&mut self, msg: &Message) {
        if let Some(id) = self.controlling
            && !self.send_to(id, msg).await
        {
            self.controlling = None;
            self.emit(Event::Controlling(None));
        }
    }

    async fn release(&mut self) {
        if let Some(id) = self.controlling.take() {
            self.send_to(id, &Message::Leave).await;
            self.emit(Event::Controlling(None));
        }
    }

    fn start_hosting(&mut self) {
        self.cancel_quietly();
        let token = format!("{:016x}", token_seed(&self.identity));
        if let Err(e) = self.discovery.set_pairing(Some(&token), self.ports.pairing) {
            self.emit(Event::PairingFailed(e.to_string()));
            return;
        }
        self.emit(Event::PairingOpen);
        let identity = self.identity.clone();
        let tx = self.tx.clone();
        let port = self.ports.pairing;
        let task = tokio::spawn(async move {
            let r = async {
                let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
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
            let _ = tx.send(Internal::Paired(r));
        });
        self.pairing = Some(Pairing {
            task,
            answer: None,
            auto: false,
        });
    }

    fn start_guest(&mut self, seen: &Seen, auto: bool) {
        self.cancel_quietly();
        let addrs: Vec<SocketAddr> = seen
            .addrs
            .iter()
            .map(|a| SocketAddr::new(a.ip(), seen.pairing_port))
            .collect();
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
            let _ = tx.send(Internal::Paired(r));
        });
        self.pairing = Some(Pairing {
            task,
            answer: None,
            auto,
        });
    }

    fn cancel_quietly(&mut self) {
        if let Some(p) = self.pairing.take() {
            p.task.abort();
        }
        let _ = self.discovery.set_pairing(None, 0);
    }

    /// Joins a computer's freshly opened pairing window, once per window,
    /// unless already paired with it and linked.
    fn join_open_windows(&mut self) {
        if self.pairing.is_some() {
            return;
        }
        let candidate = self.nearby.iter().find_map(|(id, s)| {
            let token = s.pairing.clone()?;
            let fresh = !self.tried.contains(&(*id, token.clone()));
            (fresh && !self.links.contains_key(id)).then(|| (*id, token, s.clone()))
        });
        if let Some((id, token, seen)) = candidate {
            self.tried.insert((id, token));
            self.start_guest(&seen, true);
        }
    }
}

fn token_seed(identity: &Identity) -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut x = u64::try_from(nanos & u128::from(u64::MAX)).unwrap_or(0);
    for b in identity.id().as_bytes() {
        x = x.rotate_left(5) ^ u64::from(*b);
    }
    x
}

#[allow(clippy::cast_possible_truncation)]
fn to_i32(v: f32) -> i32 {
    if v.is_finite() {
        v.trunc().clamp(-1_000_000.0, 1_000_000.0) as i32
    } else {
        0
    }
}

fn hand_over(conn: PeerConnection, tx: &mpsc::UnboundedSender<Internal>) -> Result<(), ()> {
    let id = conn.peer();
    let (sender, mut receiver) = conn.split();
    let (msg_tx, msg_rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            match receiver.recv().await {
                Ok(m) => {
                    if msg_tx.send(m).is_err() {
                        return;
                    }
                }
                Err(TransportError::Decode(DecodeError::UnknownTag(_))) => {}
                Err(_) => return,
            }
        }
    });
    tx.send(Internal::Connected(id, sender, msg_rx))
        .map_err(|_| ())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn events_become_lines_the_app_can_split() {
        let id = Identity::generate("Desk").id();
        let line = Event::Partners(vec![(id, "Desk\u{1f}PC".into(), true)]).to_line();
        assert!(line.starts_with("partners\u{1f}"));
        // A separator inside a name is removed, so fields stay aligned.
        assert_eq!(line.matches('\u{1f}').count(), 1);
        assert!(line.contains("DeskPC\u{1d}1"));
        assert_eq!(
            Event::PairingConfirm {
                name: "Desk".into(),
                code: "123456".into()
            }
            .to_line(),
            "pairing-confirm\u{1f}Desk\u{1f}123456"
        );
        assert_eq!(Event::Controlling(None).to_line(), "controlling\u{1f}");
    }

    #[test]
    fn device_keys_round_trip_through_hex() {
        let id = Identity::generate("x").id();
        assert_eq!(parse_id(&hex(&id)), Some(id));
        assert_eq!(parse_id("nope"), None);
    }
}
