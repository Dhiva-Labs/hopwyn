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
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hopwyn_discovery::{Discovery, DiscoveryEvent};
use hopwyn_pair::{DeviceId, Identity, PairedPeer};
use hopwyn_proto::{Button, DecodeError, Edge, Message};
use hopwyn_transport::{
    Node, PeerConnection, PeerSender, TransportError, TrustStore, is_home_address,
};
use tokio::sync::{mpsc, oneshot};

use crate::pairing_net::{PAIRING_PORT, PAIRING_WINDOW, PairNetError};

pub const LINK_PORT: u16 = 4242;
/// The first wait between attempts to reach a paired computer. It doubles
/// while every attempt fails, up to [`RETRY_MAX`], so a phone whose
/// computers are switched off stays quiet.
const RETRY: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(30);
/// With every paired computer linked, the dial loop only wakes for changes.
const SETTLED: Duration = Duration::from_secs(300);
const DIAL_TIMEOUT: Duration = Duration::from_secs(4);
/// Normally the device with the higher key dials. The other one starts
/// dialing too after this long without a link, for networks where only one
/// side knows the other's address.
const FALLBACK: Duration = Duration::from_secs(6);
/// A link dropped this soon was refused rather than lost.
const DROPPED_WITHIN: Duration = Duration::from_secs(1);
/// This many refusals in a row and the user is told.
const DROPS_TO_TELL: u8 = 3;
/// How often to try a computer that keeps refusing.
const DROPPING_RETRY: Duration = Duration::from_secs(300);
/// Addresses remembered per computer, newest first.
const MAX_KNOWN: usize = 4;

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
    /// Join the pairing window of the computer at this address, typed by
    /// the user because the computer doesn't show up on its own.
    JoinAddress(String),
    /// Where to reach a paired computer that isn't found on its own.
    SetAddress(DeviceId, String),
    /// The phone joined, left or changed network.
    NetworkChanged,
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
    /// Whether computers may drive this phone (its Accessibility service is
    /// on), and its screen size in pixels.
    SetControllable {
        enabled: bool,
        width: u32,
        height: u32,
    },
    Stop,
}

/// What the phone's Accessibility service must do while a computer drives
/// the phone. Coordinates are screen pixels.
#[derive(Debug, Clone, PartialEq)]
pub enum Control {
    /// Show the pointer here (a computer just took over).
    Show {
        x: f32,
        y: f32,
    },
    Move {
        x: f32,
        y: f32,
    },
    /// Hide the pointer (control went back to the computer).
    Hide,
    Tap {
        x: f32,
        y: f32,
    },
    /// A drag: from, to, how long.
    Swipe {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        ms: u32,
    },
    Back,
    Home,
    /// Type into the focused text field.
    Text(String),
    Enter,
    Backspace,
}

impl Control {
    /// One line for the app, like [`Event::to_line`].
    pub fn to_line(&self) -> String {
        let n = |v: f32| format!("{v:.1}");
        match self {
            Control::Show { x, y } => format!("show{UNIT}{}{UNIT}{}", n(*x), n(*y)),
            Control::Move { x, y } => format!("move{UNIT}{}{UNIT}{}", n(*x), n(*y)),
            Control::Hide => "hide".into(),
            Control::Tap { x, y } => format!("tap{UNIT}{}{UNIT}{}", n(*x), n(*y)),
            Control::Swipe { x1, y1, x2, y2, ms } => format!(
                "swipe{UNIT}{}{UNIT}{}{UNIT}{}{UNIT}{}{UNIT}{ms}",
                n(*x1),
                n(*y1),
                n(*x2),
                n(*y2)
            ),
            Control::Back => "back".into(),
            Control::Home => "home".into(),
            Control::Text(t) => format!("text{UNIT}{}", clean(t)),
            Control::Enter => "enter".into(),
            Control::Backspace => "backspace".into(),
        }
    }
}

/// Phone pixels per computer pixel of pointer motion (phone screens are
/// denser, so the pointer moves a little further).
const DRIVEN_SPEED: f32 = 1.3;
/// Phone pixels swiped per scrolled line.
const SWIPE_PER_LINE: f32 = 90.0;
/// How far a held click must move to be a drag rather than a tap.
const DRAG_SLOP: f32 = 12.0;

/// A computer driving this phone.
#[derive(Debug, Clone, Copy)]
struct Driven {
    by: DeviceId,
    /// The edge the pointer came in over; leaving over it goes back.
    edge: Edge,
    x: f32,
    y: f32,
    /// Where the left button went down, while it is held.
    pressed_at: Option<(f32, f32)>,
    shift: bool,
}

/// What the app hears.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Ready {
        name: String,
        id: String,
    },
    /// Paired computers: id, name, connected, seen on this network, the
    /// address last used to reach it (or empty), and whether it keeps
    /// dropping the link (it has most likely forgotten this phone).
    Partners(Vec<(DeviceId, String, bool, bool, String, bool)>),
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
                    .map(|(id, name, up, seen, addr, dropping)| {
                        format!(
                            "{}{FIELD}{}{FIELD}{}{FIELD}{}{FIELD}{}{FIELD}{}",
                            hex(id),
                            clean(name),
                            b(*up),
                            b(*seen),
                            clean(addr),
                            b(*dropping)
                        )
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
    controls: Mutex<std_mpsc::Receiver<Control>>,
}

/// Ports, overridable for tests (several cores on one machine).
#[derive(Debug, Clone, Copy)]
pub struct Ports {
    pub link: u16,
    pub pairing: u16,
    /// Stand in for a computer (tests and demos): announce as one, and
    /// treat phones as devices to pair with.
    pub computer: bool,
}

impl Default for Ports {
    fn default() -> Self {
        Self {
            link: LINK_PORT,
            pairing: PAIRING_PORT,
            computer: false,
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
        let (ctl_tx, ctl_rx) = std_mpsc::channel();
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
                    match Core::new(store, identity, ports, ev_tx, ctl_tx) {
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
            controls: Mutex::new(ctl_rx),
        })
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.cmd.send(cmd);
    }

    /// The next event, waiting up to `timeout`.
    pub fn next_event(&self, timeout: Duration) -> Option<Event> {
        self.events.lock().ok()?.recv_timeout(timeout).ok()
    }

    /// The next thing to do on screen while a computer drives the phone
    /// (for the Accessibility service), waiting up to `timeout`.
    pub fn next_control(&self, timeout: Duration) -> Option<Control> {
        self.controls.lock().ok()?.recv_timeout(timeout).ok()
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

    /// Where each paired computer was last reached, newest first.
    fn addresses(&self) -> HashMap<DeviceId, Vec<SocketAddr>> {
        let text = std::fs::read_to_string(self.dir.join("addresses")).unwrap_or_default();
        text.lines()
            .filter_map(|l| {
                let mut parts = l.split(' ');
                let id = parse_id(parts.next()?)?;
                let addrs: Vec<SocketAddr> = parts
                    .filter_map(|a| a.parse().ok())
                    .filter(|a: &SocketAddr| is_home_address(a.ip()))
                    .take(MAX_KNOWN)
                    .collect();
                (!addrs.is_empty()).then_some((id, addrs))
            })
            .collect()
    }

    fn save_addresses(&self, map: &HashMap<DeviceId, Vec<SocketAddr>>) {
        let text: String = map
            .iter()
            .map(|(id, addrs)| {
                let list: Vec<String> = addrs.iter().map(ToString::to_string).collect();
                format!("{} {}\n", hex(id), list.join(" "))
            })
            .collect();
        let _ = std::fs::write(self.dir.join("addresses"), text);
    }
}

/// Reads what the user typed for a computer's address: an IPv4 address,
/// with or without a port. Only home-network addresses are accepted.
pub fn parse_address(text: &str, default_port: u16) -> Result<SocketAddr, String> {
    let t = text.trim();
    let addr = match t.parse::<SocketAddr>() {
        Ok(a) => a,
        Err(_) => SocketAddr::new(
            t.parse::<IpAddr>()
                .map_err(|_| "that isn't an address. It looks like 192.168.1.20".to_owned())?,
            default_port,
        ),
    };
    if !is_home_address(addr.ip()) {
        return Err("Hopwyn only connects to addresses on your own network".into());
    }
    Ok(addr)
}

/// What the dial loop and the core both need to know about reaching
/// paired computers.
#[derive(Default)]
struct Reach {
    /// Addresses announced on this network.
    found: HashMap<DeviceId, Vec<SocketAddr>>,
    /// Addresses that worked before, or that the user typed.
    known: HashMap<DeviceId, Vec<SocketAddr>>,
    connected: HashSet<DeviceId>,
    /// Computers that drop every link at once, tried only now and then.
    dropping: HashSet<DeviceId>,
    /// When each computer was last linked (or the core started).
    unlinked: HashMap<DeviceId, Instant>,
}

impl Reach {
    /// The addresses to try for `id`, or none if it's linked or not yet
    /// this side's turn to dial.
    fn candidates(&self, id: &DeviceId, higher: bool, started: Instant) -> Vec<SocketAddr> {
        if self.connected.contains(id) {
            return Vec::new();
        }
        let waited = self.unlinked.get(id).copied().unwrap_or(started).elapsed();
        if !higher && waited < FALLBACK {
            return Vec::new();
        }
        let mut out: Vec<SocketAddr> = Vec::new();
        for a in self
            .found
            .get(id)
            .into_iter()
            .chain(self.known.get(id))
            .flatten()
        {
            if !out.contains(a) {
                out.push(*a);
            }
        }
        out
    }

    fn remember(&mut self, id: DeviceId, addr: SocketAddr) -> bool {
        let list = self.known.entry(id).or_default();
        if list.first() == Some(&addr) {
            return false;
        }
        list.retain(|a| *a != addr);
        list.insert(0, addr);
        list.truncate(MAX_KNOWN);
        true
    }
}

/// Paces redialing one computer. A link normally lasts, and when it drops
/// it is redialed after a short gap. One that dies soon after coming up
/// (the other end refuses it, say) would otherwise be redialed in a tight
/// loop, so the gap doubles each time that happens.
struct Pace {
    dialed_at: Instant,
    gap: Duration,
    /// Whether the last link's lifetime has been counted.
    judged: bool,
}

impl Pace {
    /// A link that lasted this long was a real one.
    const LASTED: Duration = Duration::from_secs(20);

    fn new() -> Self {
        Self {
            dialed_at: Instant::now(),
            gap: RETRY,
            judged: true,
        }
    }

    /// A dial just succeeded.
    fn dialed(&mut self) {
        self.dialed_at = Instant::now();
        self.judged = false;
    }

    /// The link is down: how long to wait before dialing again, if at all.
    /// `rarely`: it drops every link, so only check now and then whether
    /// it has been paired again.
    fn wait(&mut self, rarely: bool) -> Option<Duration> {
        let lived = self.dialed_at.elapsed();
        if rarely {
            self.judged = true;
            let left = DROPPING_RETRY.saturating_sub(lived);
            return (!left.is_zero()).then_some(left);
        }
        if !self.judged {
            self.judged = true;
            self.gap = if lived < Self::LASTED {
                (self.gap * 2).min(RETRY_MAX)
            } else {
                RETRY
            };
        }
        let left = self.gap.saturating_sub(lived);
        (!left.is_zero()).then_some(left)
    }
}

/// Two links to one computer can come up at once when both sides dial.
/// Both sides keep the one dialed by the higher key, so they agree.
/// Returns whether the new link should replace the old one.
fn replaces(me_higher: bool, old_dialed: bool, new_dialed: bool) -> bool {
    !(old_dialed == me_higher && new_dialed != me_higher)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    name: String,
    addrs: Vec<SocketAddr>,
    pairing: Option<String>,
    pairing_port: u16,
    phone: bool,
}

struct Pairing {
    task: tokio::task::JoinHandle<()>,
    answer: Option<oneshot::Sender<bool>>,
    auto: bool,
    /// The address the user typed, kept for linking once paired.
    typed: Option<IpAddr>,
}

/// A new link: who, whether this side dialed it, and the other end's
/// address.
struct NewLink {
    id: DeviceId,
    dialed: bool,
    addr: SocketAddr,
    sender: PeerSender,
    rx: mpsc::UnboundedReceiver<Message>,
}

enum Internal {
    Connected(NewLink),
    Message(DeviceId, u64, Message),
    Gone(DeviceId, u64),
    /// What discovery sees, tagged with which discovery run saw it.
    Nearby(u64, HashMap<DeviceId, Seen>),
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
    dialed: bool,
    since: Instant,
}

struct Core {
    store: Store,
    identity: Arc<Identity>,
    ports: Ports,
    events: std_mpsc::Sender<Event>,
    controls: std_mpsc::Sender<Control>,
    /// The screen size, while computers may drive this phone.
    controllable: Option<(f32, f32)>,
    driven: Option<Driven>,
    partners: Vec<(DeviceId, String)>,
    links: HashMap<DeviceId, Link>,
    /// Links in a row that each computer dropped at once without a word.
    dropped: HashMap<DeviceId, u8>,
    reach: Arc<Mutex<Reach>>,
    /// Wakes the dial loop to try again now.
    redial: Arc<tokio::sync::Notify>,
    /// Counts discovery restarts, so a stopped run's last words are ignored.
    discovery_run: u64,
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
        controls: std_mpsc::Sender<Control>,
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
        let (discovery, found) = discover(&identity, ports).map_err(|e| e.to_string())?;
        let (tx, rx) = mpsc::unbounded_channel();
        let (partners_tx, partners_rx) =
            tokio::sync::watch::channel(partners.iter().map(|(id, _)| *id).collect::<Vec<_>>());
        let reach = Arc::new(Mutex::new(Reach {
            known: store.addresses(),
            ..Reach::default()
        }));
        let redial = Arc::new(tokio::sync::Notify::new());
        watch_discovery(found, 0, tx.clone());

        // Incoming links from paired computers.
        let acc_tx = tx.clone();
        let acc_node = node.clone();
        let acc_partners = partners_rx.clone();
        tokio::spawn(async move {
            while let Some(r) = acc_node.accept().await {
                match r {
                    Ok(conn) if acc_partners.borrow().contains(&conn.peer()) => {
                        if hand_over(conn, false, &acc_tx).is_err() {
                            return;
                        }
                    }
                    Ok(conn) => conn.close(),
                    Err(_) => {}
                }
            }
        });

        // Outgoing links. The higher key dials at once; the lower one
        // joins in after FALLBACK without a link.
        let dial_tx = tx.clone();
        let me = identity.id();
        let mut dial_partners = partners_rx;
        let dial_reach = reach.clone();
        let dial_wake = redial.clone();
        tokio::spawn(async move {
            let started = Instant::now();
            let mut wait = RETRY;
            let mut pace: HashMap<DeviceId, Pace> = HashMap::new();
            loop {
                let targets: Vec<DeviceId> = dial_partners.borrow().clone();
                let mut missing = false;
                // When a paced computer may next be dialed.
                let mut soonest: Option<Duration> = None;
                for id in targets {
                    let higher = me.as_bytes() > id.as_bytes();
                    let (linked, addrs, rarely) = match dial_reach.lock() {
                        Ok(r) => (
                            r.connected.contains(&id),
                            r.candidates(&id, higher, started),
                            r.dropping.contains(&id),
                        ),
                        Err(_) => return,
                    };
                    if linked {
                        continue;
                    }
                    missing = true;
                    // A link that keeps dying is redialed ever more slowly.
                    if let Some(left) = pace.get_mut(&id).and_then(|p| p.wait(rarely)) {
                        soonest = Some(soonest.map_or(left, |s: Duration| s.min(left)));
                        continue;
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
                        if let Ok(mut r) = dial_reach.lock() {
                            r.connected.insert(id);
                        }
                        pace.entry(id).or_insert_with(Pace::new).dialed();
                        if hand_over(conn, true, &dial_tx).is_err() {
                            return;
                        }
                        wait = RETRY;
                        break;
                    }
                }
                let pause = if missing { wait } else { SETTLED };
                tokio::select! {
                    () = tokio::time::sleep(pause) => {
                        if missing {
                            wait = (wait * 2).min(RETRY_MAX);
                        }
                    }
                    r = dial_partners.changed() => {
                        if r.is_err() {
                            return;
                        }
                        wait = RETRY;
                    }
                    () = dial_wake.notified() => wait = RETRY,
                }
                // Woken early, but someone is paced: don't spin until then.
                if let Some(left) = soonest {
                    tokio::time::sleep(left.min(pause)).await;
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
            controls,
            controllable: None,
            driven: None,
            partners,
            links: HashMap::new(),
            dropped: HashMap::new(),
            reach,
            redial,
            discovery_run: 0,
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
        let known = self
            .reach
            .lock()
            .map(|r| r.known.clone())
            .unwrap_or_default();
        let list = self
            .partners
            .iter()
            .map(|(id, name)| {
                let addr = known
                    .get(id)
                    .and_then(|l| l.first())
                    .map(|a| a.ip().to_string())
                    .unwrap_or_default();
                let dropping = self.dropped.get(id).is_some_and(|n| *n >= DROPS_TO_TELL);
                (
                    *id,
                    name.clone(),
                    // A link about to be dropped again isn't a connection.
                    self.links.contains_key(id) && !dropping,
                    self.nearby.contains_key(id),
                    addr,
                    dropping,
                )
            })
            .collect();
        self.emit(Event::Partners(list));
    }

    fn publish_nearby(&self) {
        let mut list: Vec<(DeviceId, String, bool, bool)> = self
            .nearby
            .iter()
            .filter(|(_, s)| self.ports.computer || !s.phone)
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
            Internal::Connected(link) => {
                let NewLink {
                    id,
                    dialed,
                    addr,
                    sender,
                    mut rx,
                } = link;
                if !self.partners.iter().any(|(p, _)| *p == id) {
                    sender.close();
                    return;
                }
                let me_higher = self.identity.id().as_bytes() > id.as_bytes();
                if let Some(old) = self.links.get(&id)
                    && !replaces(me_higher, old.dialed, dialed)
                {
                    sender.close();
                    return;
                }
                let generation = self.next_generation;
                self.next_generation += 1;
                let remembered = self.reach.lock().is_ok_and(|mut r| {
                    r.connected.insert(id);
                    r.remember(id, addr)
                });
                if remembered {
                    self.save_addresses();
                }
                let link = Link {
                    sender,
                    generation,
                    dialed,
                    since: Instant::now(),
                };
                if let Some(old) = self.links.insert(id, link) {
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
                // Tell it what we are: a screen it can drive, or a remote.
                let hello = self.hello();
                self.send_to(id, &hello).await;
                self.publish_partners();
            }
            Internal::Message(id, generation, m) => {
                if self.links.get(&id).map(|l| l.generation) != Some(generation) {
                    return;
                }
                match m {
                    Message::Enter { edge, position } => {
                        self.driven_enter(id, edge, position).await
                    }
                    _ if self.driven.is_some_and(|d| d.by == id) => self.driven_input(m).await,
                    // It took its own mouse back (its panic key, or it gave up).
                    Message::Leave | Message::LeaveAt { .. } if self.controlling == Some(id) => {
                        self.controlling = None;
                        self.emit(Event::Controlling(None));
                    }
                    _ => {}
                }
            }
            Internal::Gone(id, generation) => {
                if self.driven.is_some_and(|d| d.by == id) {
                    self.driven = None;
                    self.control(Control::Hide);
                }
                if self.links.get(&id).map(|l| l.generation) == Some(generation) {
                    // A computer that won't keep the link (it forgot this
                    // phone, or runs a version that can't) accepts it, then
                    // drops it at once. Anything else is an ordinary lost
                    // link.
                    let refused = self
                        .links
                        .remove(&id)
                        .is_some_and(|l| l.since.elapsed() < DROPPED_WITHIN);
                    if refused {
                        let n = self.dropped.entry(id).or_insert(0);
                        *n = n.saturating_add(1);
                        if *n >= DROPS_TO_TELL
                            && let Ok(mut r) = self.reach.lock()
                        {
                            r.dropping.insert(id);
                        }
                    } else if self.dropped.remove(&id).is_some()
                        && let Ok(mut r) = self.reach.lock()
                    {
                        r.dropping.remove(&id);
                    }
                    self.unlinked(id);
                    if self.controlling == Some(id) {
                        self.controlling = None;
                        self.emit(Event::Controlling(None));
                    }
                    self.publish_partners();
                }
            }
            Internal::Nearby(run, map) => {
                // Announcements repeat; only a real change is worth acting on.
                if run != self.discovery_run || map == self.nearby {
                    return;
                }
                if let Ok(mut r) = self.reach.lock() {
                    r.found = map.iter().map(|(id, s)| (*id, s.addrs.clone())).collect();
                }
                let partner_seen = |m: &HashMap<DeviceId, Seen>| -> Vec<bool> {
                    self.partners
                        .iter()
                        .map(|(id, _)| m.contains_key(id))
                        .collect()
                };
                let partners_moved = partner_seen(&map) != partner_seen(&self.nearby);
                self.nearby = map;
                self.redial.notify_one();
                self.publish_nearby();
                if partners_moved {
                    self.publish_partners();
                }
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
                let done = self.pairing.take();
                let auto = done.as_ref().is_some_and(|p| p.auto);
                let typed = done.and_then(|p| p.typed);
                let _ = self.discovery.set_pairing(None, 0);
                match result {
                    Ok(peer) => {
                        if !self.partners.iter().any(|(p, _)| *p == peer.id) {
                            self.partners.push((peer.id, peer.name.clone()));
                        }
                        match typed {
                            Some(ip) => self.set_address(peer.id, SocketAddr::new(ip, LINK_PORT)),
                            // The usual side dials first; this one only
                            // joins in if that gets nowhere.
                            None => {
                                if let Ok(mut r) = self.reach.lock() {
                                    r.unlinked.insert(peer.id, Instant::now());
                                }
                            }
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
                    self.start_guest(&seen, false, None);
                }
            }
            Command::JoinAddress(text) => match parse_address(&text, PAIRING_PORT) {
                Ok(addr) => {
                    let seen = Seen {
                        name: String::new(),
                        addrs: vec![addr],
                        pairing: None,
                        pairing_port: addr.port(),
                        phone: false,
                    };
                    self.start_guest(&seen, false, Some(addr.ip()));
                }
                Err(e) => self.emit(Event::PairingFailed(e)),
            },
            Command::SetAddress(id, text) => match parse_address(&text, LINK_PORT) {
                Ok(addr) if self.partners.iter().any(|(p, _)| *p == id) => {
                    self.set_address(id, addr);
                    self.publish_partners();
                }
                Ok(_) => {}
                Err(e) => self.emit(Event::Error(e)),
            },
            Command::NetworkChanged => self.network_changed().await,
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
                self.dropped.remove(&id);
                if let Some(link) = self.links.remove(&id) {
                    link.sender.close();
                }
                if let Ok(mut r) = self.reach.lock() {
                    r.connected.remove(&id);
                    r.known.remove(&id);
                    r.unlinked.remove(&id);
                    r.dropping.remove(&id);
                }
                self.save_addresses();
                self.store.save_partners(&self.partners);
                let _ = self
                    .partners_tx
                    .send(self.partners.iter().map(|(id, _)| *id).collect());
                self.publish_partners();
                self.publish_nearby();
            }
            Command::SetControllable {
                enabled,
                width,
                height,
            } => {
                #[allow(clippy::cast_precision_loss)]
                let size =
                    (enabled && width > 0 && height > 0).then_some((width as f32, height as f32));
                if size == self.controllable {
                    return;
                }
                self.controllable = size;
                if size.is_none()
                    && let Some(d) = self.driven.take()
                {
                    self.control(Control::Hide);
                    self.send_to(d.by, &Message::Leave).await;
                }
                let hello = self.hello();
                let ids: Vec<DeviceId> = self.links.keys().copied().collect();
                for id in ids {
                    self.send_to(id, &hello).await;
                }
            }
            Command::Stop => {}
        }
    }

    fn control(&self, c: Control) {
        let _ = self.controls.send(c);
    }

    fn save_addresses(&self) {
        if let Ok(r) = self.reach.lock() {
            self.store.save_addresses(&r.known);
        }
    }

    /// Notes that `id` has no link now, and has the dial loop try at once.
    fn unlinked(&self, id: DeviceId) {
        if let Ok(mut r) = self.reach.lock() {
            r.connected.remove(&id);
            r.unlinked.insert(id, Instant::now());
        }
        self.redial.notify_one();
    }

    /// Makes `addr` the first place to look for `id`, and looks now.
    fn set_address(&self, id: DeviceId, addr: SocketAddr) {
        if let Ok(mut r) = self.reach.lock() {
            r.remember(id, addr);
            // Either side may dial a typed address straight away.
            r.unlinked.insert(id, Instant::now() - FALLBACK);
        }
        self.save_addresses();
        self.redial.notify_one();
    }

    /// The phone is on another network (or none): links over the old one
    /// are dead, and what was nearby no longer is. Starts looking afresh.
    async fn network_changed(&mut self) {
        if let Some(d) = self.driven.take() {
            self.control(Control::Hide);
            self.send_to(d.by, &Message::Leave).await;
        }
        self.release().await;
        let ids: Vec<DeviceId> = self.links.keys().copied().collect();
        for id in ids {
            if let Some(link) = self.links.remove(&id) {
                link.sender.close();
            }
            self.unlinked(id);
        }
        if self.pairing.as_ref().is_some_and(|p| p.answer.is_none()) {
            self.cancel_quietly();
            self.emit(Event::PairingClosed);
        }
        self.discovery_run += 1;
        self.nearby.clear();
        if let Ok(mut r) = self.reach.lock() {
            r.found.clear();
        }
        if let Ok((discovery, found)) = discover(&self.identity, self.ports) {
            // Dropping the old one says goodbye on whatever network is left.
            self.discovery = discovery;
            watch_discovery(found, self.discovery_run, self.tx.clone());
        }
        self.redial.notify_one();
        self.publish_nearby();
        self.publish_partners();
    }

    /// What computers are told about this phone: its screen (they may drive
    /// it), or that it's only a remote.
    fn hello(&self) -> Message {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        match self.controllable {
            Some((w, h)) => Message::Screen {
                width: w as u32,
                height: h as u32,
            },
            None => Message::Remote,
        }
    }

    /// A computer's pointer came over to this phone across `edge`, at
    /// `position` along it.
    async fn driven_enter(&mut self, by: DeviceId, edge: Edge, position: f32) {
        let Some((w, h)) = self.controllable else {
            // Not allowed to be driven (Accessibility service off).
            self.send_to(by, &Message::Leave).await;
            return;
        };
        if self.controlling.is_some() {
            // We're driving a computer ourselves.
            self.send_to(by, &Message::Leave).await;
            return;
        }
        let p = position.clamp(0.0, 1.0);
        let inset = 4.0;
        let (x, y) = match edge {
            Edge::Left => (inset, p * h),
            Edge::Right => (w - inset, p * h),
            Edge::Top => (p * w, inset),
            Edge::Bottom => (p * w, h - inset),
        };
        self.driven = Some(Driven {
            by,
            edge,
            x,
            y,
            pressed_at: None,
            shift: false,
        });
        self.control(Control::Show { x, y });
    }

    /// Input from the computer driving this phone.
    async fn driven_input(&mut self, m: Message) {
        let (Some(mut d), Some((w, h))) = (self.driven, self.controllable) else {
            return;
        };
        match m {
            Message::PointerMotion { dx, dy } => {
                #[allow(clippy::cast_precision_loss)]
                let (nx, ny) = (
                    d.x + dx as f32 * DRIVEN_SPEED,
                    d.y + dy as f32 * DRIVEN_SPEED,
                );
                // Back over the edge it came in by: control goes back.
                let (out, along) = match d.edge {
                    Edge::Left => (nx < 0.0, ny / h),
                    Edge::Right => (nx > w, ny / h),
                    Edge::Top => (ny < 0.0, nx / w),
                    Edge::Bottom => (ny > h, nx / w),
                };
                if out && d.pressed_at.is_none() {
                    self.driven = None;
                    self.control(Control::Hide);
                    self.send_to(
                        d.by,
                        &Message::LeaveAt {
                            position: along.clamp(0.0, 1.0),
                        },
                    )
                    .await;
                    return;
                }
                d.x = nx.clamp(0.0, w - 1.0);
                d.y = ny.clamp(0.0, h - 1.0);
                self.control(Control::Move { x: d.x, y: d.y });
            }
            Message::PointerButton { button, pressed } => match (button, pressed) {
                (Button::Left, true) => d.pressed_at = Some((d.x, d.y)),
                (Button::Left, false) => {
                    if let Some((x0, y0)) = d.pressed_at.take() {
                        if (d.x - x0).hypot(d.y - y0) > DRAG_SLOP {
                            self.control(Control::Swipe {
                                x1: x0,
                                y1: y0,
                                x2: d.x,
                                y2: d.y,
                                ms: 350,
                            });
                        } else {
                            self.control(Control::Tap { x: d.x, y: d.y });
                        }
                    }
                }
                (Button::Right | Button::Back, true) => self.control(Control::Back),
                (Button::Middle, true) => self.control(Control::Home),
                _ => {}
            },
            Message::Scroll { dy, .. } if dy.is_finite() && dy != 0.0 => {
                // Scrolling down moves the content up: swipe upwards.
                let dist = (dy * SWIPE_PER_LINE).clamp(-h * 0.6, h * 0.6);
                let y2 = (d.y - dist).clamp(1.0, h - 1.0);
                self.control(Control::Swipe {
                    x1: d.x,
                    y1: d.y,
                    x2: d.x,
                    y2,
                    ms: 120,
                });
            }
            Message::Key { code, pressed } => {
                if keys::is_shift(code) {
                    d.shift = pressed;
                } else if pressed {
                    match code {
                        keys::ENTER => self.control(Control::Enter),
                        keys::BACKSPACE => self.control(Control::Backspace),
                        keys::ESCAPE => self.control(Control::Back),
                        c if keys::is_super(c) => self.control(Control::Home),
                        c => {
                            if let Some(ch) = keys::to_char(c, d.shift) {
                                self.control(Control::Text(ch.to_string()));
                            }
                        }
                    }
                }
            }
            // The computer took its own mouse back (its panic key).
            Message::Leave | Message::LeaveAt { .. } => {
                self.driven = None;
                self.control(Control::Hide);
                return;
            }
            _ => {}
        }
        self.driven = Some(d);
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
            typed: None,
        });
    }

    fn start_guest(&mut self, seen: &Seen, auto: bool, typed: Option<IpAddr>) {
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
            typed,
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
        let candidate = self
            .nearby
            .iter()
            .filter(|(_, s)| self.ports.computer || !s.phone)
            .find_map(|(id, s)| {
                let token = s.pairing.clone()?;
                let fresh = !self.tried.contains(&(*id, token.clone()));
                (fresh && !self.links.contains_key(id)).then(|| (*id, token, s.clone()))
            });
        if let Some((id, token, seen)) = candidate {
            self.tried.insert((id, token));
            self.start_guest(&seen, true, None);
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

type Found = std_mpsc::Receiver<DiscoveryEvent>;

fn discover(
    identity: &Identity,
    ports: Ports,
) -> Result<(Discovery, Found), hopwyn_discovery::DiscoveryError> {
    if ports.computer {
        Discovery::start(identity, ports.link)
    } else {
        Discovery::start_phone(identity, ports.link)
    }
}

/// Forwards what discovery finds to the core, as whole snapshots.
fn watch_discovery(
    found: std_mpsc::Receiver<DiscoveryEvent>,
    run: u64,
    tx: mpsc::UnboundedSender<Internal>,
) {
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
                        seen.insert(
                            d.id,
                            Seen {
                                name: d.name,
                                addrs: v4,
                                pairing: d.pairing,
                                pairing_port: d.pairing_port.unwrap_or(PAIRING_PORT),
                                phone: d.phone,
                            },
                        );
                    }
                }
                DiscoveryEvent::Lost(id) => {
                    seen.remove(&id);
                }
            }
            if tx.send(Internal::Nearby(run, seen.clone())).is_err() {
                return;
            }
        }
    });
}

fn hand_over(
    conn: PeerConnection,
    dialed: bool,
    tx: &mpsc::UnboundedSender<Internal>,
) -> Result<(), ()> {
    let id = conn.peer();
    let addr = conn.remote_address();
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
    tx.send(Internal::Connected(NewLink {
        id,
        dialed,
        addr,
        sender,
        rx: msg_rx,
    }))
    .map_err(|_| ())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn events_become_lines_the_app_can_split() {
        let id = Identity::generate("Desk").id();
        let line = Event::Partners(vec![(
            id,
            "Desk\u{1f}PC".into(),
            true,
            false,
            "192.168.1.20".into(),
            false,
        )])
        .to_line();
        assert!(line.starts_with("partners\u{1f}"));
        // A separator inside a name is removed, so fields stay aligned.
        assert_eq!(line.matches('\u{1f}').count(), 1);
        assert!(line.contains("DeskPC\u{1d}1\u{1d}0\u{1d}192.168.1.20\u{1d}0"));
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
    fn typed_addresses_must_be_on_the_home_network() {
        assert_eq!(
            parse_address(" 192.168.1.20 ", 4243).unwrap(),
            "192.168.1.20:4243".parse().unwrap()
        );
        assert_eq!(
            parse_address("10.0.0.5:5000", 4243).unwrap(),
            "10.0.0.5:5000".parse().unwrap()
        );
        assert!(parse_address("8.8.8.8", 4243).is_err());
        assert!(parse_address("my-laptop", 4243).is_err());
    }

    #[test]
    fn both_sides_keep_the_link_the_higher_key_dialed() {
        // The higher side keeps its own dial over one it accepted.
        assert!(!replaces(true, true, false));
        assert!(replaces(true, false, true));
        // The lower side keeps the one it accepted over its own dial.
        assert!(!replaces(false, false, true));
        assert!(replaces(false, true, false));
        // A reconnect of the same kind replaces a stale link.
        assert!(replaces(true, true, true));
        assert!(replaces(false, false, false));
    }

    #[test]
    fn the_lower_key_waits_before_dialing() {
        let id = Identity::generate("Desk").id();
        let addr: SocketAddr = "192.168.1.20:4242".parse().unwrap();
        let mut reach = Reach::default();
        reach.remember(id, addr);
        let now = Instant::now();
        assert_eq!(reach.candidates(&id, true, now), vec![addr]);
        assert!(reach.candidates(&id, false, now).is_empty());
        reach.unlinked.insert(id, now - FALLBACK);
        assert_eq!(reach.candidates(&id, false, now), vec![addr]);
        reach.connected.insert(id);
        assert!(reach.candidates(&id, true, now).is_empty());
    }

    #[test]
    fn a_link_that_dies_young_is_redialed_more_slowly() {
        let mut pace = Pace::new();
        pace.dialed();
        // Dropped at once: wait, and longer than the first gap.
        let first = pace.wait(false).unwrap();
        assert!(first > RETRY && first <= RETRY * 2);
        pace.dialed();
        let second = pace.wait(false).unwrap();
        assert!(second > first);
        // A link that lasted is redialed without delay.
        pace.dialed();
        pace.dialed_at = Instant::now() - Pace::LASTED;
        assert!(pace.wait(false).is_none());
        assert_eq!(pace.gap, RETRY);
        // One that refuses every link is only tried now and then.
        pace.dialed();
        assert!(pace.wait(true).unwrap() > RETRY_MAX);
    }

    #[test]
    fn the_newest_address_is_tried_first() {
        let id = Identity::generate("Desk").id();
        let mut reach = Reach::default();
        for n in 1..=6u8 {
            reach.remember(id, SocketAddr::from(([192, 168, 1, n], 4242)));
        }
        let known = reach.known.get(&id).unwrap();
        assert_eq!(known.len(), MAX_KNOWN);
        assert_eq!(
            known.first(),
            Some(&SocketAddr::from(([192, 168, 1, 6], 4242)))
        );
        assert!(!reach.remember(id, SocketAddr::from(([192, 168, 1, 6], 4242))));
    }

    #[test]
    fn device_keys_round_trip_through_hex() {
        let id = Identity::generate("x").id();
        assert_eq!(parse_id(&hex(&id)), Some(id));
        assert_eq!(parse_id("nope"), None);
    }
}
