//! Moves the mouse pointer in a small square through the RemoteDesktop
//! portal, to check that injection works on this desktop. Presses no keys
//! and no buttons.
//!
//! Run: `cargo run -p hopwyn-input --example inject_demo`
//! The desktop shows a permission dialog first; choose Allow / Share.

#[cfg(target_os = "linux")]
fn main() {
    use std::time::Duration;

    use hopwyn_input::InputEvent;
    use hopwyn_input::linux::LinuxEmulator;

    println!("Asking the desktop for permission to control the pointer...");
    let emu = match LinuxEmulator::connect(None) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Could not start: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "Connected. Restore token issued: {}",
        emu.restore_token().is_some()
    );
    std::thread::sleep(Duration::from_millis(500));

    println!("Moving the pointer in a 400 px square, twice (about 5 seconds)...");
    let side = [(5.0, 0.0), (0.0, 5.0), (-5.0, 0.0), (0.0, -5.0)];
    for (dx, dy) in side.iter().chain(side.iter()).copied() {
        for _ in 0..80 {
            if emu.send(InputEvent::Motion { dx, dy }).is_err() {
                eprintln!("Session ended early.");
                std::process::exit(1);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    std::thread::sleep(Duration::from_millis(300));
    println!("Done. The pointer should be back where it started.");
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("This demo is Linux-only.");
}
