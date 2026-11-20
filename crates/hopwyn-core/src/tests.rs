#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use super::*;
use hopwyn_input::capture::ReleaseReason;
use hopwyn_proto::Button;

fn engine(edge: Edge, wins_ties: bool) -> Engine {
    let mut e = Engine::new(Config { edge, wins_ties });
    e.handle(Input::PeerConnected, Instant::now());
    e
}

fn activated(position: f32) -> Input {
    Input::Capture(CaptureEvent::Activated {
        edge: Edge::Right,
        position,
    })
}

fn captured(ev: InputEvent) -> Input {
    Input::Capture(CaptureEvent::Input(ev))
}

/// Two engines wired together: actions that send a message are delivered
/// to the other side. Returns the actions each side must perform locally.
struct Pair {
    a: Engine,
    b: Engine,
    now: Instant,
}

impl Pair {
    fn new() -> Self {
        // A's right edge faces B's left edge.
        Self {
            a: engine(Edge::Right, true),
            b: engine(Edge::Left, false),
            now: Instant::now(),
        }
    }

    fn run(&mut self, to_a: bool, input: Input) -> (Vec<Action>, Vec<Action>) {
        let mut local_a = Vec::new();
        let mut local_b = Vec::new();
        let mut queue = vec![(to_a, input)];
        while let Some((is_a, input)) = queue.pop() {
            let (eng, local) = if is_a {
                (&mut self.a, &mut local_a)
            } else {
                (&mut self.b, &mut local_b)
            };
            for action in eng.handle(input, self.now) {
                match action {
                    Action::Send(m) => queue.insert(0, (!is_a, Input::Peer(m))),
                    other => local.push(other),
                }
            }
        }
        (local_a, local_b)
    }

    /// A hands control to B and B finishes placing its cursor.
    fn cross_a_to_b(&mut self, position: f32) {
        let (_, b) = self.run(true, activated(position));
        assert_eq!(b, vec![Action::PushToEdge]);
        let (_, b) = self.run(false, activated(0.99));
        assert_eq!(
            b,
            vec![Action::ReleaseCapture {
                position: Some(position)
            }]
        );
    }
}

#[test]
fn full_round_trip_a_to_b_and_back() {
    let mut p = Pair::new();

    // A's cursor hits the right edge halfway down; B pushes into its own edge.
    let (a, b) = p.run(true, activated(0.5));
    assert!(a.is_empty());
    assert_eq!(b, vec![Action::PushToEdge]);
    assert_eq!(p.a.state(), State::Sending);
    assert!(matches!(p.b.state(), State::Entering { position, .. } if position == 0.5));
    assert!(p.b.needs_injection());
    assert!(!p.b.is_active_for_peer());

    // B's push hits B's barrier: B releases with the cursor at 0.5.
    let (a, b) = p.run(false, activated(0.9));
    assert!(a.is_empty());
    assert_eq!(
        b,
        vec![Action::ReleaseCapture {
            position: Some(0.5)
        }]
    );
    assert_eq!(p.b.state(), State::Receiving);

    // A's mouse and keyboard now drive B.
    let (_, b) = p.run(true, captured(InputEvent::Motion { dx: 10.0, dy: -4.0 }));
    assert_eq!(
        b,
        vec![Action::Inject(InputEvent::Motion { dx: 10.0, dy: -4.0 })]
    );
    let (_, b) = p.run(
        true,
        captured(InputEvent::Key {
            hid: 0x04,
            pressed: true,
        }),
    );
    assert_eq!(
        b,
        vec![Action::Inject(InputEvent::Key {
            hid: 0x04,
            pressed: true
        })]
    );
    let (_, b) = p.run(
        true,
        captured(InputEvent::Button {
            button: Button::Left,
            pressed: true,
        }),
    );
    assert_eq!(
        b,
        vec![Action::Inject(InputEvent::Button {
            button: Button::Left,
            pressed: true
        })]
    );

    // Later, the injected cursor reaches B's left edge: control returns to A.
    p.now += SETTLE;
    let (a, b) = p.run(false, activated(0.3));
    assert_eq!(
        b,
        vec![Action::ReleaseCapture {
            position: Some(0.3)
        }]
    );
    assert_eq!(a, vec![Action::ReleaseCapture { position: None }]);
    assert_eq!(p.a.state(), State::Idle);
    assert_eq!(p.b.state(), State::Idle);
}

#[test]
fn input_during_placement_is_not_injected() {
    let mut p = Pair::new();
    p.run(true, activated(0.5));
    let (_, b) = p.run(true, captured(InputEvent::Motion { dx: 5.0, dy: 0.0 }));
    assert!(b.is_empty());
}

#[test]
fn placement_timeout_accepts_control_anyway() {
    let mut b = engine(Edge::Left, false);
    let t0 = Instant::now();
    b.handle(
        Input::Peer(Message::Enter {
            edge: Edge::Left,
            position: 0.5,
        }),
        t0,
    );
    b.handle(Input::Tick, t0 + Duration::from_millis(100));
    assert!(matches!(b.state(), State::Entering { .. }));
    b.handle(Input::Tick, t0 + ENTER_TIMEOUT);
    assert_eq!(b.state(), State::Receiving);
}

#[test]
fn b_can_drive_a_the_other_way() {
    let mut p = Pair::new();
    let (a, _) = p.run(false, activated(0.2));
    assert_eq!(a, vec![Action::PushToEdge]);
    assert_eq!(p.b.state(), State::Sending);
    p.run(true, activated(0.9));
    let (a, _) = p.run(false, captured(InputEvent::Scroll { dx: 0.0, dy: 1.0 }));
    assert_eq!(
        a,
        vec![Action::Inject(InputEvent::Scroll { dx: 0.0, dy: 1.0 })]
    );
}

#[test]
fn enter_message_uses_the_opposite_edge() {
    let mut a = engine(Edge::Right, true);
    let actions = a.handle(activated(0.25), Instant::now());
    assert_eq!(
        actions,
        vec![Action::Send(Message::Enter {
            edge: Edge::Left,
            position: 0.25
        })]
    );
}

#[test]
fn not_connected_releases_immediately() {
    let mut a = Engine::new(Config {
        edge: Edge::Right,
        wins_ties: true,
    });
    assert_eq!(
        a.handle(activated(0.5), Instant::now()),
        vec![Action::ReleaseCapture { position: None }]
    );
    assert_eq!(a.state(), State::Idle);
}

#[test]
fn disconnect_while_sending_gives_the_cursor_back() {
    let mut a = engine(Edge::Right, true);
    a.handle(activated(0.5), Instant::now());
    assert_eq!(
        a.handle(Input::PeerDisconnected, Instant::now()),
        vec![Action::ReleaseCapture { position: None }]
    );
    assert_eq!(a.state(), State::Idle);
}

#[test]
fn disconnect_while_receiving_stops_injecting() {
    let mut p = Pair::new();
    p.cross_a_to_b(0.5);
    p.b.handle(Input::PeerDisconnected, p.now);
    assert_eq!(p.b.state(), State::Idle);
    assert!(
        p.b.handle(Input::Peer(Message::PointerMotion { dx: 1, dy: 1 }), p.now)
            .is_empty()
    );
}

#[test]
fn panic_shortcut_on_sender_returns_control_and_tells_peer() {
    let mut p = Pair::new();
    p.cross_a_to_b(0.5);
    let (a, b) = p.run(
        true,
        Input::Capture(CaptureEvent::Released(ReleaseReason::PanicShortcut)),
    );
    assert!(a.is_empty() && b.is_empty());
    assert_eq!(p.a.state(), State::Idle);
    assert_eq!(p.b.state(), State::Idle);
}

#[test]
fn input_is_injected_only_while_active() {
    let mut b = engine(Edge::Left, false);
    let now = Instant::now();
    assert!(
        b.handle(Input::Peer(Message::PointerMotion { dx: 5, dy: 5 }), now)
            .is_empty()
    );
    assert!(
        b.handle(
            Input::Peer(Message::Key {
                code: 4,
                pressed: true
            }),
            now
        )
        .is_empty()
    );
}

#[test]
fn captured_input_is_ignored_unless_sending() {
    let mut a = engine(Edge::Right, true);
    assert!(
        a.handle(
            captured(InputEvent::Key {
                hid: 4,
                pressed: true
            }),
            Instant::now()
        )
        .is_empty()
    );
}

#[test]
fn simultaneous_push_is_resolved_by_tie_break() {
    let mut p = Pair::new();
    let now = p.now;
    let enter_from_a = p.a.handle(activated(0.5), now);
    let enter_from_b = p.b.handle(activated(0.5), now);
    let Action::Send(ma) = enter_from_a[0].clone() else {
        panic!()
    };
    let Action::Send(mb) = enter_from_b[0].clone() else {
        panic!()
    };
    // A wins ties: it ignores B's Enter and keeps sending.
    assert!(p.a.handle(Input::Peer(mb), now).is_empty());
    assert_eq!(p.a.state(), State::Sending);
    // B yields: releases its capture and starts placing A's cursor.
    assert_eq!(
        p.b.handle(Input::Peer(ma), now),
        vec![
            Action::ReleaseCapture { position: None },
            Action::PushToEdge
        ]
    );
    assert!(p.b.needs_injection());
}

#[test]
fn barrier_hit_right_after_return_does_not_bounce() {
    let mut p = Pair::new();
    p.cross_a_to_b(0.5);
    p.now += SETTLE;
    // B hands back; A, still pushing right, hits its own barrier at once.
    p.run(false, activated(0.5));
    let (a, b) = p.run(true, activated(0.5));
    assert_eq!(a, vec![Action::ReleaseCapture { position: None }]);
    assert!(b.is_empty());
    assert_eq!(p.a.state(), State::Idle);
    // Same on B: a second barrier hit right after handing back is released quietly.
    let (a, b) = p.run(false, activated(0.5));
    assert_eq!(b, vec![Action::ReleaseCapture { position: None }]);
    assert!(a.is_empty());
}

#[test]
fn handoff_works_again_after_the_grace_period() {
    let mut p = Pair::new();
    p.cross_a_to_b(0.5);
    p.now += SETTLE;
    p.run(false, activated(0.5));
    p.now += HANDOFF_GRACE;
    let (_, b) = p.run(true, activated(0.4));
    assert_eq!(b, vec![Action::PushToEdge]);
    assert_eq!(p.a.state(), State::Sending);
}

#[test]
fn sub_pixel_motion_accumulates() {
    let mut a = engine(Edge::Right, true);
    let now = Instant::now();
    a.handle(activated(0.5), now);
    let mut sent = 0;
    for _ in 0..10 {
        for action in a.handle(captured(InputEvent::Motion { dx: 0.3, dy: 0.0 }), now) {
            if let Action::Send(Message::PointerMotion { dx, .. }) = action {
                sent += dx;
            }
        }
    }
    assert_eq!(sent, 3);
}

#[test]
fn non_finite_motion_is_dropped() {
    let mut a = engine(Edge::Right, true);
    let now = Instant::now();
    a.handle(activated(0.5), now);
    assert!(
        a.handle(
            captured(InputEvent::Motion {
                dx: f32::NAN,
                dy: f32::INFINITY
            }),
            now
        )
        .is_empty()
    );
}

#[test]
fn barrier_echo_right_after_placement_keeps_control() {
    let mut p = Pair::new();
    p.cross_a_to_b(0.7);
    // The tail of the placement push trips B's barrier again at once.
    let (a, b) = p.run(false, activated(0.85));
    assert_eq!(
        b,
        vec![Action::ReleaseCapture {
            position: Some(0.7)
        }]
    );
    assert!(a.is_empty());
    assert_eq!(p.b.state(), State::Receiving);
    assert_eq!(p.a.state(), State::Sending);
}

#[test]
fn real_return_after_settling_hands_back() {
    let mut p = Pair::new();
    p.cross_a_to_b(0.7);
    p.now += SETTLE;
    let (a, b) = p.run(false, activated(0.4));
    assert_eq!(
        b,
        vec![Action::ReleaseCapture {
            position: Some(0.4)
        }]
    );
    assert_eq!(a, vec![Action::ReleaseCapture { position: None }]);
    assert_eq!(p.b.state(), State::Idle);
}
