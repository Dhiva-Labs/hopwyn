use ed25519_dalek::SigningKey;
use ed25519_dalek::pkcs8::EncodePrivateKey;
use hopwyn_pair::{DeviceId, Identity};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use zeroize::Zeroizing;

use crate::TransportError;

/// DER encoding of an Ed25519 SubjectPublicKeyInfo, up to the 32 key bytes:
/// SEQUENCE { SEQUENCE { OID 1.3.101.112 }, BIT STRING (0 unused bits) }.
const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

pub(crate) struct DeviceCert {
    pub cert: CertificateDer<'static>,
    pub key: PrivateKeyDer<'static>,
}

/// A self-signed certificate for the device's identity key. Only the key
/// inside it matters; peers ignore names, dates and issuers.
pub(crate) fn device_cert(identity: &Identity) -> Result<DeviceCert, TransportError> {
    let secret = Zeroizing::new(identity.secret_bytes());
    let signing = SigningKey::from_bytes(&secret);
    let pkcs8 = signing
        .to_pkcs8_der()
        .map_err(|e| TransportError::Config(e.to_string()))?;
    let key_der = PrivatePkcs8KeyDer::from(pkcs8.as_bytes().to_vec());

    let key_pair = rcgen::KeyPair::from_pkcs8_der_and_sign_algo(&key_der, &rcgen::PKCS_ED25519)
        .map_err(|e| TransportError::Config(e.to_string()))?;
    let params = rcgen::CertificateParams::new(vec!["hopwyn".to_owned()])
        .map_err(|e| TransportError::Config(e.to_string()))?;
    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| TransportError::Config(e.to_string()))?;

    Ok(DeviceCert {
        cert: cert.der().clone(),
        key: PrivateKeyDer::Pkcs8(key_der),
    })
}

/// The Ed25519 key in a peer's certificate.
///
/// Returns `None` unless the encoded key appears exactly once, so a crafted
/// certificate can't smuggle a second key into a name or extension. The TLS
/// handshake signature is then checked against this same key (see
/// `verify.rs`), so presenting someone else's key without their secret fails.
pub(crate) fn device_id_from_cert(der: &[u8]) -> Option<DeviceId> {
    let mut found = None;
    for (i, window) in der.windows(ED25519_SPKI_PREFIX.len()).enumerate() {
        if window == ED25519_SPKI_PREFIX {
            if found.is_some() {
                return None;
            }
            found = Some(i);
        }
    }
    let start = found? + ED25519_SPKI_PREFIX.len();
    let key: [u8; 32] = der.get(start..start + 32)?.try_into().ok()?;
    Some(DeviceId::from_bytes(key))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn certificate_carries_the_identity_key() {
        let id = Identity::generate("Desk");
        let dc = device_cert(&id).unwrap();
        assert_eq!(device_id_from_cert(&dc.cert), Some(id.id()));
    }

    #[test]
    fn duplicated_key_pattern_is_rejected() {
        let id = Identity::generate("Desk");
        let mut der = device_cert(&id).unwrap().cert.to_vec();
        der.extend_from_slice(&ED25519_SPKI_PREFIX);
        der.extend_from_slice(&[7u8; 32]);
        assert_eq!(device_id_from_cert(&der), None);
    }

    #[test]
    fn garbage_has_no_identity() {
        assert_eq!(device_id_from_cert(&[0u8; 200]), None);
        assert_eq!(device_id_from_cert(&[]), None);
    }
}
