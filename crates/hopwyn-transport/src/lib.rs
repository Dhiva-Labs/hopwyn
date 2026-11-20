//! Encrypted transport between paired Hopwyn devices.
//!
//! - QUIC with TLS 1.3 only (rustls, ring backend). No other version or
//!   plaintext mode exists.
//! - Mutual authentication by pinned Ed25519 device keys: each side presents
//!   a self-signed certificate for its identity key and accepts the peer
//!   only if that key is in its [`TrustStore`]. Certificate chains, names
//!   and expiry are irrelevant; the key is the identity.
//! - Home network only: connections to or from public IP addresses are
//!   refused before any handshake.
//! - Messages are length-framed [`hopwyn_proto::Message`]s on one
//!   bidirectional stream, with a bounded frame size.

mod cert;
mod error;
mod netpolicy;
mod node;
mod trust;
mod verify;

pub use error::TransportError;
pub use netpolicy::is_home_address;
pub use node::{
    IDLE_TIMEOUT, KEEP_ALIVE, MAX_FRAME, Node, PeerConnection, PeerReceiver, PeerSender,
};
pub use trust::TrustStore;
