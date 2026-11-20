//! Captures input at the right edge of the screen, to check that capture
//! works on this desktop.
//!
//! Push the cursor against the right edge: the local cursor stops and input
//! is captured. Capture releases by itself after 5 seconds, or at once with
//! Ctrl+Alt+Esc. Only event counts are printed, never which keys were typed.
//! The demo exits after 60 seconds.
//!
//! Run: `cargo run -p hopwyn-input --example capture_demo`

#[cfg(target_os = "linux")]
fn main() {
    use std::time::{Duration, Instant};

    use hopwyn_input::InputEvent;
    use hopwyn_input::capture::CaptureEvent;
    use hopwyn_input::linux_capture::LinuxCapture;
    use hopwyn_proto::Edge;

    println!("GNOME will ask permission to capture input. The dialog may open behind");
    println!("other windows: check the Activities overview if you don't see it.");
    let (capture, events) = match LinuxCapture::start(Edge::Right) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Could not start capture: {e}");
            std::process::exit(1);
        }
    };
    println!("Ready. Push the cursor against the RIGHT edge of your screen.");

    let end = Instant::now() + Duration::from_secs(60);
    let mut captured_since: Option<Instant> = None;
    let (mut motion, mut keys, mut buttons, mut scrolls) = (0u32, 0u32, 0u32, 0u32);

    while Instant::now() < end {
        if let Some(t) = captured_since {
            if t.elapsed() >= Duration::from_secs(5) {
                let _ = capture.release();
                captured_since = None;
            }
        }
        match events.recv_timeout(Duration::from_millis(100)) {
            Ok(CaptureEvent::Activated { position }) => {
                println!(
                    "Captured at {:.0}% down the edge. Move, click, type; releasing in 5 s (or Ctrl+Alt+Esc).",
                    position * 100.0
                );
                captured_since = Some(Instant::now());
                (motion, keys, buttons, scrolls) = (0, 0, 0, 0);
            }
            Ok(CaptureEvent::Input(ev)) => match ev {
                InputEvent::Motion { .. } => motion += 1,
                InputEvent::Key { .. } => keys += 1,
                InputEvent::Button { .. } => buttons += 1,
                InputEvent::Scroll { .. } => scrolls += 1,
            },
            Ok(CaptureEvent::Released(reason)) => {
                captured_since = None;
                println!(
                    "Released ({reason:?}). Captured {motion} motion, {buttons} button, {scrolls} scroll, {keys} key events."
                );
                println!("Push the right edge again to repeat, or wait for the demo to end.");
            }
            Ok(CaptureEvent::Disabled) => {
                println!("The desktop turned capture off.");
                break;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                println!("Capture ended.");
                break;
            }
        }
    }
    let _ = capture.release();
    std::thread::sleep(Duration::from_millis(200));
    println!("Demo finished.");
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("This demo is Linux-only.");
}
