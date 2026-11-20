//! Where this device keeps its identity.
//!
//! TEMPORARY: the secret key lives in a file readable only by the user
//! (`~/.config/hopwyn/identity`, mode 0600). Before v1.0 it moves to the OS
//! key store (Secret Service on Linux), as SECURITY.md promises.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

use hopwyn_pair::Identity;

fn config_dir() -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir).join("hopwyn"));
    }
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(home).join(".config").join("hopwyn"))
}

/// This machine's name, shown to other devices.
pub fn device_name() -> String {
    fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Hopwyn device".to_owned())
}

/// Loads this device's identity, creating it on first use.
pub fn load_or_create_identity() -> Result<Identity, String> {
    let dir = config_dir()?;
    let path = dir.join("identity");
    let name = device_name();

    if let Ok(bytes) = fs::read(&path) {
        let meta = fs::metadata(&path).map_err(|e| e.to_string())?;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(format!(
                "{} is readable by other users; fix with: chmod 600 {}",
                path.display(),
                path.display()
            ));
        }
        let secret: [u8; 32] = bytes
            .try_into()
            .map_err(|_| format!("{} is corrupt (expected 32 bytes)", path.display()))?;
        return Ok(Identity::from_secret_bytes(&secret, &name));
    }

    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    let identity = Identity::generate(&name);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| format!("could not create {}: {e}", path.display()))?;
    file.write_all(&identity.secret_bytes())
        .map_err(|e| e.to_string())?;
    Ok(identity)
}

/// Full hex form of a device key, for `--trust`.
pub fn hex_id(id: &hopwyn_pair::DeviceId) -> String {
    id.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

pub fn parse_hex_id(s: &str) -> Option<hopwyn_pair::DeviceId> {
    let s = s.trim().to_ascii_lowercase();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (slot, pair) in out.iter_mut().zip(s.as_bytes().as_chunks::<2>().0) {
        *slot = u8::from_str_radix(std::str::from_utf8(pair.as_slice()).ok()?, 16).ok()?;
    }
    Some(hopwyn_pair::DeviceId::from_bytes(out))
}
