//! `hopwyn`: the background service (developer build).
//!
//! ```text
//! hopwyn id
//!     Print this device's key. Creates it on first run.
//!
//! hopwyn run --edge <left|right|top|bottom> --trust <peer key> [--connect <ip:port>] [--port <n>]
//!     Share this machine's mouse and keyboard with one peer, whose screen
//!     sits beyond --edge. Without --connect the peer is found on the home
//!     network automatically.
//! ```
//!
//! Pairing by 6-digit code and the desktop app replace `--trust` later;
//! this binary exists to test the handoff end to end.

#[cfg(target_os = "linux")]
mod run;
#[cfg(target_os = "linux")]
mod store;

#[cfg(target_os = "linux")]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("id") => store::load_or_create_identity().map(|id| {
            println!("{}", store::hex_id(&id.id()));
            eprintln!("Device: {} ({})", id.name(), id.id());
        }),
        Some("run") => run::Options::parse(args.get(1..).unwrap_or_default()).and_then(run::run),
        _ => Err(USAGE.to_owned()),
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(2);
    }
}

#[cfg(target_os = "linux")]
const USAGE: &str = "usage:
  hopwyn id
  hopwyn run --edge <left|right|top|bottom> --trust <peer key> [--connect <ip:port>] [--port <n>]";

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("This developer build of hopwyn runs on Linux only for now.");
    std::process::exit(2);
}
