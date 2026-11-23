#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A phone core pairs with a stand-in computer by typing its address,
//! then finds it again after a network change. Uses the real transport on
//! this machine, so it is ignored by default:
//! `cargo test -p hopwyn-mobile --test address -- --ignored`

use std::time::{Duration, Instant};

use hopwyn_mobile::{Command, Event, Mobile, Ports};

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

fn linked(e: &Event) -> bool {
    matches!(e, Event::Partners(l) if l.iter().any(|p| p.2))
}

#[test]
#[ignore = "opens real sockets and announces on the network"]
fn pairs_by_address_and_reconnects_after_a_network_change() {
    let dir = std::env::temp_dir().join(format!("hopwyn-address-{}", std::process::id()));
    let phone = Mobile::start(
        dir.join("phone"),
        "Phone",
        Ports {
            link: 4332,
            pairing: 4333,
            computer: false,
        },
    )
    .unwrap();
    let pc = Mobile::start(
        dir.join("pc"),
        "Desk",
        Ports {
            link: 4342,
            pairing: 4343,
            computer: true,
        },
    )
    .unwrap();

    // A bad address is refused with a reason, not silently.
    phone.send(Command::JoinAddress("8.8.8.8".into()));
    wait_event(&phone, "the refusal", |e| {
        matches!(e, Event::PairingFailed(_))
    });

    // The computer opens its window; the phone joins it by address.
    pc.send(Command::AddComputer);
    wait_event(&pc, "the window", |e| matches!(e, Event::PairingOpen));
    phone.send(Command::JoinAddress("127.0.0.1:4343".into()));
    for m in [&phone, &pc] {
        wait_event(m, "the code", |e| matches!(e, Event::PairingConfirm { .. }));
        m.send(Command::Confirm(true));
    }
    wait_event(&phone, "the link", linked);
    wait_event(&pc, "the link", linked);

    // The phone changes network: the link drops, then comes back from the
    // remembered address.
    phone.send(Command::NetworkChanged);
    wait_event(
        &phone,
        "the drop",
        |e| matches!(e, Event::Partners(l) if l.iter().all(|p| !p.2)),
    );
    let back = wait_event(&phone, "the link again", linked);
    match back {
        Event::Partners(l) => assert!(l.iter().any(|p| p.2 && !p.4.is_empty())),
        _ => unreachable!(),
    }

    phone.send(Command::Stop);
    pc.send(Command::Stop);
    let _ = std::fs::remove_dir_all(dir);
}
