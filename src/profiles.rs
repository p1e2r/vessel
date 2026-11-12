//! Saved connections. Connection details live in `profiles.json`; passwords and
//! key passphrases live in the macOS Keychain (never on disk in plain text).

use crate::settings::config_dir;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const KEYCHAIN_SERVICE: &str = "dev.vessel.terminal";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Ssh,
    Rdp,
    Vnc,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Ssh => "SSH",
            Kind::Rdp => "RDP",
            Kind::Vnc => "VNC",
        }
    }
    pub fn default_port(self) -> u16 {
        match self {
            Kind::Ssh => 22,
            Kind::Rdp => 3389,
            Kind::Vnc => 5900,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScaleMode {
    /// Scale the remote desktop to fit the tab, keeping aspect ratio.
    Fit,
    /// One remote pixel per screen pixel (scrolls/crops if larger).
    Actual,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum VncEncodingPref {
    /// Tight (JPEG) – best over slow links.
    Tight,
    /// ZRLE – lossless, good on LAN.
    Zrle,
    /// Raw – no compression, lowest CPU.
    Raw,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub host: String,
    pub port: u16,
    pub username: String,

    // SSH
    /// Private key file (e.g. ~/.ssh/id_ed25519). Empty = agent / default keys / password.
    pub key_path: String,
    /// Command to run instead of the login shell (optional).
    pub remote_command: String,

    // RDP
    pub domain: String,
    /// Network Level Authentication (CredSSP).
    pub nla: bool,
    pub color_depth: u32,
    pub show_wallpaper: bool,
    pub font_smoothing: bool,
    pub desktop_composition: bool,

    // RDP + VNC display
    /// Requested remote resolution. 0×0 = match the tab size when connecting.
    pub width: u16,
    pub height: u16,
    /// Frames per second the viewer redraws at (and, for VNC, how often it requests updates).
    pub fps: u16,
    pub scale: ScaleMode,
    /// Send ⌘ as Ctrl to the remote machine (⌘C / ⌘V copy-paste like on a Mac).
    pub cmd_as_ctrl: bool,

    // VNC
    pub vnc_encoding: VncEncodingPref,
    pub view_only: bool,
    pub shared: bool,

    /// Unix seconds; used to sort quick-connect.
    pub last_used: u64,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: String::new(),
            kind: Kind::Ssh,
            host: String::new(),
            port: 22,
            username: String::new(),
            key_path: String::new(),
            remote_command: String::new(),
            domain: String::new(),
            nla: true,
            color_depth: 32,
            show_wallpaper: false,
            font_smoothing: true,
            desktop_composition: false,
            width: 0,
            height: 0,
            fps: 60,
            scale: ScaleMode::Fit,
            cmd_as_ctrl: true,
            vnc_encoding: VncEncodingPref::Tight,
            view_only: false,
            shared: true,
            last_used: 0,
        }
    }
}

impl Profile {
    pub fn display_name(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.clone();
        }
        let mut s = String::new();
        if !self.username.is_empty() {
            s.push_str(&self.username);
            s.push('@');
        }
        s.push_str(&self.host);
        if self.port != self.kind.default_port() {
            s.push_str(&format!(":{}", self.port));
        }
        s
    }

    pub fn subtitle(&self) -> String {
        let mut s = format!("{} · ", self.kind.label());
        if !self.username.is_empty() {
            s.push_str(&self.username);
            s.push('@');
        }
        s.push_str(&self.host);
        s.push_str(&format!(":{}", self.port));
        s
    }

    pub fn password(&self) -> Option<String> {
        secret_get(&self.id)
    }
}

#[derive(Default, Serialize, Deserialize)]
pub struct ProfileStore {
    pub profiles: Vec<Profile>,
}

impl ProfileStore {
    fn path() -> PathBuf {
        config_dir().join("profiles.json")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let path = Self::path();
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(tmp, path);
            }
        }
    }

    pub fn upsert(&mut self, p: Profile) {
        if let Some(existing) = self.profiles.iter_mut().find(|x| x.id == p.id) {
            *existing = p;
        } else {
            self.profiles.push(p);
        }
        self.save();
    }

    pub fn remove(&mut self, id: &str) {
        self.profiles.retain(|p| p.id != id);
        secret_delete(id);
        self.save();
    }

    pub fn touch(&mut self, id: &str) {
        if let Some(p) = self.profiles.iter_mut().find(|p| p.id == id) {
            p.last_used = now();
        }
        self.save();
    }

    pub fn get(&self, id: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    /// Most recently used first, then alphabetical.
    pub fn sorted(&self) -> Vec<Profile> {
        let mut v = self.profiles.clone();
        v.sort_by(|a, b| {
            b.last_used
                .cmp(&a.last_used)
                .then_with(|| a.display_name().to_lowercase().cmp(&b.display_name().to_lowercase()))
        });
        v
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn secret_get(id: &str) -> Option<String> {
    keyring::Entry::new(KEYCHAIN_SERVICE, id)
        .ok()?
        .get_password()
        .ok()
        .filter(|s| !s.is_empty())
}

pub fn secret_set(id: &str, secret: &str) -> bool {
    match keyring::Entry::new(KEYCHAIN_SERVICE, id) {
        Ok(e) => {
            if secret.is_empty() {
                let _ = e.delete_credential();
                true
            } else {
                e.set_password(secret).is_ok()
            }
        }
        Err(_) => false,
    }
}

pub fn secret_delete(id: &str) {
    if let Ok(e) = keyring::Entry::new(KEYCHAIN_SERVICE, id) {
        let _ = e.delete_credential();
    }
}

/// Expand a leading `~/`.
pub fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        dirs::home_dir().unwrap_or_default().join(rest)
    } else {
        PathBuf::from(p)
    }
}

/// Parse a quick-connect string like `user@host:2222`, `rdp://admin@10.0.0.5`,
/// or `vnc://mac-mini.local`.
pub fn parse_quick(input: &str) -> Option<Profile> {
    let s = input.trim();
    if s.is_empty() || s.contains(' ') {
        return None;
    }
    let (kind, rest) = if let Some(r) = s.strip_prefix("ssh://") {
        (Kind::Ssh, r)
    } else if let Some(r) = s.strip_prefix("rdp://") {
        (Kind::Rdp, r)
    } else if let Some(r) = s.strip_prefix("vnc://") {
        (Kind::Vnc, r)
    } else {
        (Kind::Ssh, s)
    };
    let (user, hostport) = match rest.rsplit_once('@') {
        Some((u, h)) => (u.to_string(), h),
        None => (String::new(), rest),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => (h.to_string(), p.parse().ok()?),
        _ => (hostport.to_string(), kind.default_port()),
    };
    if host.is_empty() {
        return None;
    }
    Some(Profile { kind, host, port, username: user, ..Default::default() })
}
