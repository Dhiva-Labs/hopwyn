//! Linux input capture through the InputCapture portal and libei
//! (GNOME 45+, KDE Plasma 6.1+).
//!
//! A pointer barrier sits on one screen edge. When the cursor pushes
//! through it, the desktop hands all keyboard and mouse input to Hopwyn
//! (the local cursor stops moving) until [`LinuxCapture::release`] is
//! called, the user presses Ctrl+Alt+Esc, or the desktop ends it.

use std::num::NonZeroU32;
use std::os::unix::net::UnixStream;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use ashpd::desktop::Session;
use ashpd::desktop::input_capture::{
    Barrier, BarrierPosition, Capabilities, ConnectToEISOptions, CreateSessionOptions,
    EnableOptions, GetZonesOptions, InputCapture, ReleaseOptions, SetPointerBarriersOptions,
};
use futures_util::StreamExt;
use hopwyn_proto::Edge;
use reis::ei;
use reis::event::{DeviceCapability, EiEvent};
use tokio::sync::mpsc;

use crate::capture::{
    CaptureEvent, KeyOutcome, Rect, ReleaseReason, Translator, edge_barriers, edge_position,
    release_point,
};
use crate::{InputError, InputEvent};

/// Smooth-scroll pixels per line, for touchpads that send no discrete steps.
const SCROLL_PX_PER_LINE: f32 = 15.0;
/// Pixels inside the edge where the local cursor reappears on release.
const RELEASE_INSET: f64 = 24.0;
/// How long to wait for the user to answer the desktop's permission dialog.
const PERMISSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

enum Command {
    /// Release; with a position, put the cursor that far along the edge.
    Release(Option<f32>),
    /// Turn capture back on after the desktop disabled it.
    Enable,
    /// Move the barrier to another screen edge.
    SetEdge(Edge),
}

/// Captures input at one screen edge. Dropping it ends the session.
pub struct LinuxCapture {
    tx: mpsc::UnboundedSender<Command>,
    layout: Arc<Mutex<Vec<Rect>>>,
}

impl LinuxCapture {
    /// Starts capturing at `edge` of this desktop. Blocks until the portal
    /// session is set up. Events arrive on the returned channel.
    pub fn start(edge: Edge) -> Result<(Self, std_mpsc::Receiver<CaptureEvent>), InputError> {
        let (tx, rx) = mpsc::unbounded_channel();
        let layout = Arc::new(Mutex::new(Vec::new()));
        let layout_task = layout.clone();
        let (events_tx, events_rx) = std_mpsc::channel();
        let (ready_tx, ready_rx) = std_mpsc::channel();

        thread::Builder::new()
            .name("hopwyn-capture".into())
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
                rt.block_on(run(edge, rx, events_tx, ready_tx, layout_task));
            })
            .map_err(|e| InputError::Ei(e.to_string()))?;

        ready_rx.recv().map_err(|_| InputError::Closed)??;
        Ok((Self { tx, layout }, events_rx))
    }

    /// The desktop's screens, as the desktop reported them for capture.
    pub fn desktop(&self) -> Vec<Rect> {
        self.layout.lock().map(|l| l.clone()).unwrap_or_default()
    }

    /// Turns capture back on, for example after another session made the
    /// desktop disable it.
    pub fn enable(&self) -> Result<(), InputError> {
        self.tx
            .send(Command::Enable)
            .map_err(|_| InputError::Closed)
    }

    /// Moves the barrier to another screen edge, without a new permission dialog.
    pub fn set_edge(&self, edge: Edge) -> Result<(), InputError> {
        self.tx
            .send(Command::SetEdge(edge))
            .map_err(|_| InputError::Closed)
    }

    /// Ends the current capture and gives the local cursor back.
    pub fn release(&self) -> Result<(), InputError> {
        self.tx
            .send(Command::Release(None))
            .map_err(|_| InputError::Closed)
    }

    /// Ends the current capture and puts the local cursor `position` along
    /// the edge (0.0 to 1.0), just inside it.
    pub fn release_at(&self, position: f32) -> Result<(), InputError> {
        self.tx
            .send(Command::Release(Some(position.clamp(0.0, 1.0))))
            .map_err(|_| InputError::Closed)
    }
}

type Ready = std_mpsc::Sender<Result<(), InputError>>;

fn portal(e: ashpd::Error) -> InputError {
    InputError::Portal(e.to_string())
}

async fn zones_and_barriers(
    ic: &InputCapture,
    session: &Session<InputCapture>,
    edge: Edge,
) -> Result<Vec<Rect>, InputError> {
    trace("get zones");
    let zones = ic
        .zones(session, GetZonesOptions::default())
        .await
        .map_err(portal)?
        .response()
        .map_err(portal)?;
    let rects: Vec<Rect> = zones
        .regions()
        .iter()
        .map(|r| Rect {
            x: r.x_offset(),
            y: r.y_offset(),
            w: r.width(),
            h: r.height(),
        })
        .collect();
    let barriers: Vec<Barrier> = edge_barriers(&rects, edge)
        .into_iter()
        .zip(1u32..)
        .filter_map(|((x1, y1, x2, y2), id)| {
            Some(Barrier::new(
                NonZeroU32::new(id)?,
                BarrierPosition::new(x1, y1, x2, y2),
            ))
        })
        .collect();
    if barriers.is_empty() {
        return Err(InputError::Portal(
            "no screen edge to place a barrier on".into(),
        ));
    }
    trace(&format!(
        "set {} barrier(s) on {} zone(s)",
        barriers.len(),
        rects.len()
    ));
    let resp = ic
        .set_pointer_barriers(
            session,
            &barriers,
            zones.zone_set(),
            SetPointerBarriersOptions::default(),
        )
        .await
        .map_err(portal)?
        .response()
        .map_err(portal)?;
    if !resp.failed_barriers().is_empty() {
        return Err(InputError::Portal(
            "the desktop rejected the edge barrier".into(),
        ));
    }
    Ok(rects)
}

/// Setup tracing for diagnosing desktops, enabled with `HOPWYN_DEBUG=1`.
/// Logs step names only, never input.
fn trace(step: &str) {
    if std::env::var_os("HOPWYN_DEBUG").is_some() {
        eprintln!("[hopwyn-capture] {step}");
    }
}

async fn run(
    edge: Edge,
    mut rx: mpsc::UnboundedReceiver<Command>,
    out: std_mpsc::Sender<CaptureEvent>,
    ready: Ready,
    layout: Arc<Mutex<Vec<Rect>>>,
) {
    let ic = match InputCapture::new().await {
        Ok(ic) => ic,
        Err(e) => {
            let _ = ready.send(Err(portal(e)));
            return;
        }
    };
    let setup = async {
        // This step waits for the user to answer GNOME's permission dialog.
        trace("create_session (waiting for the permission dialog)");
        let create = ic.create_session(
            None,
            CreateSessionOptions::default()
                .set_capabilities(Capabilities::Keyboard | Capabilities::Pointer),
        );
        let (session, _caps) = tokio::time::timeout(PERMISSION_TIMEOUT, create)
            .await
            .map_err(|_| InputError::Portal("the permission dialog was not answered".into()))?
            .map_err(portal)?;
        trace("connect_to_eis");
        let fd = ic
            .connect_to_eis(&session, ConnectToEISOptions::default())
            .await
            .map_err(portal)?;
        let context =
            ei::Context::new(UnixStream::from(fd)).map_err(|e| InputError::Ei(e.to_string()))?;
        trace("ei handshake");
        let (conn, events) = context
            .handshake_tokio("hopwyn", ei::handshake::ContextType::Receiver)
            .await
            .map_err(|e| InputError::Ei(e.to_string()))?;
        trace("subscribe to signals");
        let activated = ic.receive_activated().await.map_err(portal)?;
        let deactivated = ic.receive_deactivated().await.map_err(portal)?;
        let disabled = ic.receive_disabled().await.map_err(portal)?;
        let zones_changed = ic.receive_zones_changed().await.map_err(portal)?;
        let rects = zones_and_barriers(&ic, &session, edge).await?;
        trace("enable");
        ic.enable(&session, EnableOptions::default())
            .await
            .map_err(portal)?;
        Ok::<_, InputError>((
            session,
            conn,
            events,
            activated,
            deactivated,
            disabled,
            zones_changed,
            rects,
        ))
    };

    let (session, conn, events, activated, deactivated, disabled, zones_changed, mut rects) =
        match setup.await {
            Ok(v) => v,
            Err(e) => {
                let _ = ready.send(Err(e));
                return;
            }
        };
    if let Ok(mut l) = layout.lock() {
        l.clone_from(&rects);
    }
    if ready.send(Ok(())).is_err() {
        return;
    }
    tokio::pin!(events, activated, deactivated, disabled, zones_changed);

    let mut translator = Translator::new();
    // (activation id, position along the edge) while captured.
    let mut active: Option<(u32, f32)> = None;

    let send = |e: CaptureEvent| out.send(e).is_ok();
    let mut edge = edge;

    loop {
        let mut release_now: Option<ReleaseReason> = None;
        let mut release_pos: Option<f32> = None;
        tokio::select! {
            a = activated.next() => {
                let Some(a) = a else { break };
                let position = a.cursor_position().map_or(0.5, |c| edge_position(&rects, edge, c));
                let id = a.activation_id().unwrap_or(0);
                active = Some((id, position));
                if !send(CaptureEvent::Activated { position }) { break }
            }
            d = deactivated.next() => {
                if d.is_none() { break }
                if active.take().is_some() {
                    for ev in translator.release_all() {
                        let _ = send(CaptureEvent::Input(ev));
                    }
                    if !send(CaptureEvent::Released(ReleaseReason::Desktop)) { break }
                }
            }
            d = disabled.next() => {
                if d.is_none() { break }
                trace("disabled by the desktop");
                // Not fatal: the desktop pauses capture (for example while a
                // remote-control session is open); `enable` resumes it.
                active = None;
                for ev in translator.release_all() {
                    let _ = send(CaptureEvent::Input(ev));
                }
                if !send(CaptureEvent::Disabled) { break }
            }
            z = zones_changed.next() => {
                if z.is_none() { break }
                match zones_and_barriers(&ic, &session, edge).await {
                    Ok(r) => {
                        rects = r;
                        if let Ok(mut l) = layout.lock() {
                            l.clone_from(&rects);
                        }
                    }
                    Err(_) => { let _ = send(CaptureEvent::Disabled); break }
                }
                let _ = ic.enable(&session, EnableOptions::default()).await;
            }
            cmd = rx.recv() => match cmd {
                Some(Command::Release(pos)) => {
                    release_now = Some(ReleaseReason::Requested);
                    release_pos = pos;
                }
                Some(Command::SetEdge(new_edge)) => {
                    trace("move barrier");
                    edge = new_edge;
                    match zones_and_barriers(&ic, &session, edge).await {
                        Ok(r) => {
                            rects = r;
                            if let Ok(mut l) = layout.lock() {
                                l.clone_from(&rects);
                            }
                        }
                        Err(_) => { let _ = send(CaptureEvent::Disabled); break }
                    }
                    let _ = ic.enable(&session, EnableOptions::default()).await;
                }
                Some(Command::Enable) => {
                    trace("enable");
                    let _ = ic.enable(&session, EnableOptions::default()).await;
                }
                None => break,
            },
            ev = events.next() => {
                let Some(Ok(ev)) = ev else { break };
                match ev {
                    EiEvent::SeatAdded(s) => {
                        s.seat.bind_capabilities(&[
                            DeviceCapability::Pointer,
                            DeviceCapability::Button,
                            DeviceCapability::Scroll,
                            DeviceCapability::Keyboard,
                        ]);
                        let _ = conn.flush();
                    }
                    EiEvent::Disconnected(_) => { let _ = send(CaptureEvent::Disabled); break }
                    _ if active.is_none() => {}
                    EiEvent::PointerMotion(m) => {
                        if !send(CaptureEvent::Input(InputEvent::Motion { dx: m.dx, dy: m.dy })) { break }
                    }
                    EiEvent::Button(b) => {
                        let pressed = b.state == ei::button::ButtonState::Press;
                        let code = u16::try_from(b.button).unwrap_or(0);
                        if let Some(ev) = translator.button(code, pressed) {
                            if !send(CaptureEvent::Input(ev)) { break }
                        }
                    }
                    EiEvent::ScrollDiscrete(s) => {
                        #[allow(clippy::cast_precision_loss)]
                        let ev = InputEvent::Scroll { dx: s.discrete_dx as f32 / 120.0, dy: s.discrete_dy as f32 / 120.0 };
                        if !send(CaptureEvent::Input(ev)) { break }
                    }
                    EiEvent::ScrollDelta(s) => {
                        let ev = InputEvent::Scroll { dx: s.dx / SCROLL_PX_PER_LINE, dy: s.dy / SCROLL_PX_PER_LINE };
                        if !send(CaptureEvent::Input(ev)) { break }
                    }
                    EiEvent::KeyboardKey(k) => {
                        let pressed = k.state == ei::keyboard::KeyState::Press;
                        match translator.key(u16::try_from(k.key).unwrap_or(0), pressed) {
                            KeyOutcome::Forward(ev) => if !send(CaptureEvent::Input(ev)) { break },
                            KeyOutcome::Panic => release_now = Some(ReleaseReason::PanicShortcut),
                            KeyOutcome::Drop => {}
                        }
                    }
                    _ => {}
                }
            }
        }

        if let Some(reason) = release_now {
            if let Some((id, hit)) = active.take() {
                for ev in translator.release_all() {
                    let _ = send(CaptureEvent::Input(ev));
                }
                let position = release_pos.unwrap_or(hit);
                let opts = ReleaseOptions::default()
                    .set_activation_id(id)
                    .set_cursor_position(release_point(&rects, edge, position, RELEASE_INSET));
                let _ = ic.release(&session, opts).await;
                if !send(CaptureEvent::Released(reason)) {
                    break;
                }
            }
        }
    }

    // Never leave the user without a cursor: release before tearing down.
    if let Some((id, position)) = active {
        let opts = ReleaseOptions::default()
            .set_activation_id(id)
            .set_cursor_position(release_point(&rects, edge, position, RELEASE_INSET));
        let _ = ic.release(&session, opts).await;
    }
    let _ = session.close().await;
}
