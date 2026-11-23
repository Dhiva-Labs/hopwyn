use std::net::SocketAddr;

use hopwyn_pair::{DeviceId, MAX_NAME_LEN};
use hopwyn_transport::is_home_address;

use crate::{SERVICE_TYPE, TXT_VERSION};

/// Most devices tracked at once; announcements beyond this are ignored, so a
/// flood of fake devices can't grow memory without bound.
pub const MAX_TRACKED: usize = 64;
/// Most addresses kept per device.
const MAX_ADDRS: usize = 8;

/// A device seen on the network. Unverified until the transport connects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredDevice {
    pub id: DeviceId,
    pub name: String,
    /// Home-network addresses only, IPv4 first.
    pub addrs: Vec<SocketAddr>,
    /// Set while the device has "Add computer" open: a token that changes
    /// each time, so a prompt is shown once per pairing window.
    pub pairing: Option<String>,
    /// The TCP port it pairs on, if it announced one.
    pub pairing_port: Option<u16>,
    /// It says it is a phone: a remote, not a computer to control.
    pub phone: bool,
}

/// Accepts a pairing token of 1 to 16 lowercase hex digits.
pub(crate) fn pairing_token(raw: Option<&str>) -> Option<String> {
    let t = raw?;
    let ok = (1..=16).contains(&t.len())
        && t.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase());
    ok.then(|| t.to_owned())
}

/// DNS-SD instance name: the first 16 bytes of the device key in hex
/// (32 characters, within the 63-byte DNS label limit).
pub fn instance_name(id: &DeviceId) -> String {
    hex(id.as_bytes().get(..16).unwrap_or_default())
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_id(s: &str) -> Option<DeviceId> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (slot, pair) in out.iter_mut().zip(s.as_bytes().as_chunks::<2>().0) {
        let pair = std::str::from_utf8(pair.as_slice()).ok()?;
        *slot = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(DeviceId::from_bytes(out))
}

/// Strips control and bidi-override characters (they could disguise one
/// device as another in the UI) and enforces the name length limit.
fn clean_name(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .filter(|c| {
            !c.is_control() && !matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_NAME_LEN {
        return None;
    }
    Some(trimmed.to_owned())
}

/// Checks a pairing-window announcement and returns whose it is. Its
/// instance name must match the key it claims, like a device announcement.
pub(crate) fn pairing_owner(
    fullname: &str,
    version: Option<&str>,
    id: Option<&str>,
    self_id: DeviceId,
) -> Option<DeviceId> {
    if version? != TXT_VERSION {
        return None;
    }
    let id = parse_id(&id?.to_ascii_lowercase())?;
    let expected = format!("{}.{}", instance_name(&id), crate::PAIR_SERVICE_TYPE);
    (id != self_id && fullname == expected).then_some(id)
}

/// Checks one resolved announcement. `None` means ignore it.
pub(crate) fn validate(
    fullname: &str,
    version: Option<&str>,
    id: Option<&str>,
    name: Option<&str>,
    addrs: &[SocketAddr],
    self_id: DeviceId,
) -> Option<DiscoveredDevice> {
    if version? != TXT_VERSION {
        return None;
    }
    let id = parse_id(&id?.to_ascii_lowercase())?;
    if id == self_id {
        return None;
    }
    // The instance name must match the key it claims, so one announcement
    // can't shadow another device's entry.
    if fullname != format!("{}.{SERVICE_TYPE}", instance_name(&id)) {
        return None;
    }
    let name = clean_name(name?)?;

    let mut addrs: Vec<SocketAddr> = addrs
        .iter()
        .copied()
        .filter(|a| a.port() != 0 && is_home_address(a.ip()))
        .collect();
    addrs.sort_by_key(|a| (a.is_ipv6(), *a));
    addrs.dedup();
    addrs.truncate(MAX_ADDRS);
    if addrs.is_empty() {
        return None;
    }
    Some(DiscoveredDevice {
        id,
        name,
        addrs,
        pairing: None,
        pairing_port: None,
        phone: false,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use hopwyn_pair::Identity;

    struct Case {
        fullname: String,
        id_hex: String,
        addrs: Vec<SocketAddr>,
    }

    fn case(id: &DeviceId) -> Case {
        Case {
            fullname: format!("{}.{SERVICE_TYPE}", instance_name(id)),
            id_hex: hex(id.as_bytes()),
            addrs: vec!["192.168.0.20:4242".parse().unwrap()],
        }
    }

    fn me() -> DeviceId {
        Identity::generate("me").id()
    }

    #[test]
    fn pairing_owner_must_match_its_instance_name() {
        let peer = Identity::generate("Laptop").id();
        let hex_id = hex(peer.as_bytes());
        let good = format!("{}.{}", instance_name(&peer), crate::PAIR_SERVICE_TYPE);
        assert_eq!(
            pairing_owner(&good, Some("1"), Some(&hex_id), me()),
            Some(peer)
        );
        let other = Identity::generate("Other").id();
        let wrong = format!("{}.{}", instance_name(&other), crate::PAIR_SERVICE_TYPE);
        assert_eq!(pairing_owner(&wrong, Some("1"), Some(&hex_id), me()), None);
        assert_eq!(pairing_owner(&good, Some("2"), Some(&hex_id), me()), None);
        assert_eq!(pairing_owner(&good, Some("1"), None, me()), None);
        assert_eq!(pairing_owner(&good, Some("1"), Some(&hex_id), peer), None);
    }

    #[test]
    fn pairing_tokens_are_validated() {
        assert_eq!(pairing_token(Some("0a1b2c3d")), Some("0a1b2c3d".into()));
        assert_eq!(pairing_token(None), None);
        assert_eq!(pairing_token(Some("")), None);
        assert_eq!(pairing_token(Some("XYZ")), None);
        assert_eq!(pairing_token(Some("ABCD")), None);
        assert_eq!(pairing_token(Some("0123456789abcdef0")), None);
    }

    #[test]
    fn valid_announcement_is_accepted() {
        let peer = Identity::generate("Laptop").id();
        let c = case(&peer);
        let d = validate(
            &c.fullname,
            Some("1"),
            Some(&c.id_hex),
            Some("Laptop"),
            &c.addrs,
            me(),
        )
        .unwrap();
        assert_eq!(d.id, peer);
        assert_eq!(d.name, "Laptop");
        assert_eq!(d.addrs, c.addrs);
    }

    #[test]
    fn own_announcement_is_ignored() {
        let my = me();
        let c = case(&my);
        assert!(
            validate(
                &c.fullname,
                Some("1"),
                Some(&c.id_hex),
                Some("me"),
                &c.addrs,
                my
            )
            .is_none()
        );
    }

    #[test]
    fn wrong_or_missing_version_is_ignored() {
        let peer = Identity::generate("P").id();
        let c = case(&peer);
        assert!(
            validate(
                &c.fullname,
                Some("2"),
                Some(&c.id_hex),
                Some("P"),
                &c.addrs,
                me()
            )
            .is_none()
        );
        assert!(
            validate(
                &c.fullname,
                None,
                Some(&c.id_hex),
                Some("P"),
                &c.addrs,
                me()
            )
            .is_none()
        );
    }

    #[test]
    fn malformed_keys_are_ignored() {
        let peer = Identity::generate("P").id();
        let c = case(&peer);
        for bad in [
            "",
            "zz",
            &"g".repeat(64),
            &c.id_hex[..62],
            &format!("{}00", c.id_hex),
        ] {
            assert!(
                validate(&c.fullname, Some("1"), Some(bad), Some("P"), &c.addrs, me()).is_none(),
                "{bad}"
            );
        }
    }

    #[test]
    fn instance_name_must_match_claimed_key() {
        let peer = Identity::generate("P").id();
        let other = Identity::generate("O").id();
        let c = case(&peer);
        let wrong = format!("{}.{SERVICE_TYPE}", instance_name(&other));
        assert!(
            validate(
                &wrong,
                Some("1"),
                Some(&c.id_hex),
                Some("P"),
                &c.addrs,
                me()
            )
            .is_none()
        );
    }

    #[test]
    fn names_are_cleaned_and_bounded() {
        let peer = Identity::generate("P").id();
        let c = case(&peer);
        let d = validate(
            &c.fullname,
            Some("1"),
            Some(&c.id_hex),
            Some(" Office\u{202e}-PC\n "),
            &c.addrs,
            me(),
        )
        .unwrap();
        assert_eq!(d.name, "Office-PC");
        let long = "x".repeat(MAX_NAME_LEN + 1);
        assert!(
            validate(
                &c.fullname,
                Some("1"),
                Some(&c.id_hex),
                Some(&long),
                &c.addrs,
                me()
            )
            .is_none()
        );
        assert!(
            validate(
                &c.fullname,
                Some("1"),
                Some(&c.id_hex),
                Some("\u{0007}"),
                &c.addrs,
                me()
            )
            .is_none()
        );
    }

    #[test]
    fn only_home_addresses_are_kept_ipv4_first() {
        let peer = Identity::generate("P").id();
        let c = case(&peer);
        let addrs: Vec<SocketAddr> = [
            "8.8.8.8:4242",
            "[fe80::1]:4242",
            "10.0.0.5:4242",
            "10.0.0.5:4242",
            "192.168.1.9:0",
        ]
        .iter()
        .map(|s| s.parse().unwrap())
        .collect();
        let d = validate(
            &c.fullname,
            Some("1"),
            Some(&c.id_hex),
            Some("P"),
            &addrs,
            me(),
        )
        .unwrap();
        let want: Vec<SocketAddr> = ["10.0.0.5:4242", "[fe80::1]:4242"]
            .iter()
            .map(|s| s.parse().unwrap())
            .collect();
        assert_eq!(d.addrs, want);

        let public_only: Vec<SocketAddr> = vec!["8.8.8.8:4242".parse().unwrap()];
        assert!(
            validate(
                &c.fullname,
                Some("1"),
                Some(&c.id_hex),
                Some("P"),
                &public_only,
                me()
            )
            .is_none()
        );
    }

    #[test]
    fn instance_name_fits_a_dns_label() {
        assert_eq!(instance_name(&me()).len(), 32);
    }
}
