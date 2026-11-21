//! Hopwyn wire protocol.
//!
//! Every message travels inside an already encrypted and authenticated
//! transport stream, but the decoder still treats its input as hostile:
//! every length is bounded, every float is checked, trailing bytes are
//! rejected, and no input can make it panic or allocate without limit.
//!
//! Layout of one encoded message: a one-byte tag, then the fields of that
//! variant in order. Integers are little-endian. Strings are a `u32`
//! length followed by UTF-8 bytes.

mod codec;

pub use codec::DecodeError;

/// Version of this wire format. Peers with a different version refuse to talk.
pub const PROTOCOL_VERSION: u16 = 1;

/// Longest device name accepted, in bytes.
pub const MAX_DEVICE_NAME: usize = 64;

/// Largest clipboard text accepted in one message, in bytes (1 MiB).
pub const MAX_CLIPBOARD_TEXT: usize = 1024 * 1024;

/// A screen edge, used when the cursor crosses from one device to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

/// A mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

/// One message between two paired devices.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// First message on a connection: protocol version and a display name.
    Hello { version: u16, device_name: String },
    /// The cursor enters the receiver at `edge`, `position` along it (0.0 to 1.0).
    /// The receiver becomes active.
    Enter { edge: Edge, position: f32 },
    /// The cursor left the receiver. The receiver becomes inactive.
    Leave,
    /// Like `Leave`, saying where the cursor came back: `position` along
    /// the receiver's own shared edge (0.0 to 1.0), so it reappears at the
    /// matching spot.
    LeaveAt { position: f32 },
    /// Relative pointer motion in device pixels.
    PointerMotion { dx: i32, dy: i32 },
    /// A mouse button changed state.
    PointerButton { button: Button, pressed: bool },
    /// Scroll, in lines (fractional for smooth scrolling).
    Scroll { dx: f32, dy: f32 },
    /// A key changed state. `code` is a USB HID usage (page 0x07), so the
    /// receiver maps it with its own keyboard layout.
    Key { code: u16, pressed: bool },
    /// New clipboard text from the sender.
    ClipboardText { text: String },
    /// The sender's whole desktop size in logical pixels, for drawing the
    /// layout to scale and mapping positions between unequal screens.
    Screen { width: u32, height: u32 },
    /// Like `Layout`, with the receiver's screen shifted `offset` logical
    /// pixels along the sender's edge (from its top or left).
    Placement { edge: Edge, offset: i32 },
    /// Layout: the receiver's screen sits beyond the sender's `edge`, so the
    /// receiver places the sender beyond the opposite edge. Keeps both
    /// computers' arrangement in step when it changes on either.
    Layout { edge: Edge },
    /// Keep-alive. Three missed heartbeats end the handoff.
    Heartbeat { seq: u32 },
    /// Orderly shutdown of the connection.
    Disconnect,
}

impl Message {
    /// Appends the encoding of `self` to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        codec::encode(self, out);
    }

    /// Encodes `self` into a new buffer.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    /// Decodes exactly one message from `bytes`. Trailing bytes are an error.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        codec::decode(bytes)
    }
}
