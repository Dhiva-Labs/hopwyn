use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use hopwyn_pair::DeviceId;

/// The set of device keys this device accepts. Shared and cheap to clone,
/// so pairing can add a device while connections are being verified.
#[derive(Debug, Clone, Default)]
pub struct TrustStore {
    inner: Arc<RwLock<HashSet<DeviceId>>>,
}

impl TrustStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Trust a device after a successful pairing.
    pub fn add(&self, id: DeviceId) {
        if let Ok(mut set) = self.inner.write() {
            set.insert(id);
        }
    }

    /// "Forget device": stop accepting it. Existing connections are not
    /// closed by this call; the caller closes them.
    pub fn remove(&self, id: &DeviceId) {
        if let Ok(mut set) = self.inner.write() {
            set.remove(id);
        }
    }

    /// Fails closed: a poisoned lock means nothing is trusted.
    pub fn contains(&self, id: &DeviceId) -> bool {
        self.inner
            .read()
            .map(|set| set.contains(id))
            .unwrap_or(false)
    }
}
