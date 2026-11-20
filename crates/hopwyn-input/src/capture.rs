//! Platform-independent parts of input capture: where to put the screen-edge
//! barrier, where along the edge the cursor crossed, and turning captured
//! native events into [`InputEvent`]s while tracking what is held down.
//!
//! Safety rule: whatever happens, the user can always get their local
//! cursor back. Pressing Ctrl+Alt+Esc while captured releases capture
//! (and the key never reaches the other device).

use std::collections::BTreeSet;

use hopwyn_proto::{Button, Edge};

use crate::InputEvent;
use crate::keymap::{evdev_to_button, evdev_to_hid};

/// One monitor's area in desktop coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// A barrier line from (x1, y1) to (x2, y2), inclusive.
pub type Line = (i32, i32, i32, i32);

/// What capture reports to the caller.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CaptureEvent {
    /// The cursor hit the barrier; input now goes to the other device.
    /// `position` is where along the edge it crossed, 0.0 to 1.0
    /// (top to bottom for left/right edges, left to right for top/bottom).
    Activated { position: f32 },
    /// A captured input event to send to the other device.
    Input(InputEvent),
    /// Capture ended and the local cursor is back.
    Released(ReleaseReason),
    /// The desktop turned capture off (for example the user revoked it).
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseReason {
    /// The caller asked (the cursor came back, or the peer disconnected).
    Requested,
    /// The user pressed the panic shortcut.
    PanicShortcut,
    /// The desktop ended it.
    Desktop,
}

struct Bounds {
    min_x: i64,
    min_y: i64,
    max_x: i64,
    max_y: i64,
}

fn bounds(zones: &[Rect]) -> Option<Bounds> {
    let first = zones.first()?;
    let mut b = Bounds {
        min_x: i64::from(first.x),
        min_y: i64::from(first.y),
        max_x: i64::from(first.x) + i64::from(first.w),
        max_y: i64::from(first.y) + i64::from(first.h),
    };
    for z in zones {
        b.min_x = b.min_x.min(i64::from(z.x));
        b.min_y = b.min_y.min(i64::from(z.y));
        b.max_x = b.max_x.max(i64::from(z.x) + i64::from(z.w));
        b.max_y = b.max_y.max(i64::from(z.y) + i64::from(z.h));
    }
    Some(b)
}

fn to_i32(v: i64) -> i32 {
    i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX })
}

/// Barrier lines along the outer `edge` of the desktop: one per monitor that
/// touches that edge, so the cursor can only leave through the real edge.
pub fn edge_barriers(zones: &[Rect], edge: Edge) -> Vec<Line> {
    let Some(b) = bounds(zones) else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for z in zones.iter().filter(|z| z.w > 0 && z.h > 0) {
        let (x, y) = (i64::from(z.x), i64::from(z.y));
        let (right, bottom) = (x + i64::from(z.w), y + i64::from(z.h));
        let line = match edge {
            Edge::Right if right == b.max_x => Some((right, y, right, bottom - 1)),
            Edge::Left if x == b.min_x => Some((x, y, x, bottom - 1)),
            Edge::Top if y == b.min_y => Some((x, y, right - 1, y)),
            Edge::Bottom if bottom == b.max_y => Some((x, bottom, right - 1, bottom)),
            _ => None,
        };
        if let Some((x1, y1, x2, y2)) = line {
            lines.push((to_i32(x1), to_i32(y1), to_i32(x2), to_i32(y2)));
        }
    }
    lines
}

/// Where along `edge` the cursor at `cursor` is, from 0.0 to 1.0.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
pub fn edge_position(zones: &[Rect], edge: Edge, cursor: (f32, f32)) -> f32 {
    let Some(b) = bounds(zones) else { return 0.5 };
    let (lo, hi, v) = match edge {
        Edge::Left | Edge::Right => (b.min_y as f32, b.max_y as f32, cursor.1),
        Edge::Top | Edge::Bottom => (b.min_x as f32, b.max_x as f32, cursor.0),
    };
    if hi <= lo || !v.is_finite() {
        return 0.5;
    }
    ((v - lo) / (hi - lo)).clamp(0.0, 1.0)
}

/// Where to put the local cursor on release: `inset` pixels inside `edge`
/// at `position` along it, so it doesn't immediately hit the barrier again.
#[allow(clippy::cast_precision_loss)]
pub fn release_point(zones: &[Rect], edge: Edge, position: f32, inset: f64) -> Option<(f64, f64)> {
    let b = bounds(zones)?;
    let p = f64::from(position.clamp(0.0, 1.0));
    let along_y = b.min_y as f64 + p * (b.max_y - b.min_y - 1).max(0) as f64;
    let along_x = b.min_x as f64 + p * (b.max_x - b.min_x - 1).max(0) as f64;
    Some(match edge {
        Edge::Right => (b.max_x as f64 - 1.0 - inset, along_y),
        Edge::Left => (b.min_x as f64 + inset, along_y),
        Edge::Top => (along_x, b.min_y as f64 + inset),
        Edge::Bottom => (along_x, b.max_y as f64 - 1.0 - inset),
    })
}

/// What to do with one captured native key.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum KeyOutcome {
    Forward(InputEvent),
    /// The panic shortcut: release capture, don't forward.
    Panic,
    /// No mapping, or a release for a key we never forwarded.
    Drop,
}

const EV_ESC: u16 = 1;
const EV_LEFTCTRL: u16 = 29;
const EV_RIGHTCTRL: u16 = 97;
const EV_LEFTALT: u16 = 56;
const EV_RIGHTALT: u16 = 100;

/// Translates captured evdev events and remembers what is held, so capture
/// can end without leaving keys stuck down on the other device.
#[derive(Debug, Default)]
pub struct Translator {
    held_keys: BTreeSet<u16>,
    held_buttons: Vec<Button>,
    local_down: BTreeSet<u16>,
}

impl Translator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn key(&mut self, evdev: u16, pressed: bool) -> KeyOutcome {
        if pressed {
            self.local_down.insert(evdev);
        } else {
            self.local_down.remove(&evdev);
        }
        let ctrl =
            self.local_down.contains(&EV_LEFTCTRL) || self.local_down.contains(&EV_RIGHTCTRL);
        let alt = self.local_down.contains(&EV_LEFTALT) || self.local_down.contains(&EV_RIGHTALT);
        if pressed && evdev == EV_ESC && ctrl && alt {
            return KeyOutcome::Panic;
        }
        let Some(hid) = evdev_to_hid(evdev) else {
            return KeyOutcome::Drop;
        };
        if pressed {
            self.held_keys.insert(hid);
        } else if !self.held_keys.remove(&hid) {
            return KeyOutcome::Drop;
        }
        KeyOutcome::Forward(InputEvent::Key { hid, pressed })
    }

    pub fn button(&mut self, evdev: u16, pressed: bool) -> Option<InputEvent> {
        let button = evdev_to_button(evdev)?;
        if pressed {
            if !self.held_buttons.contains(&button) {
                self.held_buttons.push(button);
            }
        } else {
            let before = self.held_buttons.len();
            self.held_buttons.retain(|b| *b != button);
            if self.held_buttons.len() == before {
                return None;
            }
        }
        Some(InputEvent::Button { button, pressed })
    }

    /// Release events for everything still held. Call when capture ends,
    /// and send them to the other device.
    pub fn release_all(&mut self) -> Vec<InputEvent> {
        let mut out: Vec<InputEvent> = std::mem::take(&mut self.held_buttons)
            .into_iter()
            .map(|button| InputEvent::Button {
                button,
                pressed: false,
            })
            .collect();
        out.extend(
            std::mem::take(&mut self.held_keys)
                .into_iter()
                .map(|hid| InputEvent::Key {
                    hid,
                    pressed: false,
                }),
        );
        self.local_down.clear();
        out
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    const ONE: [Rect; 1] = [Rect {
        x: 0,
        y: 0,
        w: 1920,
        h: 1080,
    }];
    // Two monitors side by side, the right one taller and lower.
    const TWO: [Rect; 2] = [
        Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        },
        Rect {
            x: 1920,
            y: 100,
            w: 1280,
            h: 1024,
        },
    ];

    #[test]
    fn single_monitor_barriers() {
        assert_eq!(
            edge_barriers(&ONE, Edge::Right),
            vec![(1920, 0, 1920, 1079)]
        );
        assert_eq!(edge_barriers(&ONE, Edge::Left), vec![(0, 0, 0, 1079)]);
        assert_eq!(edge_barriers(&ONE, Edge::Top), vec![(0, 0, 1919, 0)]);
        assert_eq!(
            edge_barriers(&ONE, Edge::Bottom),
            vec![(0, 1080, 1919, 1080)]
        );
    }

    #[test]
    fn only_outer_monitors_get_barriers() {
        assert_eq!(
            edge_barriers(&TWO, Edge::Right),
            vec![(3200, 100, 3200, 1123)]
        );
        assert_eq!(edge_barriers(&TWO, Edge::Left), vec![(0, 0, 0, 1079)]);
        assert_eq!(
            edge_barriers(&TWO, Edge::Bottom),
            vec![(1920, 1124, 3199, 1124)]
        );
        assert!(edge_barriers(&[], Edge::Right).is_empty());
    }

    #[test]
    fn edge_position_is_relative_and_clamped() {
        assert_eq!(edge_position(&ONE, Edge::Right, (1919.0, 540.0)), 0.5);
        assert_eq!(edge_position(&ONE, Edge::Top, (0.0, 0.0)), 0.0);
        assert_eq!(edge_position(&ONE, Edge::Left, (0.0, 5000.0)), 1.0);
        assert_eq!(edge_position(&ONE, Edge::Left, (0.0, f32::NAN)), 0.5);
    }

    #[test]
    fn release_point_is_inside_the_edge() {
        let (x, y) = release_point(&ONE, Edge::Right, 0.5, 2.0).unwrap();
        assert!(x < 1919.0 && x > 1900.0);
        assert!((y - 539.5).abs() < 1.0);
        let (x, _) = release_point(&ONE, Edge::Left, 0.0, 2.0).unwrap();
        assert_eq!(x, 2.0);
        assert!(release_point(&[], Edge::Left, 0.0, 2.0).is_none());
    }

    #[test]
    fn keys_forward_and_track() {
        let mut t = Translator::new();
        assert_eq!(
            t.key(30, true),
            KeyOutcome::Forward(InputEvent::Key {
                hid: 0x04,
                pressed: true
            })
        );
        assert_eq!(
            t.key(30, false),
            KeyOutcome::Forward(InputEvent::Key {
                hid: 0x04,
                pressed: false
            })
        );
        // A release for a key pressed before capture started is not forwarded.
        assert_eq!(t.key(30, false), KeyOutcome::Drop);
        assert_eq!(t.key(9999, true), KeyOutcome::Drop);
    }

    #[test]
    fn panic_shortcut_releases_and_is_not_forwarded() {
        let mut t = Translator::new();
        assert!(matches!(t.key(EV_LEFTCTRL, true), KeyOutcome::Forward(_)));
        assert!(matches!(t.key(EV_RIGHTALT, true), KeyOutcome::Forward(_)));
        assert_eq!(t.key(EV_ESC, true), KeyOutcome::Panic);
        // Esc alone is a normal key.
        let mut t = Translator::new();
        assert!(matches!(t.key(EV_ESC, true), KeyOutcome::Forward(_)));
    }

    #[test]
    fn release_all_lifts_everything_held() {
        let mut t = Translator::new();
        let _ = t.key(EV_LEFTCTRL, true);
        let _ = t.key(30, true);
        assert!(t.button(0x110, true).is_some());
        assert_eq!(
            t.button(0x110, true),
            Some(InputEvent::Button {
                button: Button::Left,
                pressed: true
            })
        );
        let ups = t.release_all();
        assert_eq!(
            ups,
            vec![
                InputEvent::Button {
                    button: Button::Left,
                    pressed: false
                },
                InputEvent::Key {
                    hid: 0x04,
                    pressed: false
                },
                InputEvent::Key {
                    hid: 0xe0,
                    pressed: false
                },
            ]
        );
        assert!(t.release_all().is_empty());
        assert_eq!(t.button(0x110, false), None);
    }
}
