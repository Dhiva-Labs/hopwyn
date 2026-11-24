//! The desktop's text clipboard, read and set in the background.
//!
//! Wayland only lets the focused window touch the clipboard, so a
//! background app can't follow it the usual way. GNOME's remote-control
//! session can, and needs no permission dialog (see `mutter`). Only plain
//! text is handled, and anything a password manager marks as secret is
//! left alone.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::mpsc as std_mpsc;
use std::thread;
use std::time::Duration;

use ashpd::zbus::Proxy;
use ashpd::zbus::zvariant::{self, OwnedValue, Value};
use futures_util::StreamExt;
use hopwyn_proto::MAX_CLIPBOARD_TEXT;
use tokio::sync::mpsc;

use crate::InputError;

/// Text formats offered and accepted, best first.
const TEXT: [&str; 4] = [
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "STRING",
];
/// Password managers add this to what they copy.
const SECRET_HINT: &str = "x-kde-passwordManagerHint";
/// How long one read or write of the clipboard may take.
const TRANSFER: Duration = Duration::from_secs(3);

/// Follows and sets this desktop's clipboard. Dropping it stops.
pub struct LinuxClipboard {
    tx: mpsc::UnboundedSender<String>,
}

impl LinuxClipboard {
    /// Starts following the clipboard. Text copied on this desktop arrives
    /// on the returned channel. Fails where the desktop has no way to do
    /// this in the background.
    pub fn start() -> Result<(Self, std_mpsc::Receiver<String>), InputError> {
        let (tx, rx) = mpsc::unbounded_channel();
        let (copied_tx, copied_rx) = std_mpsc::channel();
        let (ready_tx, ready_rx) = std_mpsc::channel();
        thread::Builder::new()
            .name("hopwyn-clipboard".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = ready_tx.send(Err(InputError::Ei(e.to_string())));
                        return;
                    }
                };
                rt.block_on(run(rx, copied_tx, ready_tx));
            })
            .map_err(|e| InputError::Ei(e.to_string()))?;
        ready_rx.recv().map_err(|_| InputError::Closed)??;
        Ok((Self { tx }, copied_rx))
    }

    /// Puts `text` on this desktop's clipboard.
    pub fn set(&self, text: String) -> Result<(), InputError> {
        self.tx.send(text).map_err(|_| InputError::Closed)
    }
}

/// Tracing for diagnosing desktops, enabled with `HOPWYN_DEBUG=1`. Logs
/// formats and sizes only, never what was copied.
fn trace(step: &str) {
    if std::env::var_os("HOPWYN_DEBUG").is_some() {
        eprintln!("[hopwyn-clipboard] {step}");
    }
}

fn err(e: impl std::fmt::Display) -> InputError {
    InputError::Portal(e.to_string())
}

/// The formats in a `SelectionOwnerChanged` signal, and whether this
/// session is the owner.
fn offer(options: &HashMap<String, OwnedValue>) -> (Vec<String>, bool) {
    let types = options
        .get("mime-types")
        .and_then(|v| {
            // GNOME wraps the list in a one-field struct.
            <(Vec<String>,)>::try_from(v.clone())
                .map(|t| t.0)
                .or_else(|_| Vec::<String>::try_from(v.clone()))
                .ok()
        })
        .unwrap_or_default();
    let mine = options
        .get("session-is-owner")
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(false);
    (types, mine)
}

/// Which text format to ask for, unless the copy is marked secret.
fn pick(types: &[String]) -> Option<&'static str> {
    if types.iter().any(|t| t == SECRET_HINT) {
        return None;
    }
    TEXT.into_iter()
        .find(|want| types.iter().any(|t| t == want))
}

/// Reads the pipe to its end. The desktop hands over a pipe that doesn't
/// wait, and the app that copied may take a moment to fill it, so an empty
/// pipe is tried again until [`TRANSFER`] runs out.
fn read_all(file: &mut std::fs::File) -> Option<Vec<u8>> {
    let deadline = std::time::Instant::now() + TRANSFER;
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        match file.read(&mut chunk) {
            Ok(0) => return Some(bytes),
            Ok(n) => {
                bytes.extend_from_slice(chunk.get(..n)?);
                if bytes.len() > MAX_CLIPBOARD_TEXT {
                    return None;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    trace("reading the clipboard took too long");
                    return None;
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
}

/// Writes all of `bytes`, waiting out a full pipe until [`TRANSFER`] runs out.
fn write_all(file: &mut std::fs::File, mut bytes: &[u8]) -> bool {
    let deadline = std::time::Instant::now() + TRANSFER;
    while !bytes.is_empty() {
        match file.write(bytes) {
            Ok(0) => return false,
            Ok(n) => bytes = bytes.get(n..).unwrap_or_default(),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    return false;
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
    true
}

async fn read(session: &Proxy<'static>, mime: &str) -> Option<String> {
    let fd: zvariant::OwnedFd = match session.call("SelectionRead", &(mime,)).await {
        Ok(fd) => fd,
        Err(e) => {
            trace(&format!("could not read the clipboard: {e}"));
            return None;
        }
    };
    let mut file = std::fs::File::from(std::os::fd::OwnedFd::from(fd));
    let task = tokio::task::spawn_blocking(move || read_all(&mut file));
    let bytes = task.await.ok()??;
    String::from_utf8(bytes).ok().filter(|t| !t.is_empty())
}

async fn write(session: &Proxy<'static>, serial: u32, text: Option<String>) {
    let done = async {
        let text = text?;
        let fd: zvariant::OwnedFd = session.call("SelectionWrite", &(serial,)).await.ok()?;
        let mut file = std::fs::File::from(std::os::fd::OwnedFd::from(fd));
        let task = tokio::task::spawn_blocking(move || write_all(&mut file, text.as_bytes()));
        task.await.ok()
    };
    let ok = done.await.unwrap_or(false);
    let _: Result<(), _> = session.call("SelectionWriteDone", &(serial, ok)).await;
}

async fn run(
    mut rx: mpsc::UnboundedReceiver<String>,
    copied: std_mpsc::Sender<String>,
    ready: std_mpsc::Sender<Result<(), InputError>>,
) {
    let Some(desktop) = crate::mutter::Desktop::open().await else {
        let _ = ready.send(Err(InputError::Portal(
            "this desktop can't share the clipboard in the background".into(),
        )));
        return;
    };
    let session = desktop.proxy();
    let setup = async {
        let none: HashMap<&str, Value<'_>> = HashMap::new();
        session
            .call::<_, _, ()>("EnableClipboard", &(none,))
            .await
            .map_err(err)?;
        let owner = session
            .receive_signal("SelectionOwnerChanged")
            .await
            .map_err(err)?;
        let transfer = session
            .receive_signal("SelectionTransfer")
            .await
            .map_err(err)?;
        let closed = session.receive_signal("Closed").await.map_err(err)?;
        Ok::<_, InputError>((owner, transfer, closed))
    };
    let (mut owner, mut transfer, mut closed) = match setup.await {
        Ok(v) => v,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    if ready.send(Ok(())).is_err() {
        return;
    }
    trace("following the clipboard");

    // The text this session offers to the desktop, while it is the owner.
    let mut mine: Option<String> = None;
    // The last text seen either way, so nothing is sent back where it
    // came from.
    let mut last = String::new();
    loop {
        tokio::select! {
            text = rx.recv() => {
                let Some(text) = text else { break };
                if text == last {
                    continue;
                }
                let mut options: HashMap<&str, Value<'_>> = HashMap::new();
                options.insert("mime-types", Value::from(TEXT.to_vec()));
                match session.call::<_, _, ()>("SetSelection", &(options,)).await {
                    Ok(()) => {
                        trace(&format!("offering {} bytes", text.len()));
                        last.clone_from(&text);
                        mine = Some(text);
                    }
                    Err(e) => trace(&format!("could not set the clipboard: {e}")),
                }
            }
            msg = owner.next() => {
                let Some(msg) = msg else { break };
                let Ok((options,)) = msg.body().deserialize::<(HashMap<String, OwnedValue>,)>() else {
                    continue;
                };
                let (types, by_me) = offer(&options);
                trace(&format!("owner changed (ours: {by_me}): {types:?}"));
                if by_me {
                    continue;
                }
                mine = None;
                let Some(mime) = pick(&types) else { continue };
                let text = read(session, mime).await;
                trace(&format!("read {:?} bytes as {mime}", text.as_ref().map(String::len)));
                if let Some(text) = text
                    && text != last
                {
                    last.clone_from(&text);
                    if copied.send(text).is_err() {
                        break;
                    }
                }
            }
            msg = transfer.next() => {
                let Some(msg) = msg else { break };
                let Ok((mime, serial)) = msg.body().deserialize::<(String, u32)>() else {
                    continue;
                };
                trace(&format!("asked for {mime} (have text: {})", mine.is_some()));
                let text = mine.clone().filter(|_| TEXT.contains(&mime.as_str()));
                write(session, serial, text).await;
            }
            _ = closed.next() => break,
        }
    }
    desktop.close().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn plain_text_is_picked_best_format_first() {
        assert_eq!(
            pick(&types(&[
                "text/html",
                "text/plain",
                "text/plain;charset=utf-8"
            ])),
            Some("text/plain;charset=utf-8")
        );
        assert_eq!(pick(&types(&["STRING"])), Some("STRING"));
        assert_eq!(pick(&types(&["image/png"])), None);
    }

    #[test]
    fn copies_marked_secret_are_left_alone() {
        assert_eq!(
            pick(&types(&["text/plain", "x-kde-passwordManagerHint"])),
            None
        );
    }
}
