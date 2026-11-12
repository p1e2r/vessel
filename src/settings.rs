use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// User-tweakable appearance and behaviour. Stored as JSON in
/// `~/Library/Application Support/Vessel/settings.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Monospace font used by terminal tabs.
    pub font_family: String,
    pub font_size: f32,
    /// Line height as a multiple of the font size.
    pub line_height: f32,
    /// Window background opacity, 0.0 (fully clear) – 1.0 (solid).
    pub opacity: f32,
    /// Blur whatever is behind the window (macOS vibrancy).
    pub blur: bool,
    /// Shell for local tabs. Started as a login shell.
    pub shell: String,
    /// Treat Option as Meta (sends ESC-prefixed keys) instead of typing special characters.
    pub option_as_meta: bool,
    /// Lines of scrollback kept per tab.
    pub scrollback: usize,
    /// Copy text to the clipboard as soon as it is selected.
    pub copy_on_select: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_family: "Menlo".into(),
            font_size: 13.0,
            line_height: 1.3,
            opacity: 0.80,
            blur: true,
            shell: "/bin/zsh".into(),
            option_as_meta: false,
            scrollback: 10_000,
            copy_on_select: false,
        }
    }
}

pub fn config_dir() -> PathBuf {
    let dir = dirs::config_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config"))
        .join("Vessel");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

impl Settings {
    fn path() -> PathBuf {
        config_dir().join("settings.json")
    }

    pub fn load() -> Self {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => {
                let s = Self::default();
                s.save();
                s
            }
        }
    }

    pub fn save(&self) {
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(Self::path(), json);
        }
    }
}

impl gpui::Global for Settings {}
