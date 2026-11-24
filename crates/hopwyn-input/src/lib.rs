//! Input events and OS backends for Hopwyn.
//!
//! The receiving device turns protocol messages into [`InputEvent`]s and
//! hands them to an injection backend:
//!
//! - Linux (X11 and Wayland): [`linux::LinuxEmulator`], through the
//!   RemoteDesktop portal and libei. The desktop asks the user once, and
//!   can remember the answer with a restore token.
//!
//! Keys travel as USB HID usages and are mapped to the receiver's native
//! codes here; keys without a mapping are dropped rather than guessed.

pub mod capture;
#[cfg(target_os = "linux")]
pub mod clipboard;
pub mod keymap;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
pub mod linux_capture;
#[cfg(target_os = "linux")]
mod mutter;

/// Whether this desktop lets Hopwyn share input (and the clipboard)
/// without showing a permission dialog. True on GNOME outside a sandbox;
/// elsewhere the desktop's portal asks.
#[cfg(target_os = "linux")]
pub fn no_permission_dialog() -> bool {
    mutter::available()
}

/// Turns `wayland:<exported handle>` or `x11:<hex xid>` into the window
/// the desktop's permission dialog should sit on top of.
#[cfg(target_os = "linux")]
pub(crate) fn parent_window(s: Option<&str>) -> Option<ashpd::WindowIdentifier> {
    let s = s?;
    if let Some(h) = s.strip_prefix("wayland:") {
        return (!h.is_empty())
            .then(|| ashpd::WindowIdentifier::from_xdg_foreign_exported(h.to_owned()));
    }
    let xid = std::os::raw::c_ulong::from_str_radix(s.strip_prefix("x11:")?, 16).ok()?;
    Some(ashpd::WindowIdentifier::from_xid(xid))
}

use std::fmt;

use hopwyn_proto::{Button, Message};

/// One input action to perform on this device.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputEvent {
    /// Relative pointer motion in pixels.
    Motion {
        dx: f32,
        dy: f32,
    },
    Button {
        button: Button,
        pressed: bool,
    },
    /// Scroll in lines. Positive `dy` scrolls down, positive `dx` right.
    Scroll {
        dx: f32,
        dy: f32,
    },
    /// A key by USB HID usage (page 0x07).
    Key {
        hid: u16,
        pressed: bool,
    },
}

impl InputEvent {
    /// The input action a protocol message asks for, if any.
    pub fn from_message(msg: &Message) -> Option<Self> {
        match *msg {
            Message::PointerMotion { dx, dy } => {
                // i32 -> f32 is exact for any realistic per-event motion.
                #[allow(clippy::cast_precision_loss)]
                Some(Self::Motion {
                    dx: dx as f32,
                    dy: dy as f32,
                })
            }
            Message::PointerButton { button, pressed } => Some(Self::Button { button, pressed }),
            Message::Scroll { dx, dy } => Some(Self::Scroll { dx, dy }),
            Message::Key { code, pressed } => Some(Self::Key { hid: code, pressed }),
            _ => None,
        }
    }
}

/// Why an input backend failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputError {
    /// The desktop portal refused or is missing (includes the user saying no).
    Portal(String),
    /// The libei connection failed.
    Ei(String),
    /// The backend has shut down.
    Closed,
}

impl fmt::Display for InputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Portal(e) => write!(f, "desktop portal error: {e}"),
            Self::Ei(e) => write!(f, "input emulation error: {e}"),
            Self::Closed => f.write_str("input backend has shut down"),
        }
    }
}

impl std::error::Error for InputError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_messages_convert() {
        assert_eq!(
            InputEvent::from_message(&Message::PointerMotion { dx: -3, dy: 7 }),
            Some(InputEvent::Motion { dx: -3.0, dy: 7.0 })
        );
        assert_eq!(
            InputEvent::from_message(&Message::Key {
                code: 0x04,
                pressed: true
            }),
            Some(InputEvent::Key {
                hid: 0x04,
                pressed: true
            })
        );
        assert_eq!(InputEvent::from_message(&Message::Leave), None);
        assert_eq!(
            InputEvent::from_message(&Message::Heartbeat { seq: 1 }),
            None
        );
    }
}
