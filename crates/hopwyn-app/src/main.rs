//! Hopwyn desktop app.

mod pairing_net;

#[cfg(target_os = "linux")]
mod config;
#[cfg(target_os = "linux")]
mod service;
#[cfg(target_os = "linux")]
mod ui;

#[cfg(target_os = "linux")]
fn main() -> gtk::glib::ExitCode {
    ui::run()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("The Hopwyn app currently runs on Linux; Windows and macOS are planned for v1.0.");
}
