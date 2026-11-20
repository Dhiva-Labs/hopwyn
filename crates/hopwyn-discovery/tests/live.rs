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
                if let Ok(DiscoveryEvent::Found(d)) = rx.recv_timeout(Duration::from_millis(500)) {
                    if d.id == want {
                        assert!(d.addrs.iter().all(|s| s.port() == port));
                        return d;
                    }
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
