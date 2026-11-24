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
//! - `addresses`: where each paired device was last reached, one per line:
//!   key, then its addresses, newest first. Lets devices reconnect on
//!   networks where they can't find each other on their own.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::net::SocketAddr;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use hopwyn_pair::{DeviceId, Identity, MAX_NAME_LEN};
use hopwyn_proto::Edge;

/// Most computers one can be paired with. Only one per screen edge can be
/// online at a time; computers that are never on together (a home and an
/// office desktop) can share a side.
pub const MAX_PARTNERS: usize = 16;

/// Most addresses remembered for one paired device (home, office, ...).
pub const MAX_ADDRESSES: usize = 4;

/// Where each paired device was last reached, newest first.
pub type Addresses = HashMap<DeviceId, Vec<SocketAddr>>;

/// Puts `addr` first in `id`'s list, dropping the oldest beyond
/// [`MAX_ADDRESSES`]. Returns whether the list changed.
pub fn remember_address(map: &mut Addresses, id: DeviceId, addr: SocketAddr) -> bool {
    let list = map.entry(id).or_default();
    if list.first() == Some(&addr) {
        return false;
    }
    list.retain(|a| *a != addr);
    list.insert(0, addr);
    list.truncate(MAX_ADDRESSES);
    true
}

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
    /// Send text copied here to paired devices, and take theirs. Off
    /// unless the user turns it on.
    pub clipboard: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            sharing: true,
            ask: AskWhen::Online,
            clipboard: false,
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
                Some(("clipboard", v)) => s.clipboard = v.trim() == "on",
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
            "sharing={}\nask={}\nclipboard={}\n",
            if s.sharing { "on" } else { "off" },
            match s.ask {
                AskWhen::Online => "online",
                AskWhen::Startup => "startup",
            },
            if s.clipboard { "on" } else { "off" }
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

    /// Where paired devices were last reached. A missing or damaged file
    /// is an empty list: bad lines and bad addresses are skipped.
    pub fn addresses(&self) -> Addresses {
        let text = read_private(&self.dir.join("addresses"))
            .ok()
            .flatten()
            .and_then(|b| String::from_utf8(b).ok())
            .unwrap_or_default();
        let mut out = Addresses::new();
        for line in text.lines() {
            let mut parts = line.split_whitespace();
            let Some(id) = parts.next().and_then(parse_hex_id) else {
                continue;
            };
            let mut addrs: Vec<SocketAddr> = Vec::new();
            for a in parts.filter_map(|a| a.parse::<SocketAddr>().ok()) {
                if addrs.len() < MAX_ADDRESSES && !addrs.contains(&a) {
                    addrs.push(a);
                }
            }
            if !addrs.is_empty() && out.len() < MAX_PARTNERS {
                out.entry(id).or_insert(addrs);
            }
        }
        out
    }

    pub fn save_addresses(&self, map: &Addresses) -> Result<(), String> {
        let mut lines: Vec<String> = map
            .iter()
            .filter(|(_, addrs)| !addrs.is_empty())
            .map(|(id, addrs)| {
                let list: Vec<String> = addrs.iter().map(SocketAddr::to_string).collect();
                format!("{} {}\n", hex_id(id), list.join(" "))
            })
            .collect();
        lines.sort();
        write_private(&self.dir.join("addresses"), lines.concat().as_bytes())
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
            clipboard: true,
        };
        s.save_settings(off).unwrap();
        assert_eq!(s.settings(), off);
    }

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn addresses_round_trip() {
        let s = temp_store();
        assert!(s.addresses().is_empty());
        let (a, b) = (Identity::generate("a").id(), Identity::generate("b").id());
        let mut map = Addresses::new();
        map.insert(a, vec![addr("192.168.1.20:4242"), addr("10.0.0.7:4242")]);
        map.insert(b, vec![addr("192.168.1.31:4242")]);
        s.save_addresses(&map).unwrap();
        assert_eq!(s.addresses(), map);
    }

    #[test]
    fn damaged_address_file_is_read_as_far_as_it_makes_sense() {
        let s = temp_store();
        let a = Identity::generate("a").id();
        let text = format!(
            "nonsense\n{} not-an-address 192.168.1.20:4242 192.168.1.20:4242\n\x01\x02 ::\n{}\n",
            hex_id(&a),
            hex_id(&Identity::generate("b").id()),
        );
        write_private(&s.dir.join("addresses"), text.as_bytes()).unwrap();
        let map = s.addresses();
        assert_eq!(map.len(), 1);
        assert_eq!(map.get(&a), Some(&vec![addr("192.168.1.20:4242")]));
        write_private(&s.dir.join("addresses"), &[0xff, 0xfe, 0x00]).unwrap();
        assert!(s.addresses().is_empty());
    }

    #[test]
    fn newest_address_goes_first_and_the_oldest_falls_off() {
        let id = Identity::generate("a").id();
        let mut map = Addresses::new();
        for n in 1..=5 {
            assert!(remember_address(
                &mut map,
                id,
                addr(&format!("10.0.0.{n}:4242"))
            ));
        }
        let list = map.get(&id).unwrap();
        assert_eq!(list.len(), MAX_ADDRESSES);
        assert_eq!(list.first(), Some(&addr("10.0.0.5:4242")));
        assert!(!list.contains(&addr("10.0.0.1:4242")));
        // Already first: nothing to save.
        assert!(!remember_address(&mut map, id, addr("10.0.0.5:4242")));
        // Seen before: moves to the front without a duplicate.
        assert!(remember_address(&mut map, id, addr("10.0.0.3:4242")));
        let list = map.get(&id).unwrap();
        assert_eq!(list.first(), Some(&addr("10.0.0.3:4242")));
        assert_eq!(list.len(), MAX_ADDRESSES);
    }

    #[test]
    fn restore_token_round_trips() {
        let s = temp_store();
        assert_eq!(s.restore_token(), None);
        s.save_restore_token("abc-123").unwrap();
        assert_eq!(s.restore_token().as_deref(), Some("abc-123"));
    }
}
