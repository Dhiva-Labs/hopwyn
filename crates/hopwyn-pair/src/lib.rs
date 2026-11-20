//! Device identity and pairing for Hopwyn.
//!
//! Pairing turns a 6-digit code into long-term trust between two devices:
//!
//! 1. The device with "Add device" open (the *initiator*) shows a random
//!    6-digit code. The user types it on the other device (the *joiner*).
//! 2. Both run SPAKE2 keyed by the code. An eavesdropper learns nothing it
//!    can brute-force offline, and an active attacker gets one guess per run.
//! 3. Each side sends a confirmation: its Ed25519 identity key, its name, a
//!    signature over the session transcript, and a MAC under a key derived
//!    from the SPAKE2 secret. Only a peer that knew the code can produce a
//!    valid MAC, and only the holder of the identity key can sign.
//! 4. Each side stores the other's identity key. Later connections pin it.
//!
//! The state machines here do no I/O: the caller moves bytes between peers.
//! Use [`PairingLimiter`] to enforce the pairing window and lockout.

mod identity;
mod limiter;
mod session;

pub use identity::{DeviceId, Identity, MAX_NAME_LEN};
pub use limiter::{LOCKOUT, LimiterError, MAX_FAILURES, PairingLimiter, WINDOW};
pub use session::{AwaitingConfirm, PairError, PairedPeer, Pairing, PairingCode, Role};
