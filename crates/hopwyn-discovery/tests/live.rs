#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Uses real multicast on this machine's network interfaces, so it is
//! ignored by default (CI runners often block multicast). Run with:
//! `cargo test -p hopwyn-discovery --test live -- --ignored`

use std::time::{Duration, Instant};

use hopwyn_discovery::{Discovery, DiscoveryEvent};
use hopwyn_pair::Identity;

#[test]
#[ignore = "needs multicast on a real network interface"]
fn two_devices_find_each_other() {
    let a = Identity::generate("Office-PC");
    let b = Identity::generate("Laptop");
    let (_da, rx_a) = Discovery::start(&a, 40001).unwrap();
    let (db, rx_b) = Discovery::start(&b, 40002).unwrap();

    let wait_for =
        |rx: &std::sync::mpsc::Receiver<DiscoveryEvent>, want: hopwyn_pair::DeviceId, port: u16| {
            let deadline = Instant::now() + Duration::from_secs(15);
            while Instant::now() < deadline {
                if let Ok(DiscoveryEvent::Found(d)) = rx.recv_timeout(Duration::from_millis(500))
                    && d.id == want
                {
                    assert!(d.addrs.iter().all(|s| s.port() == port));
                    return d;
                }
            }
            panic!("device not found within 15 s");
        };

    let seen_by_a = wait_for(&rx_a, b.id(), 40002);
    assert_eq!(seen_by_a.name, "Laptop");
    let seen_by_b = wait_for(&rx_b, a.id(), 40001);
    assert_eq!(seen_by_b.name, "Office-PC");

    // B leaves: A should hear it.
    drop(db);
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut lost = false;
    while Instant::now() < deadline && !lost {
        lost = matches!(rx_a.recv_timeout(Duration::from_millis(500)), Ok(DiscoveryEvent::Lost(id)) if id == b.id());
    }
    assert!(lost, "removal not seen within 15 s");
}

#[test]
#[ignore = "needs multicast on a real network interface"]
fn pairing_window_is_announced_and_withdrawn() {
    let a = Identity::generate("Desk");
    let b = Identity::generate("Laptop");
    let (mut da, _rx_a) = Discovery::start(&a, 40011).unwrap();
    let (_db, rx_b) = Discovery::start(&b, 40012).unwrap();

    let wait = |want: Option<&str>| {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if let Ok(DiscoveryEvent::Found(d)) = rx_b.recv_timeout(Duration::from_millis(300))
                && d.id == a.id()
                && d.pairing.as_deref() == want
            {
                if want.is_some() {
                    assert_eq!(d.pairing_port, Some(40013));
                }
                return;
            }
        }
        panic!("pairing state {want:?} not seen within 15 s");
    };

    wait(None);
    da.set_pairing(Some("1a2b3c4d"), 40013).unwrap();
    wait(Some("1a2b3c4d"));
    da.set_pairing(None, 0).unwrap();
    wait(None);
    assert!(da.set_pairing(Some("NOT HEX"), 40013).is_err());
    // A new window with a new token is seen too.
    da.set_pairing(Some("5e6f"), 40013).unwrap();
    wait(Some("5e6f"));
}
