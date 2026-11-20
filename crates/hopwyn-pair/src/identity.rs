use std::fmt;

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand_core::OsRng;

/// Longest device name, in bytes.
pub const MAX_NAME_LEN: usize = 64;

/// A device's long-term identity: an Ed25519 key pair and a display name.
///
/// The secret key is zeroed when this value is dropped. Persist it only
/// through the OS key store (see `Identity::secret_bytes`).
pub struct Identity {
    signing: SigningKey,
    name: String,
}

impl Identity {
    /// Creates a new identity with a fresh key from the OS random source.
    /// Names longer than [`MAX_NAME_LEN`] bytes are cut at a character boundary.
    pub fn generate(name: &str) -> Self {
        Self {
            signing: SigningKey::generate(&mut OsRng),
            name: clamp_name(name),
        }
    }

    /// Restores an identity from a secret key read out of the OS key store.
    pub fn from_secret_bytes(secret: &[u8; 32], name: &str) -> Self {
        Self {
            signing: SigningKey::from_bytes(secret),
            name: clamp_name(name),
        }
    }

    /// The secret key, for saving to the OS key store. Never log or send it.
    pub fn secret_bytes(&self) -> [u8; 32] {
        self.signing.to_bytes()
    }

    /// The public identifier other devices pin.
    pub fn id(&self) -> DeviceId {
        DeviceId(self.signing.verifying_key().to_bytes())
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn sign(&self, msg: &[u8]) -> [u8; 64] {
        self.signing.sign(msg).to_bytes()
    }
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print the secret key.
        f.debug_struct("Identity")
            .field("id", &self.id())
            .field("name", &self.name)
            .finish()
    }
}

/// A device's public Ed25519 key, used as its stable identifier.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeviceId(pub(crate) [u8; 32]);

impl DeviceId {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Checks `signature` over `msg` against this key.
    pub(crate) fn verify(&self, msg: &[u8], signature: &[u8; 64]) -> bool {
        let Ok(key) = VerifyingKey::from_bytes(&self.0) else {
            return false;
        };
        key.verify(msg, &Signature::from_bytes(signature)).is_ok()
    }
}

impl fmt::Debug for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DeviceId({self})")
    }
}

impl fmt::Display for DeviceId {
    /// Short form for UIs and logs: the first 8 bytes in hex, in pairs.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, chunk) in self.0.chunks(2).take(4).enumerate() {
            if i > 0 {
                f.write_str("-")?;
            }
            for b in chunk {
                write!(f, "{b:02x}")?;
            }
        }
        Ok(())
    }
}

fn clamp_name(name: &str) -> String {
    if name.len() <= MAX_NAME_LEN {
        return name.to_owned();
    }
    let mut end = MAX_NAME_LEN;
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name.get(..end).unwrap_or_default().to_owned()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn secret_round_trips() {
        let a = Identity::generate("Desk");
        let b = Identity::from_secret_bytes(&a.secret_bytes(), "Desk");
        assert_eq!(a.id(), b.id());
    }

    #[test]
    fn debug_hides_secret() {
        let a = Identity::generate("Desk");
        let secret_hex: String = a
            .secret_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let shown = format!("{a:?}");
        assert!(!shown.contains(&secret_hex));
        assert!(!shown.contains(&format!("{:?}", a.secret_bytes())));
    }

    #[test]
    fn long_names_are_clamped_on_char_boundary() {
        let name = "é".repeat(40); // 80 bytes
        let id = Identity::generate(&name);
        assert!(id.name().len() <= MAX_NAME_LEN);
        assert!(id.name().chars().all(|c| c == 'é'));
    }

    #[test]
    fn signatures_verify_only_for_the_signer() {
        let a = Identity::generate("A");
        let b = Identity::generate("B");
        let sig = a.sign(b"msg");
        assert!(a.id().verify(b"msg", &sig));
        assert!(!b.id().verify(b"msg", &sig));
        assert!(!a.id().verify(b"other", &sig));
    }
}
