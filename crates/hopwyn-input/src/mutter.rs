//! GNOME's own input interfaces, used directly.
//!
//! The portal shows a permission dialog each time an app asks to capture
//! input, and GNOME can't remember the answer. GNOME's compositor (Mutter)
//! offers the same sessions on the session bus to apps that are not
//! sandboxed, with no dialog: the user already chose to run the app, and
//! says what it may do in Hopwyn's own settings. Hopwyn uses these when
//! they are there and falls back to the portal when they are not (other
//! desktops, or a sandbox that hides them).

use std::collections::HashMap;
use std::os::fd::OwnedFd;

use ashpd::zbus::zvariant::{self, OwnedObjectPath, Structure, Value};
use ashpd::zbus::{Connection, Proxy, proxy::SignalStream};

use crate::InputError;
use crate::capture::Rect;

const CAPTURE: &str = "org.gnome.Mutter.InputCapture";
const CAPTURE_PATH: &str = "/org/gnome/Mutter/InputCapture";
const CAPTURE_SESSION: &str = "org.gnome.Mutter.InputCapture.Session";
const REMOTE: &str = "org.gnome.Mutter.RemoteDesktop";
const REMOTE_PATH: &str = "/org/gnome/Mutter/RemoteDesktop";
const REMOTE_SESSION: &str = "org.gnome.Mutter.RemoteDesktop.Session";
/// Keyboard and pointer.
const CAPABILITIES: u32 = 1 | 2;

fn err(e: impl std::fmt::Display) -> InputError {
    InputError::Portal(e.to_string())
}

/// Set `HOPWYN_PORTAL=1` to use the portal even where GNOME's interfaces
/// are available (for testing the fallback).
fn wanted() -> bool {
    std::env::var_os("HOPWYN_PORTAL").is_none()
}

/// Whether GNOME's own interfaces are there, so input and the clipboard
/// can be used without a permission dialog. Checked once.
pub(crate) fn available() -> bool {
    static FOUND: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FOUND.get_or_init(|| {
        if !wanted() {
            return false;
        }
        let check = async {
            let conn = Connection::session().await.ok()?;
            let bus = ashpd::zbus::fdo::DBusProxy::new(&conn).await.ok()?;
            for name in [CAPTURE, REMOTE] {
                let name = ashpd::zbus::names::BusName::try_from(name).ok()?;
                if !bus.name_has_owner(name).await.ok()? {
                    return None;
                }
            }
            Some(())
        };
        // Its own thread: the caller may already be inside a runtime.
        std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .is_ok_and(|rt| rt.block_on(check).is_some())
        })
        .join()
        .unwrap_or(false)
    })
}

/// Opens the session proxy at the path a `CreateSession` call returned.
async fn session_at(
    conn: &Connection,
    service: &'static str,
    at: OwnedObjectPath,
    interface: &'static str,
) -> Option<Proxy<'static>> {
    Proxy::new(conn, service, at, interface).await.ok()
}

/// A capture session: pointer barriers at screen edges, and the input that
/// crosses them.
pub(crate) struct Capture {
    _conn: Connection,
    session: Proxy<'static>,
}

impl Capture {
    /// Opens a session, or `None` where GNOME's interface isn't available.
    pub(crate) async fn open() -> Option<Self> {
        if !wanted() {
            return None;
        }
        let conn = Connection::session().await.ok()?;
        let manager = Proxy::new(&conn, CAPTURE, CAPTURE_PATH, CAPTURE)
            .await
            .ok()?;
        let at: OwnedObjectPath = manager.call("CreateSession", &(CAPABILITIES,)).await.ok()?;
        let session = session_at(&conn, CAPTURE, at, CAPTURE_SESSION).await?;
        Some(Self {
            _conn: conn,
            session,
        })
    }

    /// The screens, and the serial that barriers must quote.
    pub(crate) async fn zones(&self) -> Result<(u32, Vec<Rect>), InputError> {
        let (serial, zones): (u32, Vec<(u32, u32, i32, i32)>) =
            self.session.call("GetZones", &()).await.map_err(err)?;
        let rects = zones
            .into_iter()
            .map(|(w, h, x, y)| Rect { x, y, w, h })
            .collect();
        Ok((serial, rects))
    }

    pub(crate) async fn clear_barriers(&self) -> Result<(), InputError> {
        self.session.call("ClearBarriers", &()).await.map_err(err)
    }

    /// Adds one barrier line and returns the id GNOME gave it.
    pub(crate) async fn add_barrier(
        &self,
        serial: u32,
        line: (i32, i32, i32, i32),
    ) -> Result<u32, InputError> {
        self.session
            .call("AddBarrier", &(serial, line))
            .await
            .map_err(err)
    }

    pub(crate) async fn enable(&self) -> Result<(), InputError> {
        self.session.call("Enable", &()).await.map_err(err)
    }

    pub(crate) async fn disable(&self) -> Result<(), InputError> {
        self.session.call("Disable", &()).await.map_err(err)
    }

    /// Gives the cursor back, at `at` if given.
    pub(crate) async fn release(&self, at: Option<(f64, f64)>) -> Result<(), InputError> {
        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
        if let Some(at) = at {
            options.insert("cursor_position", Value::from(Structure::from(at)));
        }
        self.session.call("Release", &(options,)).await.map_err(err)
    }

    pub(crate) async fn connect_to_eis(&self) -> Result<OwnedFd, InputError> {
        let fd: zvariant::OwnedFd = self.session.call("ConnectToEIS", &()).await.map_err(err)?;
        Ok(fd.into())
    }

    pub(crate) async fn signal(
        &self,
        name: &'static str,
    ) -> Result<SignalStream<'static>, InputError> {
        self.session.receive_signal(name).await.map_err(err)
    }

    pub(crate) async fn close(&self) {
        let _: Result<(), _> = self.session.call("Close", &()).await;
    }
}

/// A remote-control session: injects input, and reads and sets the
/// clipboard.
pub(crate) struct Desktop {
    _conn: Connection,
    session: Proxy<'static>,
}

impl Desktop {
    /// Opens and starts a session, or `None` where GNOME's interface isn't
    /// available.
    pub(crate) async fn open() -> Option<Self> {
        if !wanted() {
            return None;
        }
        let conn = Connection::session().await.ok()?;
        let manager = Proxy::new(&conn, REMOTE, REMOTE_PATH, REMOTE).await.ok()?;
        let at: OwnedObjectPath = manager.call("CreateSession", &()).await.ok()?;
        let session = session_at(&conn, REMOTE, at, REMOTE_SESSION).await?;
        session.call::<_, _, ()>("Start", &()).await.ok()?;
        Some(Self {
            _conn: conn,
            session,
        })
    }

    pub(crate) async fn connect_to_eis(&self) -> Result<OwnedFd, InputError> {
        let options: HashMap<&str, Value<'_>> = HashMap::new();
        let fd: zvariant::OwnedFd = self
            .session
            .call("ConnectToEIS", &(options,))
            .await
            .map_err(err)?;
        Ok(fd.into())
    }

    pub(crate) fn proxy(&self) -> &Proxy<'static> {
        &self.session
    }

    pub(crate) async fn close(&self) {
        let _: Result<(), _> = self.session.call("Stop", &()).await;
    }
}
