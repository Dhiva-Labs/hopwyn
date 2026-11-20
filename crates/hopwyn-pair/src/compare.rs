//! Pairing by comparing a code shown on both screens (numeric comparison,
//! as in Bluetooth), so nothing has to be typed.
//!
//! The *host* clicked "Add computer"; the *guest* is the other device.
//!
//! 1. Guest → host: its identity key, name and a commitment to a random
//!    nonce `ng` (a hash of it).
//! 2. Host → guest: its identity key, name and a random nonce `nh`.
//! 3. Guest → host: `ng`. The host checks it against the commitment.
//! 4. Both derive a 6-digit code from both keys and both nonces and show
//!    it. The user checks the two screens match and clicks Pair on each.
//! 5. Each side sends a signature over the whole exchange with its identity
//!    key, and checks the other's.
//!
//! Why it is safe: a man in the middle would have to make two different
//! exchanges produce the same code. The commitment fixes the guest's nonce
//! before the host's is known, so the attacker cannot search for a match;
//! one attempt succeeds with probability 1 in 1,000,000, and the user sees
//! a mismatch otherwise. The signatures prove each side holds the key it
//! claimed. The state machines do no I/O.

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};

use crate::identity::{DeviceId, Identity, MAX_NAME_LEN};
use crate::session::{PairError, PairedPeer};

const TAG_GUEST_HELLO: u8 = 1;
const TAG_HOST_HELLO: u8 = 2;
const TAG_REVEAL: u8 = 3;
const TAG_CONFIRM: u8 = 4;
const TAG_CANCEL: u8 = 5;

/// What a confirmation-step message says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// The other side's user clicked Pair; its signature checked out.
    Confirmed,
    /// The other side's user cancelled, or the codes didn't match.
    Cancelled,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Host,
    Guest,
}

impl Side {
    fn label(self) -> &'static [u8] {
        match self {
            Side::Host => b"hopwyn compare confirm H",
            Side::Guest => b"hopwyn compare confirm G",
        }
    }

    fn other(self) -> Side {
        match self {
            Side::Host => Side::Guest,
            Side::Guest => Side::Host,
        }
    }
}

fn nonce() -> [u8; 32] {
    let mut n = [0u8; 32];
    OsRng.fill_bytes(&mut n);
    n
}

fn commitment(key: &[u8; 32], nonce: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"hopwyn compare commit v1");
    h.update(key);
    h.update(nonce);
    h.finalize().into()
}

fn encode_hello(tag: u8, key: &[u8; 32], extra: &[u8; 32], name: &str) -> Vec<u8> {
    let name = name.as_bytes();
    let len = u8::try_from(name.len().min(MAX_NAME_LEN)).unwrap_or(0);
    let mut out = Vec::with_capacity(66 + name.len());
    out.push(tag);
    out.extend_from_slice(key);
    out.extend_from_slice(extra);
    out.push(len);
    out.extend_from_slice(name.get(..usize::from(len)).unwrap_or_default());
    out
}

/// Parses `[tag][key 32][extra 32][name len][name]`.
fn decode_hello(tag: u8, msg: &[u8]) -> Result<([u8; 32], [u8; 32], String), PairError> {
    let (&t, rest) = msg.split_first().ok_or(PairError::Malformed)?;
    if t != tag {
        return Err(PairError::Malformed);
    }
    let (key, rest) = rest.split_first_chunk::<32>().ok_or(PairError::Malformed)?;
    let (extra, rest) = rest.split_first_chunk::<32>().ok_or(PairError::Malformed)?;
    let (&len, name) = rest.split_first().ok_or(PairError::Malformed)?;
    if usize::from(len) != name.len() || name.len() > MAX_NAME_LEN {
        return Err(PairError::Malformed);
    }
    let name = std::str::from_utf8(name).map_err(|_| PairError::Malformed)?;
    // The key must be a valid Ed25519 point, or no signature can ever verify.
    VerifyingKey::from_bytes(key).map_err(|_| PairError::Malformed)?;
    Ok((*key, *extra, name.to_owned()))
}

/// The guest's side after sending its hello.
pub struct Guest<'a> {
    identity: &'a Identity,
    nonce: [u8; 32],
}

impl<'a> Guest<'a> {
    /// Starts pairing as the guest. Send the returned message to the host.
    pub fn start(identity: &'a Identity) -> (Self, Vec<u8>) {
        let n = nonce();
        let key = *identity.id().as_bytes();
        let msg = encode_hello(
            TAG_GUEST_HELLO,
            &key,
            &commitment(&key, &n),
            identity.name(),
        );
        (Self { identity, nonce: n }, msg)
    }

    /// Takes the host's hello. Returns the comparison (show its code) and
    /// the reveal message to send to the host.
    pub fn receive_host_hello(self, msg: &[u8]) -> Result<(Compared, Vec<u8>), PairError> {
        let (host_key, host_nonce, host_name) = decode_hello(TAG_HOST_HELLO, msg)?;
        let own = *self.identity.id().as_bytes();
        if host_key == own {
            return Err(PairError::Reflected);
        }
        let compared = Compared::new(
            Side::Guest,
            self.identity,
            Keys {
                host: (host_key, host_name),
                guest: (own, self.identity.name().to_owned()),
                host_nonce,
                guest_nonce: self.nonce,
            },
        );
        let mut reveal = Vec::with_capacity(33);
        reveal.push(TAG_REVEAL);
        reveal.extend_from_slice(&self.nonce);
        Ok((compared, reveal))
    }
}

/// The host's side after answering the guest's hello.
pub struct Host<'a> {
    identity: &'a Identity,
    nonce: [u8; 32],
    guest_key: [u8; 32],
    guest_name: String,
    guest_commitment: [u8; 32],
}

impl<'a> Host<'a> {
    /// Takes the guest's hello. Send the returned message back.
    pub fn receive_guest_hello(
        identity: &'a Identity,
        msg: &[u8],
    ) -> Result<(Self, Vec<u8>), PairError> {
        let (guest_key, guest_commitment, guest_name) = decode_hello(TAG_GUEST_HELLO, msg)?;
        let own = *identity.id().as_bytes();
        if guest_key == own {
            return Err(PairError::Reflected);
        }
        let n = nonce();
        let reply = encode_hello(TAG_HOST_HELLO, &own, &n, identity.name());
        Ok((
            Self {
                identity,
                nonce: n,
                guest_key,
                guest_name,
                guest_commitment,
            },
            reply,
        ))
    }

    /// Takes the guest's revealed nonce and checks it against the commitment.
    pub fn receive_reveal(self, msg: &[u8]) -> Result<Compared, PairError> {
        let (&tag, rest) = msg.split_first().ok_or(PairError::Malformed)?;
        let guest_nonce: [u8; 32] = rest.try_into().map_err(|_| PairError::Malformed)?;
        if tag != TAG_REVEAL {
            return Err(PairError::Malformed);
        }
        if commitment(&self.guest_key, &guest_nonce) != self.guest_commitment {
            return Err(PairError::WrongCodeOrTampered);
        }
        Ok(Compared::new(
            Side::Host,
            self.identity,
            Keys {
                host: (
                    *self.identity.id().as_bytes(),
                    self.identity.name().to_owned(),
                ),
                guest: (self.guest_key, self.guest_name),
                host_nonce: self.nonce,
                guest_nonce,
            },
        ))
    }
}

struct Keys {
    host: ([u8; 32], String),
    guest: ([u8; 32], String),
    host_nonce: [u8; 32],
    guest_nonce: [u8; 32],
}

/// Both sides have everything: show [`Compared::code`] and ask the user.
pub struct Compared {
    side: Side,
    code: String,
    transcript: [u8; 32],
    peer_key: [u8; 32],
    peer: PairedPeer,
    confirm: Vec<u8>,
}

impl Compared {
    fn new(side: Side, identity: &Identity, k: Keys) -> Self {
        let mut c = Sha256::new();
        c.update(b"hopwyn compare code v1");
        c.update(k.host.0);
        c.update(k.guest.0);
        c.update(k.host_nonce);
        c.update(k.guest_nonce);
        let digest: [u8; 32] = c.finalize().into();
        let mut first = [0u8; 8];
        first.copy_from_slice(digest.get(..8).unwrap_or(&[0; 8]));
        let code = format!("{:06}", u64::from_be_bytes(first) % 1_000_000);

        let mut t = Sha256::new();
        t.update(b"hopwyn compare transcript v1");
        for (key, name) in [&k.host, &k.guest] {
            t.update(key);
            t.update([u8::try_from(name.len()).unwrap_or(u8::MAX)]);
            t.update(name.as_bytes());
        }
        t.update(k.host_nonce);
        t.update(k.guest_nonce);
        let transcript: [u8; 32] = t.finalize().into();

        let mut signed = side.label().to_vec();
        signed.extend_from_slice(&transcript);
        let mut confirm = Vec::with_capacity(65);
        confirm.push(TAG_CONFIRM);
        confirm.extend_from_slice(&identity.sign(&signed));

        let (peer_key, peer_name) = match side {
            Side::Host => k.guest,
            Side::Guest => k.host,
        };
        Self {
            side,
            code,
            transcript,
            peer_key,
            peer: PairedPeer {
                id: DeviceId::from_bytes(peer_key),
                name: peer_name,
            },
            confirm,
        }
    }

    /// The 6 digits to show. Both screens show the same digits unless
    /// someone interfered.
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Who is on the other side (trust it only once both confirmed).
    pub fn peer(&self) -> &PairedPeer {
        &self.peer
    }

    /// Send this when the local user clicks Pair.
    pub fn confirm_message(&self) -> Vec<u8> {
        self.confirm.clone()
    }

    /// Send this when the local user cancels.
    pub fn cancel_message() -> Vec<u8> {
        vec![TAG_CANCEL]
    }

    /// Reads the other side's decision, checking its signature.
    pub fn receive_decision(&self, msg: &[u8]) -> Result<Decision, PairError> {
        match msg.split_first() {
            Some((&TAG_CANCEL, [])) => Ok(Decision::Cancelled),
            Some((&TAG_CONFIRM, sig)) => {
                let sig: [u8; 64] = sig.try_into().map_err(|_| PairError::Malformed)?;
                let key = VerifyingKey::from_bytes(&self.peer_key)
                    .map_err(|_| PairError::BadSignature)?;
                let mut signed = self.side.other().label().to_vec();
                signed.extend_from_slice(&self.transcript);
                key.verify(&signed, &Signature::from_bytes(&sig))
                    .map_err(|_| PairError::BadSignature)?;
                Ok(Decision::Confirmed)
            }
            _ => Err(PairError::Malformed),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    use super::*;

    fn run(host: &Identity, guest: &Identity) -> (Compared, Compared) {
        let (g, hello) = Guest::start(guest);
        let (h, reply) = Host::receive_guest_hello(host, &hello).unwrap();
        let (gc, reveal) = g.receive_host_hello(&reply).unwrap();
        let hc = h.receive_reveal(&reveal).unwrap();
        (hc, gc)
    }

    #[test]
    fn both_sides_show_the_same_code_and_accept_each_other() {
        let (host, guest) = (Identity::generate("Desk"), Identity::generate("Laptop"));
        let (hc, gc) = run(&host, &guest);
        assert_eq!(hc.code(), gc.code());
        assert_eq!(hc.code().len(), 6);
        assert!(hc.code().chars().all(|c| c.is_ascii_digit()));
        assert_eq!(hc.peer().id, guest.id());
        assert_eq!(hc.peer().name, "Laptop");
        assert_eq!(gc.peer().id, host.id());
        assert_eq!(gc.peer().name, "Desk");
        assert_eq!(
            hc.receive_decision(&gc.confirm_message()),
            Ok(Decision::Confirmed)
        );
        assert_eq!(
            gc.receive_decision(&hc.confirm_message()),
            Ok(Decision::Confirmed)
        );
    }

    #[test]
    fn each_run_gives_a_fresh_code() {
        let (host, guest) = (Identity::generate("A"), Identity::generate("B"));
        let codes: std::collections::HashSet<String> = (0..8)
            .map(|_| run(&host, &guest).0.code().to_owned())
            .collect();
        assert!(codes.len() > 1);
    }

    #[test]
    fn a_man_in_the_middle_gets_different_codes() {
        let (host, guest, mallory) = (
            Identity::generate("Desk"),
            Identity::generate("Laptop"),
            Identity::generate("Desk"),
        );
        // Mallory talks to the guest as "Desk" and to the host as "Laptop".
        let (g, hello) = Guest::start(&guest);
        let (m_to_guest, m_reply) = Host::receive_guest_hello(&mallory, &hello).unwrap();
        let (gc, reveal) = g.receive_host_hello(&m_reply).unwrap();
        let _ = m_to_guest.receive_reveal(&reveal).unwrap();

        let (m_as_guest, m_hello) = Guest::start(&mallory);
        let (h, reply) = Host::receive_guest_hello(&host, &m_hello).unwrap();
        let (_, m_reveal) = m_as_guest.receive_host_hello(&reply).unwrap();
        let hc = h.receive_reveal(&m_reveal).unwrap();

        // The users see different codes (equal only by a 1-in-a-million fluke).
        assert_ne!(hc.code(), gc.code());
        // And Mallory's confirmations don't pass for the real devices.
        assert_eq!(hc.peer().id, mallory.id());
        assert_ne!(gc.peer().id, host.id());
    }

    #[test]
    fn a_reveal_that_does_not_match_the_commitment_is_refused() {
        let (host, guest) = (Identity::generate("A"), Identity::generate("B"));
        let (g, hello) = Guest::start(&guest);
        let (h, reply) = Host::receive_guest_hello(&host, &hello).unwrap();
        let (_, mut reveal) = g.receive_host_hello(&reply).unwrap();
        reveal[5] ^= 1;
        assert_eq!(
            h.receive_reveal(&reveal).err(),
            Some(PairError::WrongCodeOrTampered)
        );
    }

    #[test]
    fn a_forged_or_misdirected_confirmation_is_refused() {
        let (host, guest) = (Identity::generate("A"), Identity::generate("B"));
        let (hc, gc) = run(&host, &guest);
        let mut forged = gc.confirm_message();
        forged[10] ^= 1;
        assert_eq!(hc.receive_decision(&forged), Err(PairError::BadSignature));
        // A side's own confirmation, reflected back, is not the other side's.
        assert_eq!(
            hc.receive_decision(&hc.confirm_message()),
            Err(PairError::BadSignature)
        );
        // A confirmation from another run doesn't carry over.
        let (_, other) = run(&host, &guest);
        assert_eq!(
            hc.receive_decision(&other.confirm_message()),
            Err(PairError::BadSignature)
        );
    }

    #[test]
    fn cancel_is_reported() {
        let (host, guest) = (Identity::generate("A"), Identity::generate("B"));
        let (hc, _) = run(&host, &guest);
        assert_eq!(
            hc.receive_decision(&Compared::cancel_message()),
            Ok(Decision::Cancelled)
        );
    }

    #[test]
    fn reflected_hello_is_refused() {
        let me = Identity::generate("A");
        let (_, hello) = Guest::start(&me);
        assert_eq!(
            Host::receive_guest_hello(&me, &hello).err(),
            Some(PairError::Reflected)
        );
    }

    #[test]
    fn malformed_messages_are_refused() {
        let (host, guest) = (Identity::generate("A"), Identity::generate("B"));
        let (_, hello) = Guest::start(&guest);
        for cut in [0, 1, 33, 65, hello.len() - 1] {
            assert!(
                Host::receive_guest_hello(&host, &hello[..cut]).is_err(),
                "cut {cut}"
            );
        }
        let mut long = hello.clone();
        long.push(0);
        assert!(Host::receive_guest_hello(&host, &long).is_err());
        let mut wrong_tag = hello.clone();
        wrong_tag[0] = TAG_HOST_HELLO;
        assert!(Host::receive_guest_hello(&host, &wrong_tag).is_err());
        let (hc, _) = run(&host, &guest);
        assert!(hc.receive_decision(&[]).is_err());
        assert!(hc.receive_decision(&[TAG_CANCEL, 0]).is_err());
        assert!(hc.receive_decision(&[TAG_CONFIRM, 1, 2]).is_err());
    }
}
