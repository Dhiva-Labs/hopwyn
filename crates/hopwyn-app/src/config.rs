//! What the app remembers between runs, under `~/.config/hopwyn/`:
//!
//! - `identity`: this device's secret key (mode 0600). TEMPORARY until the
//!   OS key store is wired up, as SECURITY.md notes.
//! - `partners`: the paired devices, one per line: key, which edge of this
//!   screen it sits beyond, its name. (Older builds wrote one to `partner`.)
//! - `restore-token`: the desktop's remote-control permission token (0600),
//!   so the remote-control dialog is shown once, not on every handoff.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use hopwyn_pair::{DeviceId, Identity, MAX_NAME_LEN};
use hopwyn_proto::Edge;

/// Most devices one computer can be paired with: one beyond each screen edge.
pub const MAX_PARTNERS: usize = 4;

/// A device this one shares its mouse and keyboard with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partner {
    pub id: DeviceId,
    pub name: String,
    /// The edge of THIS screen beyond which the partner's screen sits.
    pub edge: Edge,
}

#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// The default location, `$XDG_CONFIG_HOME/hopwyn` or `~/.config/hopwyn`.
    pub fn open_default() -> Result<Self, String> {
        let base = match std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
            Some(d) => PathBuf::from(d),
            None => {
                PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?).join(".config")
            }
        };
        Self::open(base.join("hopwyn"))
    }

    pub fn open(dir: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        Ok(Self { dir })
    }

    /// Loads this device's identity, creating it on first use.
    pub fn identity(&self, name: &str) -> Result<Identity, String> {
        let path = self.dir.join("identity");
        match read_private(&path)? {
            Some(bytes) => {
                let secret: [u8; 32] = bytes
                    .try_into()
                    .map_err(|_| format!("{} is corrupt", path.display()))?;
                Ok(Identity::from_secret_bytes(&secret, name))
            }
            None => {
                let identity = Identity::generate(name);
                write_private(&path, &identity.secret_bytes())?;
                Ok(identity)
            }
        }
    }

    /// The paired devices, at most one per edge. Bad lines are skipped.
    pub fn partners(&self) -> Vec<Partner> {
        let text = fs::read_to_string(self.dir.join("partners"))
            .or_else(|_| fs::read_to_string(self.dir.join("partner")))
            .unwrap_or_default();
        let mut out: Vec<Partner> = Vec::new();
        for p in text.lines().filter_map(parse_partner) {
            if out.len() < MAX_PARTNERS && !out.iter().any(|q| q.id == p.id || q.edge == p.edge) {
                out.push(p);
            }
        }
        out
    }

    pub fn save_partners(&self, partners: &[Partner]) -> Result<(), String> {
        let text: String = partners.iter().map(format_partner).collect();
        write_private(&self.dir.join("partners"), text.as_bytes())?;
        match fs::remove_file(self.dir.join("partner")) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }

    pub fn restore_token(&self) -> Option<String> {
        let bytes = read_private(&self.dir.join("restore-token")).ok()??;
        String::from_utf8(bytes)
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    }

    pub fn save_restore_token(&self, token: &str) -> Result<(), String> {
        write_private(&self.dir.join("restore-token"), token.as_bytes())
    }

    /// The input-capture permission token, on desktops that can remember it.
    pub fn capture_token(&self) -> Option<String> {
        let bytes = read_private(&self.dir.join("capture-token")).ok()??;
        String::from_utf8(bytes)
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    }

    pub fn save_capture_token(&self, token: &str) -> Result<(), String> {
        write_private(&self.dir.join("capture-token"), token.as_bytes())
    }
}

/// Reads a file that must not be readable by other users. `None` if missing.
fn read_private(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
        Ok(meta) if meta.permissions().mode() & 0o077 != 0 => {
            return Err(format!(
                "{} is readable by other users; fix with: chmod 600 {}",
                path.display(),
                path.display()
            ));
        }
        Ok(_) => {}
    }
    fs::read(path).map(Some).map_err(|e| e.to_string())
}

/// Writes atomically with mode 0600: temp file, then rename.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    let _ = fs::remove_file(&tmp);
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
    f.write_all(bytes)
        .and_then(|()| f.sync_all())
        .map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())
}

pub fn edge_name(edge: Edge) -> &'static str {
    match edge {
        Edge::Left => "left",
        Edge::Right => "right",
        Edge::Top => "top",
        Edge::Bottom => "bottom",
    }
}

pub fn parse_edge(s: &str) -> Option<Edge> {
    match s {
        "left" => Some(Edge::Left),
        "right" => Some(Edge::Right),
        "top" => Some(Edge::Top),
        "bottom" => Some(Edge::Bottom),
        _ => None,
    }
}

pub fn hex_id(id: &DeviceId) -> String {
    id.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

pub fn parse_hex_id(s: &str) -> Option<DeviceId> {
    let s = s.trim();
    if s.len() != 64 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (slot, pair) in out.iter_mut().zip(s.as_bytes().as_chunks::<2>().0) {
        *slot = u8::from_str_radix(std::str::from_utf8(pair.as_slice()).ok()?, 16).ok()?;
    }
    Some(DeviceId::from_bytes(out))
}

/// One line: `<key hex> <edge> <name>`. The name may contain spaces.
fn format_partner(p: &Partner) -> String {
    format!(
        "{} {} {}\n",
        hex_id(&p.id),
        edge_name(p.edge),
        p.name.replace(['\n', '\r'], " ")
    )
}

fn parse_partner(line: &str) -> Option<Partner> {
    let mut parts = line.splitn(3, ' ');
    let id = parse_hex_id(parts.next()?)?;
    let edge = parse_edge(parts.next()?)?;
    let name: String = parts
        .next()
        .unwrap_or("")
        .chars()
        .take(MAX_NAME_LEN)
        .collect();
    Some(Partner { id, name, edge })
}

/// This machine's name, shown to other devices.
pub fn device_name() -> String {
    fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Hopwyn device".to_owned())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn temp_store() -> Store {
        let n: u64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .try_into()
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("hopwyn-test-{}-{n}", std::process::id()));
        Store::open(dir).unwrap()
    }

    #[test]
    fn identity_is_created_once_and_private() {
        let s = temp_store();
        let a = s.identity("Desk").unwrap();
        let b = s.identity("Desk").unwrap();
        assert_eq!(a.id(), b.id());
        let mode = fs::metadata(s.dir.join("identity"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn readable_secret_is_refused() {
        let s = temp_store();
        s.identity("Desk").unwrap();
        fs::set_permissions(s.dir.join("identity"), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(s.identity("Desk").is_err());
    }

    fn partner(name: &str, edge: Edge) -> Partner {
        Partner {
            id: Identity::generate("x").id(),
            name: name.into(),
            edge,
        }
    }

    #[test]
    fn partners_round_trip_and_forget() {
        let s = temp_store();
        assert!(s.partners().is_empty());
        let list = vec![
            partner("Living room PC", Edge::Left),
            partner("Laptop", Edge::Top),
        ];
        s.save_partners(&list).unwrap();
        assert_eq!(s.partners(), list);
        s.save_partners(&[]).unwrap();
        assert!(s.partners().is_empty());
    }

    #[test]
    fn old_single_partner_file_is_read_and_replaced() {
        let s = temp_store();
        let p = partner("Desk", Edge::Right);
        write_private(&s.dir.join("partner"), format_partner(&p).as_bytes()).unwrap();
        assert_eq!(s.partners(), vec![p.clone()]);
        s.save_partners(std::slice::from_ref(&p)).unwrap();
        assert!(!s.dir.join("partner").exists());
        assert_eq!(s.partners(), vec![p]);
    }

    #[test]
    fn duplicate_edges_and_bad_lines_are_skipped() {
        let s = temp_store();
        let (a, b) = (partner("A", Edge::Left), partner("B", Edge::Left));
        let text = format!(
            "{}nonsense here\n{}",
            format_partner(&a),
            format_partner(&b)
        );
        write_private(&s.dir.join("partners"), text.as_bytes()).unwrap();
        assert_eq!(s.partners(), vec![a]);
    }

    #[test]
    fn restore_token_round_trips() {
        let s = temp_store();
        assert_eq!(s.restore_token(), None);
        s.save_restore_token("abc-123").unwrap();
        assert_eq!(s.restore_token().as_deref(), Some("abc-123"));
    }
}
