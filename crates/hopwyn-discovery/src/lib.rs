//! Finds other Hopwyn devices on the home network.
//!
//! Each device announces `_hopwyn._udp.local.` with its full device key,
//! display name and protocol version in the TXT record, and browses for the
//! same service.
//!
//! Discovery is **untrusted**: anyone on the network can announce any key
//! and name. Results are only hints for where to connect. Who a peer really
//! is gets proven by the transport's pinned-key handshake. This crate still
//! validates everything it receives: wrong versions, malformed keys, names
//! with control characters, non-home addresses and floods are dropped.

mod parse;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::mpsc;
use std::thread;

use hopwyn_pair::{DeviceId, Identity};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

pub use parse::{DiscoveredDevice, MAX_TRACKED, instance_name};

/// DNS-SD service type for Hopwyn.
pub const SERVICE_TYPE: &str = "_hopwyn._udp.local.";
/// TXT record format version.
pub const TXT_VERSION: &str = "1";

/// A change in the set of devices seen on the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryEvent {
    /// A device appeared, or its name or addresses changed.
    Found(DiscoveredDevice),
    /// A device stopped announcing.
    Lost(DeviceId),
}

/// Why discovery could not start.
#[derive(Debug)]
pub struct DiscoveryError(String);

impl std::fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "discovery failed: {}", self.0)
    }
}

impl std::error::Error for DiscoveryError {}

fn err(e: impl std::fmt::Display) -> DiscoveryError {
    DiscoveryError(e.to_string())
}

/// Announces this device and reports others. Stops on drop.
pub struct Discovery {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Discovery {
    /// Starts announcing `identity` on `port` (the transport's UDP port) and
    /// browsing for other devices. Events arrive on the returned channel.
    pub fn start(
        identity: &Identity,
        port: u16,
    ) -> Result<(Self, mpsc::Receiver<DiscoveryEvent>), DiscoveryError> {
        let daemon = ServiceDaemon::new().map_err(err)?;
        let self_id = identity.id();

        let instance = instance_name(&self_id);
        let host = format!("{instance}.local.");
        let mut txt = HashMap::new();
        txt.insert("v".to_owned(), TXT_VERSION.to_owned());
        txt.insert("id".to_owned(), parse::hex(self_id.as_bytes()));
        txt.insert("name".to_owned(), identity.name().to_owned());

        let info = ServiceInfo::new(SERVICE_TYPE, &instance, &host, (), port, txt)
            .map_err(err)?
            .enable_addr_auto();
        let fullname = info.get_fullname().to_owned();
        daemon.register(info).map_err(err)?;

        let events = daemon.browse(SERVICE_TYPE).map_err(err)?;
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("hopwyn-discovery".into())
            .spawn(move || pump(&events, &tx, self_id))
            .map_err(err)?;

        Ok((Self { daemon, fullname }, rx))
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        // Best effort: say goodbye so peers drop us quickly, then stop.
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

/// Turns raw mDNS events into validated discovery events. Ends when either
/// side of the channel goes away.
fn pump(
    events: &mdns_sd::Receiver<ServiceEvent>,
    tx: &mpsc::Sender<DiscoveryEvent>,
    self_id: DeviceId,
) {
    let mut known: HashMap<String, DeviceId> = HashMap::new();
    while let Ok(event) = events.recv() {
        let out = match event {
            ServiceEvent::ServiceResolved(svc) => {
                let addrs: Vec<SocketAddr> = svc
                    .get_addresses()
                    .iter()
                    .map(|ip| SocketAddr::new(ip.to_ip_addr(), svc.get_port()))
                    .collect();
                let device = parse::validate(
                    &svc.fullname,
                    svc.get_property_val_str("v"),
                    svc.get_property_val_str("id"),
                    svc.get_property_val_str("name"),
                    &addrs,
                    self_id,
                );
                match device {
                    Some(d) if known.contains_key(&svc.fullname) || known.len() < MAX_TRACKED => {
                        known.insert(svc.fullname.clone(), d.id);
                        Some(DiscoveryEvent::Found(d))
                    }
                    _ => None,
                }
            }
            ServiceEvent::ServiceRemoved(_, fullname) => {
                known.remove(&fullname).map(DiscoveryEvent::Lost)
            }
            _ => None,
        };
        if let Some(e) = out {
            if tx.send(e).is_err() {
                return;
            }
        }
    }
}
