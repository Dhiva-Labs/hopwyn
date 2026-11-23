#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Two phone cores on this machine: one drives the other the way a
//! computer would (Enter, motion, click, keys, back over the edge). Uses
//! real multicast discovery and the real transport, so it is ignored by
//! default: `cargo test -p hopwyn-mobile --test driven -- --ignored`

use std::time::{Duration, Instant};

use hopwyn_mobile::{Command, Control, Event, Mobile, Ports};
use hopwyn_proto::Button;

fn wait_event(m: &Mobile, what: &str, mut f: impl FnMut(&Event) -> bool) -> Event {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Some(e) = m.next_event(Duration::from_millis(200))
            && f(&e)
        {
            return e;
        }
    }
    panic!("timed out waiting for {what}");
}

fn wait_control(m: &Mobile, what: &str, mut f: impl FnMut(&Control) -> bool) -> Control {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(c) = m.next_control(Duration::from_millis(200))
            && f(&c)
        {
            return c;
        }
    }
    panic!("timed out waiting for {what}");
}

#[test]
#[ignore = "needs multicast on a real network interface"]
fn a_computer_drives_the_phone_and_comes_back() {
    let dir = std::env::temp_dir().join(format!("hopwyn-driven-{}", std::process::id()));
    let phone = Mobile::start(
        dir.join("phone"),
        "Phone",
        Ports {
            link: 4302,
            pairing: 4303,
            computer: false,
        },
    )
    .unwrap();
    let pc = Mobile::start(
        dir.join("pc"),
        "Driver",
        Ports {
            link: 4312,
            pairing: 4313,
            computer: true,
        },
    )
    .unwrap();
    phone.send(Command::SetControllable {
        enabled: true,
        width: 1080,
        height: 2400,
    });

    // Pair: the phone opens a window, the other joins on its own.
    phone.send(Command::AddComputer);
    for m in [&phone, &pc] {
        wait_event(m, "the code", |e| matches!(e, Event::PairingConfirm { .. }));
        m.send(Command::Confirm(true));
    }
    let id = match wait_event(
        &pc,
        "the link",
        |e| matches!(e, Event::Partners(l) if l.iter().any(|p| p.2)),
    ) {
        Event::Partners(l) => l.into_iter().find(|p| p.2).unwrap().0,
        _ => unreachable!(),
    };

    // Take over: the core sends Enter over its left edge, mid-way.
    pc.send(Command::Control(id));
    let shown = wait_control(&phone, "the pointer", |c| matches!(c, Control::Show { .. }));
    assert_eq!(shown, Control::Show { x: 4.0, y: 1200.0 });

    // Move right 100 px (130 phone px), click: a tap there.
    pc.send(Command::Motion(100.0, 0.0));
    wait_control(&phone, "a move", |c| matches!(c, Control::Move { .. }));
    pc.send(Command::Button(Button::Left, true));
    pc.send(Command::Button(Button::Left, false));
    let tap = wait_control(&phone, "a tap", |c| matches!(c, Control::Tap { .. }));
    assert_eq!(
        tap,
        Control::Tap {
            x: 134.0,
            y: 1200.0
        }
    );

    // Typing and a right click (Back).
    pc.send(Command::Text("Hi".into()));
    let mut typed = String::new();
    while typed.len() < 2 {
        if let Control::Text(t) = wait_control(&phone, "typing", |c| matches!(c, Control::Text(_)))
        {
            typed += &t;
        }
    }
    assert_eq!(typed, "Hi");
    pc.send(Command::Button(Button::Right, true));
    wait_control(&phone, "back", |c| *c == Control::Back);

    // Back over the left edge: the pointer hides and control returns.
    pc.send(Command::Motion(-400.0, 0.0));
    wait_control(&phone, "hiding", |c| *c == Control::Hide);
    wait_event(&pc, "control back", |e| *e == Event::Controlling(None));

    phone.send(Command::Stop);
    pc.send(Command::Stop);
    let _ = std::fs::remove_dir_all(dir);
}
