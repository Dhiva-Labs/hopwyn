//! Acts as a computer driving a phone, from this machine: opens a pairing
//! window, accepts the code, then pushes the pointer onto the phone, moves
//! it by the given amount, clicks, and brings it back.
//!
//! `cargo run -p hopwyn-mobile --example drive_phone -- <state-dir> <dx> <dy>`
//! Exits with 3 if pairing worked but no link came up (this side's key
//! sorts higher, so it would have to dial a phone behind an emulator's
//! NAT): rerun with a fresh dir.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use hopwyn_mobile::{Command, Event, Mobile, Ports};
use hopwyn_proto::Button;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = PathBuf::from(
        args.get(1)
            .cloned()
            .unwrap_or_else(|| "/tmp/hopwyn-drive".into()),
    );
    let dx: f32 = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(300.0);
    let dy: f32 = args.get(3).and_then(|v| v.parse().ok()).unwrap_or(0.0);
    // Optional 4th argument: a key (hex) this side's key must sort below,
    // so this side waits to be dialled (a phone behind an emulator's NAT
    // can dial out but not be reached).
    if let Some(below) = args.get(4).and_then(|h| hopwyn_mobile::parse_id(h)) {
        let _ = std::fs::create_dir_all(&dir);
        loop {
            let id = hopwyn_pair::Identity::generate("Driver PC");
            if id.id().as_bytes() < below.as_bytes() {
                let _ = std::fs::write(dir.join("identity"), id.secret_bytes());
                break;
            }
        }
    }
    let Ok(m) = Mobile::start(
        dir,
        "Driver PC",
        Ports {
            link: 4322,
            pairing: 4323,
        },
    ) else {
        std::process::exit(1);
    };
    // Open a pairing window at once: a phone behind an emulator's NAT can
    // see this machine but not the other way round.
    m.send(Command::AddComputer);
    let mut target = None;
    let mut paired_at: Option<Instant> = None;
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline && target.is_none() {
        if paired_at.is_some_and(|t| t.elapsed() > Duration::from_secs(12)) {
            // Paired but never linked: this side's key sorts higher, so it
            // would have to dial the phone, which it can't reach.
            println!("paired but no link; rerun with a fresh dir");
            std::process::exit(3);
        }
        let Some(ev) = m.next_event(Duration::from_millis(300)) else {
            continue;
        };
        println!(
            "event: {}",
            ev.to_line().replace(['\u{1f}', '\u{1e}', '\u{1d}'], " | ")
        );
        match ev {
            Event::PairingConfirm { .. } => m.send(Command::Confirm(true)),
            Event::PairingDone(_) => paired_at = Some(Instant::now()),
            Event::Partners(list) => {
                target = list.into_iter().find(|p| p.2).map(|p| p.0);
            }
            _ => {}
        }
    }
    let Some(id) = target else {
        println!("no phone connected");
        std::process::exit(2);
    };
    m.send(Command::Control(id));
    std::thread::sleep(Duration::from_millis(700));
    for _ in 0..20 {
        m.send(Command::Motion(dx / 20.0, dy / 20.0));
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(500));
    m.send(Command::Button(Button::Left, true));
    m.send(Command::Button(Button::Left, false));
    println!("clicked");
    std::thread::sleep(Duration::from_millis(3000));
    // Back to the left edge, past it: control returns.
    m.send(Command::Motion(-5000.0, 0.0));
    let back = Instant::now() + Duration::from_secs(5);
    while Instant::now() < back {
        if let Some(ev) = m.next_event(Duration::from_millis(200)) {
            println!(
                "event: {}",
                ev.to_line().replace(['\u{1f}', '\u{1e}', '\u{1d}'], " | ")
            );
            if ev == Event::Controlling(None) {
                println!("control came back");
                break;
            }
        }
    }
    m.send(Command::Stop);
}
