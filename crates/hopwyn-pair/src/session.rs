use std::fmt;

use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Identity as SpakeIdentity, Password, Spake2};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::identity::{DeviceId, Identity, MAX_NAME_LEN};

const SPAKE_ID: &[u8] = b"hopwyn-pair-v1";
const SIG_CONTEXT: &[u8] = b"hopwyn-pair-sig-v1";
const ID_LEN: usize = 32;
const SIG_LEN: usize = 64;
const MAC_LEN: usize = 32;

type HmacSha256 = Hmac<Sha256>;

/// A 6-digit pairing code. Zeroed when dropped.
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct PairingCode([u8; 6]);

impl PairingCode {
    /// A uniformly random code from the OS random source.
    pub fn generate() -> Self {
        // Rejection sampling keeps every code equally likely.
        const LIMIT: u32 = u32::MAX - (u32::MAX % 1_000_000);
        let n = loop {
            let v = OsRng.next_u32();
            if v < LIMIT {
                break v % 1_000_000;
            }
        };
        let mut digits = [b'0'; 6];
        let mut rest = n;
        for d in digits.iter_mut().rev() {
            // `rest % 10` is always a single digit.
            *d = b'0' + u8::try_from(rest % 10).unwrap_or(0);
            rest /= 10;
        }
        Self(digits)
    }

    /// Parses what a user typed. Spaces and dashes are ignored; exactly six
    /// digits must remain.
    pub fn parse(input: &str) -> Option<Self> {
        let mut digits = [0u8; 6];
        let mut n = 0;
        for c in input.chars().filter(|c| !matches!(c, ' ' | '-')) {
            let slot = digits.get_mut(n)?;
            if !c.is_ascii_digit() {
                return None;
            }
            *slot = u8::try_from(c).ok()?;
            n += 1;
        }
        (n == 6).then_some(Self(digits))
    }

    /// The code as six ASCII digits, for display.
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap_or_default()
    }
}

impl fmt::Debug for PairingCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairingCode(******)")
    }
}

/// Which side of the pairing this device is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Has "Add device" open and shows the code.
    Initiator,
    /// The user types the code here.
    Joiner,
}

impl Role {
    fn byte(self) -> u8 {
        match self {
            Self::Initiator => b'I',
            Self::Joiner => b'J',
        }
    }

    fn other(self) -> Self {
        match self {
            Self::Initiator => Self::Joiner,
            Self::Joiner => Self::Initiator,
        }
    }

    fn confirm_info(self) -> &'static [u8] {
        match self {
            Self::Initiator => b"hopwyn confirm I",
            Self::Joiner => b"hopwyn confirm J",
        }
    }
}

/// Why pairing failed. Every failure means: nothing was stored, start over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairError {
    /// A message had the wrong shape.
    Malformed,
    /// The peer sent back this device's own message or identity.
    Reflected,
    /// The confirmation came from the wrong role.
    UnexpectedRole,
    /// The codes didn't match, or a message was altered in transit.
    WrongCodeOrTampered,
    /// The peer could not prove it holds its identity key.
    BadSignature,
}

impl fmt::Display for PairError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Malformed => "pairing message is malformed",
            Self::Reflected => "pairing message was reflected back",
            Self::UnexpectedRole => "pairing confirmation came from the wrong side",
            Self::WrongCodeOrTampered => "code did not match, or the exchange was tampered with",
            Self::BadSignature => "peer identity signature is invalid",
        })
    }
}

impl std::error::Error for PairError {}

/// A device this one now trusts. Store `id` and pin it on every connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedPeer {
    pub id: DeviceId,
    pub name: String,
}

/// Step 1: exchanging SPAKE2 messages.
pub struct Pairing<'a> {
    role: Role,
    identity: &'a Identity,
    spake: Spake2<Ed25519Group>,
    my_msg: Vec<u8>,
}

impl<'a> Pairing<'a> {
    /// Starts pairing. Send the returned bytes to the peer.
    pub fn start(role: Role, identity: &'a Identity, code: &PairingCode) -> (Self, Vec<u8>) {
        let (spake, msg) = Spake2::<Ed25519Group>::start_symmetric(
            &Password::new(code.0),
            &SpakeIdentity::new(SPAKE_ID),
        );
        (
            Self {
                role,
                identity,
                spake,
                my_msg: msg.clone(),
            },
            msg,
        )
    }

    /// Takes the peer's SPAKE2 message. Send the returned confirmation to the peer.
    pub fn receive_spake(
        self,
        peer_msg: &[u8],
    ) -> Result<(AwaitingConfirm<'a>, Vec<u8>), PairError> {
        if peer_msg == self.my_msg.as_slice() {
            return Err(PairError::Reflected);
        }
        let shared = Zeroizing::new(
            self.spake
                .finish(peer_msg)
                .map_err(|_| PairError::Malformed)?,
        );

        let (first, second) = match self.role {
            Role::Initiator => (self.my_msg.as_slice(), peer_msg),
            Role::Joiner => (peer_msg, self.my_msg.as_slice()),
        };
        let transcript = transcript_hash(first, second);

        let hk = Hkdf::<Sha256>::new(Some(&transcript), &shared);
        let mut my_key = Zeroizing::new([0u8; 32]);
        let mut peer_key = Zeroizing::new([0u8; 32]);
        hk.expand(self.role.confirm_info(), my_key.as_mut())
            .map_err(|_| PairError::Malformed)?;
        hk.expand(self.role.other().confirm_info(), peer_key.as_mut())
            .map_err(|_| PairError::Malformed)?;

        let confirm = build_confirm(self.role, self.identity, &transcript, &my_key);
        let next = AwaitingConfirm {
            role: self.role,
            identity: self.identity,
            transcript,
            peer_key,
        };
        Ok((next, confirm))
    }
}

/// Step 2: waiting for the peer's confirmation.
#[cfg_attr(test, derive(Clone))]
pub struct AwaitingConfirm<'a> {
    role: Role,
    identity: &'a Identity,
    transcript: [u8; 32],
    peer_key: Zeroizing<[u8; 32]>,
}

impl AwaitingConfirm<'_> {
    /// Checks the peer's confirmation. On success, store the returned peer.
    pub fn receive_confirm(self, msg: &[u8]) -> Result<PairedPeer, PairError> {
        let c = Confirm::parse(msg)?;
        if c.role != self.role.other().byte() {
            return Err(PairError::UnexpectedRole);
        }

        // MAC first: it proves the peer knew the code, in constant time.
        let mut mac = new_mac(&self.peer_key);
        mac.update(&self.transcript);
        mac.update(c.authenticated);
        mac.verify_slice(c.mac)
            .map_err(|_| PairError::WrongCodeOrTampered)?;

        let id = DeviceId::from_bytes(c.id);
        if id == self.identity.id() {
            return Err(PairError::Reflected);
        }
        if !id.verify(&signed_message(&self.transcript, c.role), &c.signature) {
            return Err(PairError::BadSignature);
        }
        let name = std::str::from_utf8(c.name)
            .map_err(|_| PairError::Malformed)?
            .to_owned();
        Ok(PairedPeer { id, name })
    }
}

fn transcript_hash(initiator_msg: &[u8], joiner_msg: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(SPAKE_ID);
    for m in [initiator_msg, joiner_msg] {
        h.update(u32::try_from(m.len()).unwrap_or(u32::MAX).to_le_bytes());
        h.update(m);
    }
    h.finalize().into()
}

fn signed_message(transcript: &[u8; 32], role: u8) -> Vec<u8> {
    let mut m = Vec::with_capacity(SIG_CONTEXT.len() + 33);
    m.extend_from_slice(SIG_CONTEXT);
    m.extend_from_slice(transcript);
    m.push(role);
    m
}

fn new_mac(key: &[u8; 32]) -> HmacSha256 {
    // HMAC accepts keys of any length, so this never fails.
    <HmacSha256 as Mac>::new_from_slice(key).unwrap_or_else(|_| unreachable!())
}

/// Wire layout: role(1) id(32) name_len(1) name(<=64) signature(64) mac(32).
/// The MAC covers the transcript and every byte before it.
fn build_confirm(
    role: Role,
    identity: &Identity,
    transcript: &[u8; 32],
    key: &[u8; 32],
) -> Vec<u8> {
    let name = identity.name().as_bytes();
    let signature = identity.sign(&signed_message(transcript, role.byte()));
    let mut out = Vec::with_capacity(1 + ID_LEN + 1 + name.len() + SIG_LEN + MAC_LEN);
    out.push(role.byte());
    out.extend_from_slice(identity.id().as_bytes());
    out.push(u8::try_from(name.len()).unwrap_or(0));
    out.extend_from_slice(name);
    out.extend_from_slice(&signature);
    let mut mac = new_mac(key);
    mac.update(transcript);
    mac.update(&out);
    out.extend_from_slice(&mac.finalize().into_bytes());
    out
}

struct Confirm<'m> {
    role: u8,
    id: [u8; ID_LEN],
    name: &'m [u8],
    signature: [u8; SIG_LEN],
    /// Every byte before the MAC.
    authenticated: &'m [u8],
    mac: &'m [u8],
}

impl<'m> Confirm<'m> {
    fn parse(msg: &'m [u8]) -> Result<Self, PairError> {
        let (authenticated, mac) = msg
            .split_at_checked(msg.len().checked_sub(MAC_LEN).ok_or(PairError::Malformed)?)
            .ok_or(PairError::Malformed)?;
        let (&role, rest) = authenticated.split_first().ok_or(PairError::Malformed)?;
        let (id, rest) = rest
            .split_first_chunk::<ID_LEN>()
            .ok_or(PairError::Malformed)?;
        let (&name_len, rest) = rest.split_first().ok_or(PairError::Malformed)?;
        let name_len = usize::from(name_len);
        if name_len > MAX_NAME_LEN {
            return Err(PairError::Malformed);
        }
        let (name, rest) = rest
            .split_at_checked(name_len)
            .ok_or(PairError::Malformed)?;
        let (signature, rest) = rest
            .split_first_chunk::<SIG_LEN>()
            .ok_or(PairError::Malformed)?;
        if !rest.is_empty() {
            return Err(PairError::Malformed);
        }
        Ok(Self {
            role,
            id: *id,
            name,
            signature: *signature,
            authenticated,
            mac,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    struct Run {
        i_confirm_from_j: Result<PairedPeer, PairError>,
        j_confirm_from_i: Result<PairedPeer, PairError>,
    }

    fn run(a: &Identity, b: &Identity, code_a: &PairingCode, code_b: &PairingCode) -> Run {
        let (pa, ma) = Pairing::start(Role::Initiator, a, code_a);
        let (pb, mb) = Pairing::start(Role::Joiner, b, code_b);
        let (ca, conf_a) = pa.receive_spake(&mb).unwrap();
        let (cb, conf_b) = pb.receive_spake(&ma).unwrap();
        Run {
            i_confirm_from_j: ca.receive_confirm(&conf_b),
            j_confirm_from_i: cb.receive_confirm(&conf_a),
        }
    }

    #[test]
    fn matching_codes_pair_both_sides() {
        let a = Identity::generate("Office-PC");
        let b = Identity::generate("Laptop");
        let code = PairingCode::generate();
        let r = run(&a, &b, &code, &code);
        assert_eq!(
            r.i_confirm_from_j,
            Ok(PairedPeer {
                id: b.id(),
                name: "Laptop".into()
            })
        );
        assert_eq!(
            r.j_confirm_from_i,
            Ok(PairedPeer {
                id: a.id(),
                name: "Office-PC".into()
            })
        );
    }

    #[test]
    fn wrong_code_fails_on_both_sides() {
        let a = Identity::generate("A");
        let b = Identity::generate("B");
        let r = run(
            &a,
            &b,
            &PairingCode::parse("123456").unwrap(),
            &PairingCode::parse("123457").unwrap(),
        );
        assert_eq!(r.i_confirm_from_j, Err(PairError::WrongCodeOrTampered));
        assert_eq!(r.j_confirm_from_i, Err(PairError::WrongCodeOrTampered));
    }

    #[test]
    fn any_flipped_bit_in_confirm_is_rejected() {
        let a = Identity::generate("A");
        let b = Identity::generate("B");
        let code = PairingCode::generate();
        let (pa, ma) = Pairing::start(Role::Initiator, &a, &code);
        let (pb, mb) = Pairing::start(Role::Joiner, &b, &code);
        let (_, conf_a) = pa.receive_spake(&mb).unwrap();
        let (cb, _) = pb.receive_spake(&ma).unwrap();
        // Same exchange every time: only the flipped bit differs.
        assert!(cb.clone().receive_confirm(&conf_a).is_ok());
        for i in 0..conf_a.len() {
            for bit in 0..8 {
                let mut bad = conf_a.clone();
                bad[i] ^= 1 << bit;
                assert!(
                    cb.clone().receive_confirm(&bad).is_err(),
                    "flip at byte {i} bit {bit} accepted"
                );
            }
        }
    }

    #[test]
    fn tampered_spake_message_breaks_pairing() {
        let a = Identity::generate("A");
        let b = Identity::generate("B");
        let code = PairingCode::generate();
        let (pa, ma) = Pairing::start(Role::Initiator, &a, &code);
        let (pb, mut mb) = Pairing::start(Role::Joiner, &b, &code);
        let last = mb.len() - 1;
        mb[last] ^= 0x01;
        // A tampered point either fails to decode or yields a different key.
        let Ok((ca, _)) = pa.receive_spake(&mb) else {
            return;
        };
        let (_, conf_b) = pb.receive_spake(&ma).unwrap();
        assert_eq!(
            ca.receive_confirm(&conf_b),
            Err(PairError::WrongCodeOrTampered)
        );
    }

    #[test]
    fn reflected_messages_are_rejected() {
        let a = Identity::generate("A");
        let code = PairingCode::generate();
        let (pa, ma) = Pairing::start(Role::Initiator, &a, &code);
        assert!(matches!(pa.receive_spake(&ma), Err(PairError::Reflected)));
    }

    #[test]
    fn confirm_from_same_role_is_rejected() {
        let a = Identity::generate("A");
        let b = Identity::generate("B");
        let code = PairingCode::generate();
        let (pa, _) = Pairing::start(Role::Initiator, &a, &code);
        let (pb, mb) = Pairing::start(Role::Joiner, &b, &code);
        let (ca, conf_a) = pa.receive_spake(&mb).unwrap();
        let _ = pb;
        // The initiator's own confirmation sent back to it.
        assert_eq!(ca.receive_confirm(&conf_a), Err(PairError::UnexpectedRole));
    }

    #[test]
    fn short_or_long_confirm_is_malformed() {
        let a = Identity::generate("A");
        let b = Identity::generate("B");
        let code = PairingCode::generate();
        let (pa, _) = Pairing::start(Role::Initiator, &a, &code);
        let (_, mb) = Pairing::start(Role::Joiner, &b, &code);
        let (ca, _) = pa.receive_spake(&mb).unwrap();
        assert_eq!(ca.receive_confirm(&[0u8; 10]), Err(PairError::Malformed));
    }

    #[test]
    fn codes_are_six_digits_and_parse_round_trips() {
        for _ in 0..1000 {
            let c = PairingCode::generate();
            assert_eq!(c.as_str().len(), 6);
            assert!(c.as_str().bytes().all(|b| b.is_ascii_digit()));
            assert_eq!(PairingCode::parse(c.as_str()), Some(c));
        }
    }

    #[test]
    fn parse_accepts_separators_and_rejects_bad_input() {
        assert_eq!(PairingCode::parse("123 456").unwrap().as_str(), "123456");
        assert_eq!(PairingCode::parse("123-456").unwrap().as_str(), "123456");
        assert!(PairingCode::parse("12345").is_none());
        assert!(PairingCode::parse("1234567").is_none());
        assert!(PairingCode::parse("12a456").is_none());
        assert!(PairingCode::parse("١٢٣٤٥٦").is_none()); // non-ASCII digits
    }

    #[test]
    fn code_debug_hides_digits() {
        let c = PairingCode::parse("987654").unwrap();
        assert!(!format!("{c:?}").contains("987654"));
    }
}
