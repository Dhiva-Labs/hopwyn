//! Linux input injection through the RemoteDesktop portal and libei.
//!
//! Works on Wayland (GNOME 45+, KDE Plasma 6.1+) and on X11 sessions whose
//! portal supports `ConnectToEIS`. The first connection shows the desktop's
//! permission dialog; pass the returned restore token next time so the
//! user isn't asked again until they revoke it.

use std::os::unix::net::UnixStream;
use std::sync::mpsc as std_mpsc;
use std::thread;

use ashpd::desktop::remote_desktop::{
    ConnectToEISOptions, DeviceType, RemoteDesktop, SelectDevicesOptions, StartOptions,
};
use ashpd::desktop::{CreateSessionOptions, PersistMode};
use futures_util::StreamExt;
use reis::ei;
use reis::event::{Connection, Device, DeviceCapability, EiEvent};
use tokio::sync::mpsc;

use crate::keymap::{button_to_evdev, hid_to_evdev};
use crate::{InputError, InputEvent};

/// Events queued beyond this are dropped: stale input is worse than lost input.
const QUEUE: usize = 1024;
/// How long to wait, after the user allows access, for a usable pointer.
const READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Injects input into the local desktop. Dropping it ends the session.
pub struct LinuxEmulator {
    tx: mpsc::Sender<InputEvent>,
    restore_token: Option<String>,
}

impl LinuxEmulator {
    /// Opens a RemoteDesktop session. Blocks until the user answers the
    /// desktop's dialog (or the restore token is accepted) and the libei
    /// connection is ready.
    pub fn connect(restore_token: Option<&str>) -> Result<Self, InputError> {
        Self::connect_with_parent(restore_token, None)
    }

    /// Like [`connect`](Self::connect), showing the dialog on top of
    /// `parent` (see `parent_window`), so it can't open hidden.
    pub fn connect_with_parent(
        restore_token: Option<&str>,
        parent: Option<String>,
    ) -> Result<Self, InputError> {
        let token = restore_token.map(str::to_owned);
        let (tx, rx) = mpsc::channel(QUEUE);
        let (ready_tx, ready_rx) = std_mpsc::channel();

        thread::Builder::new()
            .name("hopwyn-ei".into())
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
                rt.block_on(run(token, parent, rx, ready_tx));
            })
            .map_err(|e| InputError::Ei(e.to_string()))?;

        let restore_token = ready_rx.recv().map_err(|_| InputError::Closed)??;
        Ok(Self { tx, restore_token })
    }

    /// The token to pass to [`connect`](Self::connect) next time, if the
    /// desktop issued one. Store it with the app's settings.
    pub fn restore_token(&self) -> Option<&str> {
        self.restore_token.as_deref()
    }

    /// Queues one input event. Never blocks; drops the event if the queue is full.
    pub fn send(&self, event: InputEvent) -> Result<(), InputError> {
        match self.tx.try_send(event) {
            Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => Ok(()),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(InputError::Closed),
        }
    }
}

type Ready = std_mpsc::Sender<Result<Option<String>, InputError>>;

async fn run(
    token: Option<String>,
    parent: Option<String>,
    mut rx: mpsc::Receiver<InputEvent>,
    ready: Ready,
) {
    let parent = crate::parent_window(parent.as_deref());
    let portal = |e: ashpd::Error| InputError::Portal(e.to_string());
    let setup = async {
        let rd = RemoteDesktop::new().await.map_err(portal)?;
        let session = rd
            .create_session(CreateSessionOptions::default())
            .await
            .map_err(portal)?;
        rd.select_devices(
            &session,
            SelectDevicesOptions::default()
                .set_devices(DeviceType::Keyboard | DeviceType::Pointer)
                .set_persist_mode(PersistMode::ExplicitlyRevoked)
                .set_restore_token(token.as_deref()),
        )
        .await
        .map_err(portal)?;
        let selected = rd
            .start(&session, parent.as_ref(), StartOptions::default())
            .await
            .map_err(portal)?
            .response()
            .map_err(portal)?;
        let new_token = selected.restore_token().map(str::to_owned);
        let fd = rd
            .connect_to_eis(&session, ConnectToEISOptions::default())
            .await
            .map_err(portal)?;
        let context =
            ei::Context::new(UnixStream::from(fd)).map_err(|e| InputError::Ei(e.to_string()))?;
        let (conn, events) = context
            .handshake_tokio("hopwyn", ei::handshake::ContextType::Sender)
            .await
            .map_err(|e| InputError::Ei(e.to_string()))?;
        Ok::<_, InputError>((rd, session, conn, events, new_token))
    };

    let (_rd, session, conn, mut events, new_token) = match setup.await {
        Ok(v) => v,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let mut state = State {
        conn,
        devices: Vec::new(),
        sequence: 0,
    };

    // Don't report ready until the compositor has resumed a pointer device:
    // events sent before that have nowhere to go and would be lost.
    let wait = async {
        while state.device_with(DeviceCapability::Pointer).is_none() {
            let Some(Ok(ev)) = events.next().await else {
                return false;
            };
            if !state.handle(ev) {
                return false;
            }
        }
        true
    };
    match tokio::time::timeout(READY_TIMEOUT, wait).await {
        Ok(true) => {}
        Ok(false) => {
            let _ = ready.send(Err(InputError::Ei("connection closed during setup".into())));
            return;
        }
        Err(_) => {
            let _ = ready.send(Err(InputError::Ei("no pointer device became ready".into())));
            return;
        }
    }
    if ready.send(Ok(new_token)).is_err() {
        return;
    }

    loop {
        tokio::select! {
            ev = events.next() => match ev {
                Some(Ok(ev)) => if !state.handle(ev) { break },
                _ => break,
            },
            cmd = rx.recv() => match cmd {
                Some(input) => state.emit(input),
                None => break,
            },
        }
    }
    // End the remote session explicitly so the desktop drops it at once.
    let _ = session.close().await;
}

struct State {
    conn: Connection,
    /// Devices the compositor has resumed; only these may send events.
    devices: Vec<Device>,
    sequence: u32,
}

impl State {
    /// Returns false when the connection is over.
    fn handle(&mut self, ev: EiEvent) -> bool {
        match ev {
            EiEvent::SeatAdded(s) => {
                s.seat.bind_capabilities(&[
                    DeviceCapability::Pointer,
                    DeviceCapability::Button,
                    DeviceCapability::Scroll,
                    DeviceCapability::Keyboard,
                ]);
            }
            EiEvent::DeviceResumed(r) => {
                r.device
                    .device()
                    .start_emulating(self.conn.serial(), self.sequence);
                self.sequence = self.sequence.wrapping_add(1);
                self.devices.retain(|d| d != &r.device);
                self.devices.push(r.device);
            }
            EiEvent::DevicePaused(p) => self.devices.retain(|d| d != &p.device),
            EiEvent::DeviceRemoved(r) => self.devices.retain(|d| d != &r.device),
            EiEvent::Disconnected(_) => return false,
            _ => {}
        }
        let _ = self.conn.flush();
        true
    }

    fn device_with(&self, cap: DeviceCapability) -> Option<&Device> {
        self.devices.iter().find(|d| d.has_capability(cap))
    }

    fn emit(&mut self, input: InputEvent) {
        let emitted = match input {
            InputEvent::Motion { dx, dy } => {
                self.device_with(DeviceCapability::Pointer).and_then(|d| {
                    d.interface::<ei::Pointer>()?.motion_relative(dx, dy);
                    Some(d)
                })
            }
            InputEvent::Button { button, pressed } => {
                let state = if pressed {
                    ei::button::ButtonState::Press
                } else {
                    ei::button::ButtonState::Released
                };
                self.device_with(DeviceCapability::Button).and_then(|d| {
                    d.interface::<ei::Button>()?
                        .button(u32::from(button_to_evdev(button)), state);
                    Some(d)
                })
            }
            InputEvent::Scroll { dx, dy } => {
                self.device_with(DeviceCapability::Scroll).and_then(|d| {
                    // libei discrete scroll: 120 units per wheel detent (line).
                    let to_units = |v: f32| (v * 120.0).round().clamp(-12_000.0, 12_000.0) as i32;
                    d.interface::<ei::Scroll>()?
                        .scroll_discrete(to_units(dx), to_units(dy));
                    Some(d)
                })
            }
            InputEvent::Key { hid, pressed } => {
                let code = hid_to_evdev(hid);
                let state = if pressed {
                    ei::keyboard::KeyState::Press
                } else {
                    ei::keyboard::KeyState::Released
                };
                self.device_with(DeviceCapability::Keyboard).and_then(|d| {
                    d.interface::<ei::Keyboard>()?.key(u32::from(code?), state);
                    Some(d)
                })
            }
        };
        if let Some(d) = emitted {
            d.device().frame(self.conn.serial(), monotonic_us());
            let _ = self.conn.flush();
        }
    }
}

/// CLOCK_MONOTONIC in microseconds, the time base libei expects.
fn monotonic_us() -> u64 {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    let secs = u64::try_from(t.tv_sec).unwrap_or(0);
    let nanos = u64::try_from(t.tv_nsec).unwrap_or(0);
    secs.saturating_mul(1_000_000).saturating_add(nanos / 1_000)
}
