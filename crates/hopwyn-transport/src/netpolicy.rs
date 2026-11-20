use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// True for addresses that can only be on a home or local network:
/// IPv4 private ranges (RFC 1918), IPv4 and IPv6 link-local, IPv6 unique
/// local (fc00::/7), and loopback. Everything else, including CGNAT
/// (100.64.0.0/10), is refused.
pub fn is_home_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_home_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_home_v4(v4),
            None => is_home_v6(v6),
        },
    }
}

fn is_home_v4(ip: Ipv4Addr) -> bool {
    ip.is_private() || ip.is_link_local() || ip.is_loopback()
}

fn is_home_v6(ip: Ipv6Addr) -> bool {
    let first = ip.segments().first().copied().unwrap_or(0);
    let unique_local = first & 0xfe00 == 0xfc00;
    let link_local = first & 0xffc0 == 0xfe80;
    unique_local || link_local || ip.is_loopback()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn home_addresses_are_allowed() {
        for s in [
            "192.168.0.132",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "169.254.10.10",
            "127.0.0.1",
            "fd42:f72:6d9:135e::1",
            "fe80::1",
            "::1",
            "::ffff:192.168.1.5",
        ] {
            assert!(is_home_address(ip(s)), "{s} should be allowed");
        }
    }

    #[test]
    fn public_and_special_addresses_are_refused() {
        for s in [
            "8.8.8.8",
            "172.32.0.1",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "2001:4860:4860::8888",
            "::",
            "::ffff:8.8.8.8",
        ] {
            assert!(!is_home_address(ip(s)), "{s} should be refused");
        }
    }
}
