//! `.interlock/config.toml`, settings for runs in one checkout. `[pins]`
//! names the version each host must have. Hosts ship often and change
//! defaults that move results, so a run on any other version is blocked
//! before a session starts.
//!
//! ```toml
//! [pins]
//! copilot = "1.0.91"
//! claude-code = "2.1.289"
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    /// Host name to exact version.
    #[serde(default)]
    pub pins: BTreeMap<String, String>,
}

impl Config {
    /// Where the config lives, beside the store.
    pub fn path(dir: &Path) -> PathBuf {
        dir.join("config.toml")
    }

    /// Reads the config in `dir`. A missing file is an empty config; other
    /// sections are left for whoever owns them.
    pub fn load(dir: &Path) -> Result<Config, String> {
        let path = Config::path(dir);
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    pub fn pin(&self, host: &str) -> Option<&str> {
        self.pins.get(host).map(String::as_str)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PinState {
    Unpinned,
    Match,
    Mismatch,
    /// Pinned, but the installed version could not be read.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PinStatus {
    pub status: PinState,
    pub pinned: Option<String>,
    pub installed: Option<String>,
}

impl PinStatus {
    pub fn of(pinned: Option<&str>, installed: Option<&str>) -> PinStatus {
        let status = match (pinned, installed) {
            (None, _) => PinState::Unpinned,
            (Some(_), None) => PinState::Unknown,
            (Some(p), Some(i)) if p.trim() == i.trim() => PinState::Match,
            (Some(_), Some(_)) => PinState::Mismatch,
        };
        PinStatus { status, pinned: pinned.map(str::to_string), installed: installed.map(str::to_string) }
    }

    /// Why a run must not start on this host, if it must not.
    pub fn refusal(&self, host: &str) -> Option<String> {
        let pinned = self.pinned.as_deref().unwrap_or_default();
        match self.status {
            PinState::Mismatch => Some(format!(
                "{host} {} is installed, but .interlock/config.toml pins {pinned}; install the pinned version or change the pin",
                self.installed.as_deref().unwrap_or_default()
            )),
            PinState::Unknown => Some(format!(
                "{host} is pinned to {pinned} in .interlock/config.toml, but its version could not be read"
            )),
            PinState::Unpinned | PinState::Match => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_are_read_from_the_config_and_compared_exactly() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Config::load(dir.path()).unwrap().pins.is_empty(), "no file, no pins");
        std::fs::write(
            Config::path(dir.path()),
            "[pins]\ncopilot = \"1.0.91\"\nclaude-code = \"2.1.289\"\n\n[forge]\nremote = \"origin\"\n",
        )
        .unwrap();
        let config = Config::load(dir.path()).unwrap();
        assert_eq!(config.pin("copilot"), Some("1.0.91"));
        assert_eq!(config.pin("claude-code"), Some("2.1.289"));

        assert_eq!(PinStatus::of(None, Some("1.0.92")).status, PinState::Unpinned);
        assert_eq!(PinStatus::of(Some("1.0.91"), Some("1.0.91")).status, PinState::Match);
        let off = PinStatus::of(Some("1.0.91"), Some("1.0.92"));
        assert_eq!(off.status, PinState::Mismatch);
        let why = off.refusal("copilot").unwrap();
        assert!(why.contains("copilot 1.0.92 is installed, but .interlock/config.toml pins 1.0.91"), "{why}");
        assert!(PinStatus::of(Some("1.0.91"), None).refusal("copilot").is_some());
        assert!(PinStatus::of(Some("1.0.91"), Some("1.0.91")).refusal("copilot").is_none());

        std::fs::write(Config::path(dir.path()), "[pins\n").unwrap();
        assert!(Config::load(dir.path()).is_err());
    }
}
