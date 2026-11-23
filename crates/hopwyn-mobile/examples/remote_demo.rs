//! Drives a computer the way the phone app does, from this machine:
//! if not paired yet, opens a pairing window and accepts the code when a
//! computer joins, takes control of the first paired
//! computer that connects, moves the pointer in a square, clicks, types,
//! and lets go.
//!
//! `cargo run -p hopwyn-mobile --example remote_demo -- <state-dir>`
//! Uses ports 4292/4293 so it can run beside a desktop copy.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use hopwyn_mobile::{Command, Event, Mobile, Ports};
use hopwyn_proto::Button;

fn main() {
    let dir = std::env::args()
        .nth(1)
        .map_or_else(|| PathBuf::from("/tmp/hopwyn-remote"), PathBuf::from);
    let ports = Ports {
        link: 4292,
        pairing: 4293,
        computer: false,
    };
    let m = match Mobile::start(dir, "Test Phone", ports) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("start failed: {e}");
            std::process::exit(1);
        }
    };
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut target = None;
    let mut hosting = false;
    while Instant::now() < deadline && target.is_none() {
        let Some(ev) = m.next_event(Duration::from_millis(500)) else {
            continue;
        };
        println!(
            "event: {}",
            ev.to_line().replace(['\u{1f}', '\u{1e}', '\u{1d}'], " | ")
        );
        match ev {
            Event::PairingConfirm { .. } => m.send(Command::Confirm(true)),
            Event::Partners(list) => {
                if list.is_empty() && !hosting {
                    // Not paired yet: open a pairing window for a computer to join.
                    hosting = true;
                    m.send(Command::AddComputer);
                }
                target = list.into_iter().find(|p| p.2).map(|p| p.0);
            }
            _ => {}
        }
    }
    let Some(id) = target else {
        eprintln!("no paired computer connected");
        std::process::exit(2);
    };
    m.send(Command::Control(id));
    std::thread::sleep(Duration::from_millis(800));
    for (dx, dy) in [(40.0, 0.0), (0.0, 40.0), (-40.0, 0.0), (0.0, -40.0)] {
        for _ in 0..10 {
            m.send(Command::Motion(dx / 10.0, dy / 10.0));
            std::thread::sleep(Duration::from_millis(16));
        }
    }
    m.send(Command::Button(Button::Left, true));
    m.send(Command::Button(Button::Left, false));
    m.send(Command::Text("Hi!".into()));
    std::thread::sleep(Duration::from_millis(500));
    m.send(Command::Release);
    std::thread::sleep(Duration::from_millis(500));
    while let Some(ev) = m.next_event(Duration::from_millis(200)) {
        println!(
            "event: {}",
            ev.to_line().replace(['\u{1f}', '\u{1e}', '\u{1d}'], " | ")
        );
    }
    m.send(Command::Stop);
    println!("done");
}
