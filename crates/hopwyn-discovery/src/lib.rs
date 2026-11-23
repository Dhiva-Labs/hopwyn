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
/// TXT `kind` value a phone announces. Computers announce none.
const KIND_PHONE: &str = "phone";

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

/// DNS-SD service type announced only while "Add computer" is open. A
/// separate, short-lived service (rather than a changed TXT record) so
/// other devices notice at once: peers may keep a cached TXT record.
pub const PAIR_SERVICE_TYPE: &str = "_hopwyn-pair._tcp.local.";

/// Announces this device and reports others. Stops on drop.
pub struct Discovery {
    daemon: ServiceDaemon,
    fullname: String,
    instance: String,
    host: String,
    id_hex: String,
    pairing: Option<String>,
}

impl Discovery {
    /// Starts announcing `identity` on `port` (the transport's UDP port) and
    /// browsing for other devices. Events arrive on the returned channel.
    pub fn start(
        identity: &Identity,
        port: u16,
    ) -> Result<(Self, mpsc::Receiver<DiscoveryEvent>), DiscoveryError> {
        Self::start_as(identity, port, false)
    }

    /// Like [`Discovery::start`], for a phone: other devices see
    /// [`DiscoveredDevice::phone`] set, so phones don't list each other as
    /// computers.
    pub fn start_phone(
        identity: &Identity,
        port: u16,
    ) -> Result<(Self, mpsc::Receiver<DiscoveryEvent>), DiscoveryError> {
        Self::start_as(identity, port, true)
    }

    fn start_as(
        identity: &Identity,
        port: u16,
        phone: bool,
    ) -> Result<(Self, mpsc::Receiver<DiscoveryEvent>), DiscoveryError> {
        let daemon = ServiceDaemon::new().map_err(err)?;
        let self_id = identity.id();

        let instance = instance_name(&self_id);
        let host = format!("{instance}.local.");
        let id_hex = parse::hex(self_id.as_bytes());
        let mut txt = HashMap::new();
        txt.insert("v".to_owned(), TXT_VERSION.to_owned());
        txt.insert("id".to_owned(), id_hex.clone());
        txt.insert("name".to_owned(), identity.name().to_owned());
        if phone {
            txt.insert("kind".to_owned(), KIND_PHONE.to_owned());
        }

        let info = ServiceInfo::new(SERVICE_TYPE, &instance, &host, (), port, txt)
            .map_err(err)?
            .enable_addr_auto();
        let fullname = info.get_fullname().to_owned();
        daemon.register(info).map_err(err)?;

        // Both browsers feed one channel, so one thread keeps the state.
        let (raw_tx, raw_rx) = mpsc::channel::<(Kind, ServiceEvent)>();
        for (kind, ty) in [
            (Kind::Device, SERVICE_TYPE),
            (Kind::Pairing, PAIR_SERVICE_TYPE),
        ] {
            let events = daemon.browse(ty).map_err(err)?;
            let raw_tx = raw_tx.clone();
            thread::Builder::new()
                .name("hopwyn-browse".into())
                .spawn(move || {
                    while let Ok(e) = events.recv() {
                        if raw_tx.send((kind, e)).is_err() {
                            return;
                        }
                    }
                })
                .map_err(err)?;
        }
        drop(raw_tx);
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("hopwyn-discovery".into())
            .spawn(move || pump(&raw_rx, &tx, self_id))
            .map_err(err)?;

        Ok((
            Self {
                daemon,
                fullname,
                instance,
                host,
                id_hex,
                pairing: None,
            },
            rx,
        ))
    }

    /// Announces that this device has "Add computer" open, pairing on TCP
    /// `port` (`Some(token)`: 1 to 16 lowercase hex digits, new for each
    /// window), or closed (`None`). Other devices see it in
    /// [`DiscoveredDevice::pairing`] and [`DiscoveredDevice::pairing_port`].
    pub fn set_pairing(&mut self, token: Option<&str>, port: u16) -> Result<(), DiscoveryError> {
        let fullname = format!("{}.{PAIR_SERVICE_TYPE}", self.instance);
        if self.pairing.take().is_some() {
            let _ = self.daemon.unregister(&fullname);
        }
        let Some(t) = token else {
            return Ok(());
        };
        if parse::pairing_token(Some(t)).is_none() {
            return Err(DiscoveryError("bad pairing token".into()));
        }
        let mut txt = HashMap::new();
        txt.insert("v".to_owned(), TXT_VERSION.to_owned());
        txt.insert("id".to_owned(), self.id_hex.clone());
        txt.insert("t".to_owned(), t.to_owned());
        let info = ServiceInfo::new(PAIR_SERVICE_TYPE, &self.instance, &self.host, (), port, txt)
            .map_err(err)?
            .enable_addr_auto();
        self.daemon.register(info).map_err(err)?;
        self.pairing = Some(t.to_owned());
        Ok(())
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        // Best effort: say goodbye so peers drop us quickly, then stop.
        if self.pairing.is_some() {
            let _ = self
                .daemon
                .unregister(&format!("{}.{PAIR_SERVICE_TYPE}", self.instance));
        }
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

#[derive(Debug, Clone, Copy)]
enum Kind {
    Device,
    Pairing,
}

/// Turns raw mDNS events into validated discovery events. Ends when either
/// side of the channel goes away.
fn pump(
    events: &mpsc::Receiver<(Kind, ServiceEvent)>,
    tx: &mpsc::Sender<DiscoveryEvent>,
    self_id: DeviceId,
) {
    let mut devices: HashMap<String, DiscoveredDevice> = HashMap::new();
    // Open pairing windows: device -> (token, port), and fullname -> device.
    let mut open: HashMap<DeviceId, (String, u16)> = HashMap::new();
    let mut open_names: HashMap<String, DeviceId> = HashMap::new();
    let with_pairing = |mut d: DiscoveredDevice, open: &HashMap<DeviceId, (String, u16)>| {
        if let Some((t, p)) = open.get(&d.id) {
            d.pairing = Some(t.clone());
            d.pairing_port = Some(*p);
        }
        d
    };
    while let Ok((kind, event)) = events.recv() {
        let out = match (kind, event) {
            (Kind::Device, ServiceEvent::ServiceResolved(svc)) => {
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
                let phone = svc.get_property_val_str("kind") == Some(KIND_PHONE);
                match device.map(|d| DiscoveredDevice { phone, ..d }) {
                    Some(d)
                        if devices.contains_key(&svc.fullname) || devices.len() < MAX_TRACKED =>
                    {
                        devices.insert(svc.fullname.clone(), d.clone());
                        Some(DiscoveryEvent::Found(with_pairing(d, &open)))
                    }
                    _ => None,
                }
            }
            (Kind::Device, ServiceEvent::ServiceRemoved(_, fullname)) => devices
                .remove(&fullname)
                .map(|d| DiscoveryEvent::Lost(d.id)),
            (Kind::Pairing, ServiceEvent::ServiceResolved(svc)) => {
                let id = parse::pairing_owner(
                    &svc.fullname,
                    svc.get_property_val_str("v"),
                    svc.get_property_val_str("id"),
                    self_id,
                );
                let token = parse::pairing_token(svc.get_property_val_str("t"));
                match (id, token) {
                    (Some(id), Some(token)) if svc.get_port() != 0 && open.len() < MAX_TRACKED => {
                        open.insert(id, (token, svc.get_port()));
                        open_names.insert(svc.fullname.clone(), id);
                        devices
                            .values()
                            .find(|d| d.id == id)
                            .map(|d| DiscoveryEvent::Found(with_pairing(d.clone(), &open)))
                    }
                    _ => None,
                }
            }
            (Kind::Pairing, ServiceEvent::ServiceRemoved(_, fullname)) => {
                open_names.remove(&fullname).and_then(|id| {
                    open.remove(&id);
                    devices
                        .values()
                        .find(|d| d.id == id)
                        .map(|d| DiscoveryEvent::Found(d.clone()))
                })
            }
            _ => None,
        };
        if let Some(e) = out
            && tx.send(e).is_err()
        {
            return;
        }
    }
}
