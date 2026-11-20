//! The edge handoff engine.
//!
//! Every device runs the same engine, with screen-edge capture on the edge
//! that faces its peer and input injection for when the peer is in control.
//! The engine does no I/O: it takes what happened ([`Input`]) and returns
//! what to do ([`Action`]).
//!
//! The cursor crossing from A to B:
//!
//! 1. A's cursor hits A's edge barrier; capture activates on A.
//! 2. A sends `Enter` and becomes **inactive** (sending its input to B).
//! 3. B receives `Enter`. To put its cursor at the matching spot, B pushes
//!    it into its own shared edge; B's barrier catches it, and B releases
//!    capture with the cursor at the exact requested position. (Relative
//!    moves can't place it reliably: desktops scale and accelerate them.)
//! 4. B is **active** and injects everything A sends.
//!
//! The cursor coming back:
//!
//! 5. The injected cursor hits B's barrier. B sends `Leave`, releases its
//!    capture and goes idle.
//! 6. A receives `Leave`, releases capture and is active again.
//!
//! Safety: losing the connection always returns each device to its own
//! input, and a device that is sending never stays captured without a peer.

use std::time::{Duration, Instant};

use hopwyn_input::InputEvent;
use hopwyn_input::capture::CaptureEvent;
use hopwyn_proto::{Edge, Message};

/// After control returns, barrier hits within this window are released
/// without starting a new handoff (the cursor is often still pressed
/// against the edge).
pub const HANDOFF_GRACE: Duration = Duration::from_millis(500);
/// How long the placement push may take to hit our own barrier before we
/// accept control anyway, with the cursor wherever it is.
pub const ENTER_TIMEOUT: Duration = Duration::from_millis(500);
/// Right after placement, our own barrier can fire again from the tail of
/// the placement push. Hits within this window re-place the cursor instead
/// of handing control back.
pub const SETTLE: Duration = Duration::from_millis(400);

/// Something that happened.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Capture(CaptureEvent),
    Peer(Message),
    PeerConnected,
    PeerDisconnected,
    /// The placement push asked for by [`Action::PushToEdge`] has been sent.
    /// Opening the injection session can take a second or more, so the
    /// placement timeout counts from here, not from the `Enter` message.
    Pushed,
    /// Periodic tick, for timeouts.
    Tick,
}

/// Something to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Send a message to the peer.
    Send(Message),
    /// Inject an input event into this desktop.
    Inject(InputEvent),
    /// End capture. With a position, put the local cursor that far along
    /// the shared edge (0.0 to 1.0), just inside it.
    ReleaseCapture { position: Option<f32> },
    /// Push the local cursor hard into the shared edge, so it hits our own
    /// barrier and can be placed exactly with `ReleaseCapture`.
    PushToEdge,
}

/// Who is in control, as seen by this device.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum State {
    /// This device's own input drives this device.
    Idle,
    /// Captured: this device's input goes to the peer. This device is inactive.
    Sending,
    /// The peer handed over control; placing the cursor.
    Entering { position: f32, since: Instant },
    /// The peer's input drives this device. This device is active.
    Receiving,
}

/// Where the cursor was placed on entry, and when.
#[derive(Debug, Clone, Copy)]
struct Placed {
    position: f32,
    at: Instant,
}

/// Engine configuration.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// The edge of this desktop that faces the peer.
    pub edge: Edge,
    /// If both devices try to take control at the same moment, the one with
    /// `wins_ties` keeps it. Exactly one side must set this (for example the
    /// one with the lower device key).
    pub wins_ties: bool,
}

#[derive(Debug)]
pub struct Engine {
    config: Config,
    state: State,
    connected: bool,
    /// Sub-pixel motion not yet sent, so slow movements aren't lost to rounding.
    carry: (f32, f32),
    /// Barrier hits before this instant are released without a handoff.
    grace_until: Option<Instant>,
    /// Set when placement finished, for the settle window.
    placed: Option<Placed>,
}

impl Engine {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            state: State::Idle,
            connected: false,
            carry: (0.0, 0.0),
            grace_until: None,
            placed: None,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// True while the peer's input is being injected into this device.
    pub fn is_active_for_peer(&self) -> bool {
        self.state == State::Receiving
    }

    /// True while this device needs its input-injection session open.
    pub fn needs_injection(&self) -> bool {
        matches!(self.state, State::Entering { .. } | State::Receiving)
    }

    fn idle_after_handoff(&mut self, now: Instant) {
        self.state = State::Idle;
        self.grace_until = Some(now + HANDOFF_GRACE);
    }

    pub fn handle(&mut self, input: Input, now: Instant) -> Vec<Action> {
        match input {
            Input::PeerConnected => {
                self.connected = true;
                Vec::new()
            }
            Input::PeerDisconnected => {
                self.connected = false;
                let was = std::mem::replace(&mut self.state, State::Idle);
                match was {
                    State::Sending => vec![Action::ReleaseCapture { position: None }],
                    _ => Vec::new(),
                }
            }
            Input::Pushed => {
                if let State::Entering { position, .. } = self.state {
                    self.state = State::Entering {
                        position,
                        since: now,
                    };
                }
                Vec::new()
            }
            Input::Tick => match self.state {
                State::Entering { position, since }
                    if now.duration_since(since) >= ENTER_TIMEOUT =>
                {
                    // A late barrier hit from the push is still placement,
                    // not the cursor heading back.
                    self.state = State::Receiving;
                    self.placed = Some(Placed { position, at: now });
                    Vec::new()
                }
                _ => Vec::new(),
            },
            Input::Capture(ev) => self.on_capture(ev, now),
            Input::Peer(msg) => self.on_peer(msg, now),
        }
    }

    fn on_capture(&mut self, ev: CaptureEvent, now: Instant) -> Vec<Action> {
        match (self.state, ev) {
            // A barrier hit just after control came back: release, don't hand off.
            (State::Idle, CaptureEvent::Activated { .. })
                if self.grace_until.is_some_and(|until| now < until) =>
            {
                vec![Action::ReleaseCapture { position: None }]
            }
            // Our cursor hit the edge: hand control to the peer.
            (State::Idle, CaptureEvent::Activated { position, .. }) => {
                if self.connected {
                    self.state = State::Sending;
                    self.carry = (0.0, 0.0);
                    vec![Action::Send(Message::Enter {
                        edge: opposite(self.config.edge),
                        position,
                    })]
                } else {
                    vec![Action::ReleaseCapture { position: None }]
                }
            }
            (State::Sending, CaptureEvent::Input(input)) => {
                self.forward(input).into_iter().collect()
            }
            // Capture ended on our side (panic shortcut or desktop): take control back.
            (State::Sending, CaptureEvent::Released(_) | CaptureEvent::Disabled) => {
                self.state = State::Idle;
                vec![Action::Send(Message::Leave)]
            }
            // The placement push hit our barrier: put the cursor where the peer left off.
            (State::Entering { position, .. }, CaptureEvent::Activated { .. }) => {
                self.state = State::Receiving;
                self.placed = Some(Placed { position, at: now });
                vec![Action::ReleaseCapture {
                    position: Some(position),
                }]
            }
            // An echo of the placement push: place the cursor again, stay active.
            (State::Receiving, CaptureEvent::Activated { .. })
                if self
                    .placed
                    .is_some_and(|p| now.duration_since(p.at) < SETTLE) =>
            {
                let position = self.placed.map(|p| p.position);
                vec![Action::ReleaseCapture { position }]
            }
            // The injected cursor reached our edge: hand control back.
            (State::Receiving, CaptureEvent::Activated { position, .. }) => {
                self.idle_after_handoff(now);
                vec![
                    Action::Send(Message::Leave),
                    Action::ReleaseCapture {
                        position: Some(position),
                    },
                ]
            }
            (State::Sending, CaptureEvent::Activated { .. })
            | (_, CaptureEvent::Input(_) | CaptureEvent::Released(_) | CaptureEvent::Disabled) => {
                Vec::new()
            }
        }
    }

    fn on_peer(&mut self, msg: Message, now: Instant) -> Vec<Action> {
        match msg {
            Message::Enter { position, .. } => match self.state {
                State::Idle => self.begin_entering(position, now),
                // Both pushed at once: one side keeps control.
                State::Sending if self.config.wins_ties => Vec::new(),
                State::Sending => {
                    let mut actions = vec![Action::ReleaseCapture { position: None }];
                    actions.extend(self.begin_entering(position, now));
                    actions
                }
                State::Entering { .. } | State::Receiving => Vec::new(),
            },
            Message::Leave => match self.state {
                // The peer handed control back to us.
                State::Sending => {
                    self.idle_after_handoff(now);
                    vec![Action::ReleaseCapture { position: None }]
                }
                // The peer took its control back (its panic shortcut, or it gave up).
                State::Entering { .. } | State::Receiving => {
                    self.idle_after_handoff(now);
                    Vec::new()
                }
                State::Idle => Vec::new(),
            },
            Message::Disconnect => self.handle(Input::PeerDisconnected, now),
            other if self.is_active_for_peer() => InputEvent::from_message(&other)
                .map(Action::Inject)
                .into_iter()
                .collect(),
            _ => Vec::new(),
        }
    }

    fn begin_entering(&mut self, position: f32, now: Instant) -> Vec<Action> {
        self.state = State::Entering {
            position: position.clamp(0.0, 1.0),
            since: now,
        };
        vec![Action::PushToEdge]
    }

    fn forward(&mut self, input: InputEvent) -> Option<Action> {
        let msg = match input {
            InputEvent::Motion { dx, dy } => {
                let (x, y) = (self.carry.0 + dx, self.carry.1 + dy);
                let (ix, iy) = (to_i32(x), to_i32(y));
                #[allow(clippy::cast_precision_loss)]
                {
                    self.carry = (x - ix as f32, y - iy as f32);
                }
                if ix == 0 && iy == 0 {
                    return None;
                }
                Message::PointerMotion { dx: ix, dy: iy }
            }
            InputEvent::Button { button, pressed } => Message::PointerButton { button, pressed },
            InputEvent::Scroll { dx, dy } => Message::Scroll { dx, dy },
            InputEvent::Key { hid, pressed } => Message::Key { code: hid, pressed },
        };
        Some(Action::Send(msg))
    }
}

/// Truncates toward zero, saturating; non-finite input becomes 0.
#[allow(clippy::cast_possible_truncation)]
fn to_i32(v: f32) -> i32 {
    if v.is_finite() {
        v.trunc().clamp(-1_000_000.0, 1_000_000.0) as i32
    } else {
        0
    }
}

pub fn opposite(edge: Edge) -> Edge {
    match edge {
        Edge::Left => Edge::Right,
        Edge::Right => Edge::Left,
        Edge::Top => Edge::Bottom,
        Edge::Bottom => Edge::Top,
    }
}

#[cfg(test)]
mod tests;
