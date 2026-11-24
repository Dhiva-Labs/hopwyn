//! Follows this desktop's clipboard for 20 seconds, printing how long each
//! copied text is. With an argument, puts that text on the clipboard
//! first. `cargo run -p hopwyn-input --example clipboard_demo -- "hello"`

#[cfg(target_os = "linux")]
fn main() {
    use std::time::{Duration, Instant};

    use hopwyn_input::clipboard::LinuxClipboard;

    let (clipboard, copied) = match LinuxClipboard::start() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Could not follow the clipboard: {e}");
            return;
        }
    };
    if let Some(text) = std::env::args().nth(1) {
        let _ = clipboard.set(text);
        println!("Set the clipboard. Paste somewhere to check.");
    }
    let end = Instant::now() + Duration::from_secs(20);
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        if let Ok(text) = copied.recv_timeout(left) {
            println!("Copied: {} characters", text.chars().count());
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("This demo is Linux-only.");
}
