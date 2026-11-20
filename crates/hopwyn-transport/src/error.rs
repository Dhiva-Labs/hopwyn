use std::fmt;
use std::net::SocketAddr;

use hopwyn_proto::DecodeError;

/// Why a transport operation failed.
#[derive(Debug)]
pub enum TransportError {
    /// The address is not on a home network, so Hopwyn refuses it.
    NotHomeNetwork(SocketAddr),
    /// Building the TLS or QUIC configuration failed.
    Config(String),
    /// Binding the UDP socket failed.
    Bind(std::io::Error),
    /// The connection could not be set up (includes an untrusted peer key).
    Connect(String),
    /// The connection or stream broke.
    Connection(String),
    /// The peer's certificate did not carry exactly one Ed25519 key.
    NoPeerIdentity,
    /// The peer spoke first with something other than `Hello`, or used
    /// another protocol version.
    Handshake(&'static str),
    /// A frame was larger than [`crate::MAX_FRAME`].
    FrameTooLarge(usize),
    /// A frame did not decode to a valid message.
    Decode(DecodeError),
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotHomeNetwork(a) => write!(f, "{a} is not a home-network address"),
            Self::Config(e) => write!(f, "transport configuration failed: {e}"),
            Self::Bind(e) => write!(f, "could not bind socket: {e}"),
            Self::Connect(e) => write!(f, "could not connect: {e}"),
            Self::Connection(e) => write!(f, "connection error: {e}"),
            Self::NoPeerIdentity => f.write_str("peer did not present a device key"),
            Self::Handshake(why) => write!(f, "handshake failed: {why}"),
            Self::FrameTooLarge(n) => write!(f, "frame of {n} bytes exceeds the limit"),
            Self::Decode(e) => write!(f, "invalid message: {e}"),
        }
    }
}

impl std::error::Error for TransportError {}

impl From<DecodeError> for TransportError {
    fn from(e: DecodeError) -> Self {
        Self::Decode(e)
    }
}
