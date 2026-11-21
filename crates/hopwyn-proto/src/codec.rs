use std::fmt;

use crate::{Button, Edge, MAX_CLIPBOARD_TEXT, MAX_DEVICE_NAME, Message};

mod tag {
    pub const HELLO: u8 = 0x01;
    pub const ENTER: u8 = 0x02;
    pub const LEAVE: u8 = 0x03;
    pub const POINTER_MOTION: u8 = 0x10;
    pub const POINTER_BUTTON: u8 = 0x11;
    pub const SCROLL: u8 = 0x12;
    pub const KEY: u8 = 0x20;
    pub const CLIPBOARD_TEXT: u8 = 0x30;
    pub const LAYOUT: u8 = 0x40;
    pub const SCREEN: u8 = 0x41;
    pub const PLACEMENT: u8 = 0x42;
    pub const LEAVE_AT: u8 = 0x04;
    pub const HEARTBEAT: u8 = 0x70;
    pub const DISCONNECT: u8 = 0x7f;
}

/// Why a byte sequence is not a valid message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// The input ended before the message did.
    Truncated,
    /// Bytes remained after a complete message.
    TrailingBytes,
    /// The first byte is not a known message tag.
    UnknownTag(u8),
    /// A field held a value outside its allowed set.
    InvalidValue(&'static str),
    /// A string was longer than its limit.
    TooLong {
        field: &'static str,
        len: usize,
        max: usize,
    },
    /// A string was not valid UTF-8.
    InvalidUtf8,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => f.write_str("message is truncated"),
            Self::TrailingBytes => f.write_str("unexpected bytes after message"),
            Self::UnknownTag(t) => write!(f, "unknown message tag 0x{t:02x}"),
            Self::InvalidValue(field) => write!(f, "invalid value for {field}"),
            Self::TooLong { field, len, max } => {
                write!(f, "{field} is {len} bytes, limit is {max}")
            }
            Self::InvalidUtf8 => f.write_str("string is not valid UTF-8"),
        }
    }
}

impl std::error::Error for DecodeError {}

pub(crate) fn encode(msg: &Message, out: &mut Vec<u8>) {
    match msg {
        Message::Hello {
            version,
            device_name,
        } => {
            out.push(tag::HELLO);
            out.extend_from_slice(&version.to_le_bytes());
            put_str(out, device_name);
        }
        Message::Enter { edge, position } => {
            out.push(tag::ENTER);
            out.push(edge_to_u8(*edge));
            out.extend_from_slice(&position.to_le_bytes());
        }
        Message::Leave => out.push(tag::LEAVE),
        Message::PointerMotion { dx, dy } => {
            out.push(tag::POINTER_MOTION);
            out.extend_from_slice(&dx.to_le_bytes());
            out.extend_from_slice(&dy.to_le_bytes());
        }
        Message::PointerButton { button, pressed } => {
            out.push(tag::POINTER_BUTTON);
            out.push(button_to_u8(*button));
            out.push(u8::from(*pressed));
        }
        Message::Scroll { dx, dy } => {
            out.push(tag::SCROLL);
            out.extend_from_slice(&dx.to_le_bytes());
            out.extend_from_slice(&dy.to_le_bytes());
        }
        Message::Key { code, pressed } => {
            out.push(tag::KEY);
            out.extend_from_slice(&code.to_le_bytes());
            out.push(u8::from(*pressed));
        }
        Message::ClipboardText { text } => {
            out.push(tag::CLIPBOARD_TEXT);
            put_str(out, text);
        }
        Message::LeaveAt { position } => {
            out.push(tag::LEAVE_AT);
            out.extend_from_slice(&position.to_le_bytes());
        }
        Message::Screen { width, height } => {
            out.push(tag::SCREEN);
            out.extend_from_slice(&width.to_le_bytes());
            out.extend_from_slice(&height.to_le_bytes());
        }
        Message::Placement { edge, offset } => {
            out.push(tag::PLACEMENT);
            out.push(edge_to_u8(*edge));
            out.extend_from_slice(&offset.to_le_bytes());
        }
        Message::Layout { edge } => {
            out.push(tag::LAYOUT);
            out.push(edge_to_u8(*edge));
        }
        Message::Heartbeat { seq } => {
            out.push(tag::HEARTBEAT);
            out.extend_from_slice(&seq.to_le_bytes());
        }
        Message::Disconnect => out.push(tag::DISCONNECT),
    }
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Message, DecodeError> {
    let mut r = Reader { rest: bytes };
    let msg = match r.u8()? {
        tag::HELLO => Message::Hello {
            version: r.u16()?,
            device_name: r.string("device_name", MAX_DEVICE_NAME)?,
        },
        tag::ENTER => {
            let edge = edge_from_u8(r.u8()?)?;
            let position = r.f32("position")?;
            if !(0.0..=1.0).contains(&position) {
                return Err(DecodeError::InvalidValue("position"));
            }
            Message::Enter { edge, position }
        }
        tag::LEAVE => Message::Leave,
        tag::POINTER_MOTION => Message::PointerMotion {
            dx: r.i32()?,
            dy: r.i32()?,
        },
        tag::POINTER_BUTTON => Message::PointerButton {
            button: button_from_u8(r.u8()?)?,
            pressed: r.bool("pressed")?,
        },
        tag::SCROLL => Message::Scroll {
            dx: r.f32("scroll dx")?,
            dy: r.f32("scroll dy")?,
        },
        tag::KEY => Message::Key {
            code: r.u16()?,
            pressed: r.bool("pressed")?,
        },
        tag::CLIPBOARD_TEXT => Message::ClipboardText {
            text: r.string("clipboard text", MAX_CLIPBOARD_TEXT)?,
        },
        tag::LEAVE_AT => {
            let position = r.f32("position")?;
            if !(0.0..=1.0).contains(&position) {
                return Err(DecodeError::InvalidValue("position"));
            }
            Message::LeaveAt { position }
        }
        tag::SCREEN => {
            let (width, height) = (r.u32()?, r.u32()?);
            // Bounded so layout arithmetic can't overflow.
            if width == 0 || height == 0 || width > 65_536 || height > 65_536 {
                return Err(DecodeError::InvalidValue("screen size"));
            }
            Message::Screen { width, height }
        }
        tag::PLACEMENT => {
            let edge = edge_from_u8(r.u8()?)?;
            let offset = r.i32()?;
            if offset.unsigned_abs() > 65_536 {
                return Err(DecodeError::InvalidValue("offset"));
            }
            Message::Placement { edge, offset }
        }
        tag::LAYOUT => Message::Layout {
            edge: edge_from_u8(r.u8()?)?,
        },
        tag::HEARTBEAT => Message::Heartbeat { seq: r.u32()? },
        tag::DISCONNECT => Message::Disconnect,
        other => return Err(DecodeError::UnknownTag(other)),
    };
    if !r.rest.is_empty() {
        return Err(DecodeError::TrailingBytes);
    }
    Ok(msg)
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    // Encoding never truncates silently: strings over u32::MAX bytes cannot
    // exist in practice, and the decoder enforces the real per-field limits.
    let len = u32::try_from(s.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let (head, tail) = self
            .rest
            .split_first_chunk::<N>()
            .ok_or(DecodeError::Truncated)?;
        self.rest = tail;
        Ok(*head)
    }

    fn bytes(&mut self, len: usize) -> Result<&'a [u8], DecodeError> {
        let (head, tail) = self
            .rest
            .split_at_checked(len)
            .ok_or(DecodeError::Truncated)?;
        self.rest = tail;
        Ok(head)
    }

    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(u8::from_le_bytes(self.take()?))
    }

    fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_le_bytes(self.take()?))
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(self.take()?))
    }

    fn i32(&mut self) -> Result<i32, DecodeError> {
        Ok(i32::from_le_bytes(self.take()?))
    }

    fn f32(&mut self, field: &'static str) -> Result<f32, DecodeError> {
        let v = f32::from_le_bytes(self.take()?);
        if v.is_finite() {
            Ok(v)
        } else {
            Err(DecodeError::InvalidValue(field))
        }
    }

    fn bool(&mut self, field: &'static str) -> Result<bool, DecodeError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(DecodeError::InvalidValue(field)),
        }
    }

    fn string(&mut self, field: &'static str, max: usize) -> Result<String, DecodeError> {
        let len = usize::try_from(self.u32()?).unwrap_or(usize::MAX);
        // Check the limit before touching the payload, so a forged length
        // never drives an allocation.
        if len > max {
            return Err(DecodeError::TooLong { field, len, max });
        }
        let raw = self.bytes(len)?;
        std::str::from_utf8(raw)
            .map(str::to_owned)
            .map_err(|_| DecodeError::InvalidUtf8)
    }
}

fn edge_to_u8(e: Edge) -> u8 {
    match e {
        Edge::Left => 0,
        Edge::Right => 1,
        Edge::Top => 2,
        Edge::Bottom => 3,
    }
}

fn edge_from_u8(v: u8) -> Result<Edge, DecodeError> {
    match v {
        0 => Ok(Edge::Left),
        1 => Ok(Edge::Right),
        2 => Ok(Edge::Top),
        3 => Ok(Edge::Bottom),
        _ => Err(DecodeError::InvalidValue("edge")),
    }
}

fn button_to_u8(b: Button) -> u8 {
    match b {
        Button::Left => 0,
        Button::Right => 1,
        Button::Middle => 2,
        Button::Back => 3,
        Button::Forward => 4,
    }
}

fn button_from_u8(v: u8) -> Result<Button, DecodeError> {
    match v {
        0 => Ok(Button::Left),
        1 => Ok(Button::Right),
        2 => Ok(Button::Middle),
        3 => Ok(Button::Back),
        4 => Ok(Button::Forward),
        _ => Err(DecodeError::InvalidValue("button")),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::PROTOCOL_VERSION;

    fn all_messages() -> Vec<Message> {
        vec![
            Message::Hello {
                version: PROTOCOL_VERSION,
                device_name: "Office-PC".into(),
            },
            Message::Enter {
                edge: Edge::Left,
                position: 0.0,
            },
            Message::Enter {
                edge: Edge::Bottom,
                position: 1.0,
            },
            Message::Leave,
            Message::PointerMotion {
                dx: -12,
                dy: i32::MAX,
            },
            Message::PointerButton {
                button: Button::Forward,
                pressed: true,
            },
            Message::Scroll { dx: 0.0, dy: -2.5 },
            Message::Key {
                code: 0x04,
                pressed: false,
            },
            Message::ClipboardText {
                text: "héllo ✓".into(),
            },
            Message::ClipboardText {
                text: String::new(),
            },
            Message::Heartbeat { seq: 42 },
            Message::Layout { edge: Edge::Top },
            Message::LeaveAt { position: 0.25 },
            Message::Screen {
                width: 2560,
                height: 1440,
            },
            Message::Placement {
                edge: Edge::Left,
                offset: -300,
            },
            Message::Disconnect,
        ]
    }

    #[test]
    fn every_message_round_trips() {
        for msg in all_messages() {
            assert_eq!(Message::decode(&msg.to_bytes()), Ok(msg));
        }
    }

    #[test]
    fn every_truncation_is_rejected() {
        for msg in all_messages() {
            let bytes = msg.to_bytes();
            for cut in 0..bytes.len() {
                assert_eq!(
                    Message::decode(&bytes[..cut]),
                    Err(DecodeError::Truncated),
                    "{msg:?} cut at {cut}"
                );
            }
        }
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut bytes = Message::Leave.to_bytes();
        bytes.push(0);
        assert_eq!(Message::decode(&bytes), Err(DecodeError::TrailingBytes));
    }

    #[test]
    fn unknown_tag_is_rejected() {
        assert_eq!(Message::decode(&[0xee]), Err(DecodeError::UnknownTag(0xee)));
    }

    #[test]
    fn non_finite_floats_are_rejected() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let bytes = Message::Scroll { dx: bad, dy: 0.0 }.to_bytes();
            assert_eq!(
                Message::decode(&bytes),
                Err(DecodeError::InvalidValue("scroll dx"))
            );
        }
    }

    #[test]
    fn enter_position_out_of_range_is_rejected() {
        let bytes = Message::Enter {
            edge: Edge::Top,
            position: 1.5,
        }
        .to_bytes();
        assert_eq!(
            Message::decode(&bytes),
            Err(DecodeError::InvalidValue("position"))
        );
    }

    #[test]
    fn invalid_enum_and_bool_bytes_are_rejected() {
        assert_eq!(
            Message::decode(&[tag::POINTER_BUTTON, 9, 0]),
            Err(DecodeError::InvalidValue("button"))
        );
        assert_eq!(
            Message::decode(&[tag::POINTER_BUTTON, 0, 2]),
            Err(DecodeError::InvalidValue("pressed"))
        );
        let mut enter = vec![tag::ENTER, 7];
        enter.extend_from_slice(&0.5f32.to_le_bytes());
        assert_eq!(
            Message::decode(&enter),
            Err(DecodeError::InvalidValue("edge"))
        );
    }

    #[test]
    fn forged_huge_length_is_rejected_before_allocating() {
        let mut bytes = vec![tag::CLIPBOARD_TEXT];
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            Message::decode(&bytes),
            Err(DecodeError::TooLong {
                field: "clipboard text",
                ..
            })
        ));
    }

    #[test]
    fn device_name_over_limit_is_rejected() {
        let name = "x".repeat(MAX_DEVICE_NAME + 1);
        let bytes = Message::Hello {
            version: 1,
            device_name: name,
        }
        .to_bytes();
        assert!(matches!(
            Message::decode(&bytes),
            Err(DecodeError::TooLong {
                field: "device_name",
                ..
            })
        ));
    }

    #[test]
    fn invalid_utf8_is_rejected() {
        let mut bytes = vec![tag::CLIPBOARD_TEXT];
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&[0xff, 0xfe]);
        assert_eq!(Message::decode(&bytes), Err(DecodeError::InvalidUtf8));
    }

    #[test]
    fn random_input_never_panics() {
        // Small deterministic generator so the test needs no dependencies.
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..200_000 {
            let len = (next() % 24) as usize;
            let bytes: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            let _ = Message::decode(&bytes);
        }
    }
}
