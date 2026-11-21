//! What the app remembers between runs, under `~/.config/hopwyn/`:
//!
//! - `identity`: this device's secret key (mode 0600). TEMPORARY until the
//!   OS key store is wired up, as SECURITY.md notes.
//! - `partners`: the paired devices, one per line: key, which edge of this
//!   screen it sits beyond, its name. (Older builds wrote one to `partner`.)
//! - `restore-token`: the desktop's remote-control permission token (0600),
//!   so the remote-control dialog is shown once, not on every handoff.
//! - `settings`: `key=value` lines: `sharing` (on/off) and `ask`
//!   (`online` or `startup`: when to ask for the input-capture permission).

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use hopwyn_pair::{DeviceId, Identity, MAX_NAME_LEN};
use hopwyn_proto::Edge;

/// Most computers one can be paired with. Only one per screen edge can be
/// online at a time; computers that are never on together (a home and an
/// office desktop) can share a side.
pub const MAX_PARTNERS: usize = 16;

/// When to ask the desktop for the input-capture permission. GNOME 50 and
/// older forget it at logout, so this decides when the dialog appears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskWhen {
    /// Only once a paired computer is online (no dialog at login otherwise).
    Online,
    /// As soon as Hopwyn starts.
    Startup,
}

/// User preferences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// Share this mouse and keyboard at all. Off: no permissions are asked,
    /// and other computers can't take control of this one.
    pub sharing: bool,
    pub ask: AskWhen,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            sharing: true,
            ask: AskWhen::Online,
        }
    }
}

/// A device this one shares its mouse and keyboard with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partner {
    pub id: DeviceId,
    pub name: String,
    /// The edge of THIS screen beyond which the partner's screen sits.
    pub edge: Edge,
    /// How far along that edge the partner's screen starts, in this
    /// screen's logical pixels (see `layout`).
    pub offset: i32,
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

    /// The paired devices (several may share a side). Bad lines are skipped.
    pub fn partners(&self) -> Vec<Partner> {
        let text = fs::read_to_string(self.dir.join("partners"))
            .or_else(|_| fs::read_to_string(self.dir.join("partner")))
            .unwrap_or_default();
        let mut out: Vec<Partner> = Vec::new();
        for p in text.lines().filter_map(parse_partner) {
            if out.len() < MAX_PARTNERS && !out.iter().any(|q| q.id == p.id) {
                out.push(p);
            }
        }
        out
    }

    pub fn settings(&self) -> Settings {
        let mut s = Settings::default();
        let text = fs::read_to_string(self.dir.join("settings")).unwrap_or_default();
        for line in text.lines() {
            match line.trim().split_once('=') {
                Some(("sharing", v)) => s.sharing = v.trim() != "off",
                Some(("ask", v)) => {
                    s.ask = if v.trim() == "startup" {
                        AskWhen::Startup
                    } else {
                        AskWhen::Online
                    };
                }
                _ => {}
            }
        }
        s
    }

    pub fn save_settings(&self, s: Settings) -> Result<(), String> {
        let text = format!(
            "sharing={}\nask={}\n",
            if s.sharing { "on" } else { "off" },
            match s.ask {
                AskWhen::Online => "online",
                AskWhen::Startup => "startup",
            }
        );
        write_private(&self.dir.join("settings"), text.as_bytes())
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

/// One line: `<key hex> <edge>[@offset] <name>`. The name may contain spaces.
fn format_partner(p: &Partner) -> String {
    let side = if p.offset == 0 {
        edge_name(p.edge).to_owned()
    } else {
        format!("{}@{}", edge_name(p.edge), p.offset)
    };
    format!(
        "{} {side} {}\n",
        hex_id(&p.id),
        p.name.replace(['\n', '\r'], " ")
    )
}

fn parse_partner(line: &str) -> Option<Partner> {
    let mut parts = line.splitn(3, ' ');
    let id = parse_hex_id(parts.next()?)?;
    let side = parts.next()?;
    let (edge, offset) = match side.split_once('@') {
        Some((e, o)) => (
            parse_edge(e)?,
            o.parse::<i32>().ok()?.clamp(-65_536, 65_536),
        ),
        None => (parse_edge(side)?, 0),
    };
    let name: String = parts
        .next()
        .unwrap_or("")
        .chars()
        .take(MAX_NAME_LEN)
        .collect();
    Some(Partner {
        id,
        name,
        edge,
        offset,
    })
}

/// This machine's name, shown to other devices.
/// This computer's name as others see it: `HOPWYN_NAME` if set (for test
/// copies), else the hostname.
pub fn device_name() -> String {
    if let Some(n) = std::env::var("HOPWYN_NAME")
        .ok()
        .filter(|n| !n.trim().is_empty())
    {
        return n.trim().to_owned();
    }
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
            offset: 0,
        }
    }

    #[test]
    fn offsets_round_trip_and_old_lines_mean_zero() {
        let s = temp_store();
        let mut p = partner("Laptop beside tall monitor", Edge::Right);
        p.offset = -360;
        s.save_partners(std::slice::from_ref(&p)).unwrap();
        assert_eq!(s.partners(), vec![p.clone()]);
        let old = format!("{} left Desk\n", hex_id(&p.id));
        write_private(&s.dir.join("partners"), old.as_bytes()).unwrap();
        assert_eq!(s.partners().first().unwrap().offset, 0);
        assert_eq!(s.partners().first().unwrap().edge, Edge::Left);
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
    fn shared_sides_are_kept_and_bad_lines_and_duplicates_skipped() {
        let s = temp_store();
        let (a, b) = (partner("Home", Edge::Left), partner("Office", Edge::Left));
        let text = format!(
            "{}nonsense here\n{}{}",
            format_partner(&a),
            format_partner(&b),
            format_partner(&a)
        );
        write_private(&s.dir.join("partners"), text.as_bytes()).unwrap();
        assert_eq!(s.partners(), vec![a, b]);
    }

    #[test]
    fn settings_default_and_round_trip() {
        let s = temp_store();
        assert_eq!(s.settings(), Settings::default());
        let off = Settings {
            sharing: false,
            ask: AskWhen::Startup,
        };
        s.save_settings(off).unwrap();
        assert_eq!(s.settings(), off);
    }

    #[test]
    fn restore_token_round_trips() {
        let s = temp_store();
        assert_eq!(s.restore_token(), None);
        s.save_restore_token("abc-123").unwrap();
        assert_eq!(s.restore_token().as_deref(), Some("abc-123"));
    }
}
