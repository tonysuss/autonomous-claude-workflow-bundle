//! `.interlock/config.toml`, settings for runs in one checkout. `[pins]`
//! names the version each host must have. Hosts ship often and change
//! defaults that move results, so a run on any other version is blocked
//! before a session starts.
//!
//! `[env] pass` names variables from interlock's own environment that every
//! session may have, beyond the built-in allowlist (`interlock_adapter::env`):
//! exact names, or prefixes ending in `*`.
//!
//! ```toml
//! [pins]
//! copilot = "1.0.91"
//! claude-code = "2.1.289"
//!
//! [env]
//! pass = ["MY_TOOL_HOME", "PIP_*"]
//!
//! [host_policy.copilot]
//! deny = ["web", "shell:curl"]        # host-neutral tools no session on this host may use
//! classes = ["read", "local_reversible"]  # the action classes this host allows
//! ```
//!
//! `[host_policy.<host>]` is the host side of the effective grant (user grant
//! ∩ host policy ∩ task needs). Without it a host allows every grantable
//! class and tool.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use interlock_core::grants::HostPolicy;
use interlock_schema::ActionClass;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    /// Host name to exact version.
    #[serde(default)]
    pub pins: BTreeMap<String, String>,
    #[serde(default)]
    pub env: EnvConfig,
    /// Host name to what interlock may grant on that host.
    #[serde(default)]
    pub host_policy: BTreeMap<String, HostPolicyConfig>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostPolicyConfig {
    /// Host-neutral tools no session on this host may use.
    #[serde(default)]
    pub deny: Vec<String>,
    /// The action classes this host allows; every grantable class when absent.
    #[serde(default)]
    pub classes: Option<Vec<ActionClass>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvConfig {
    /// Extra variables every session may have.
    #[serde(default)]
    pub pass: Vec<String>,
}

impl Config {
    /// Where the config lives, beside the store.
    pub fn path(dir: &Path) -> PathBuf {
        dir.join("config.toml")
    }

    /// Reads the config in `dir`. A missing file is an empty config. Other
    /// sections are left for whoever owns them, but `[pins]` may name only
    /// known hosts.
    pub fn load(dir: &Path) -> Result<Config, String> {
        let path = Config::path(dir);
        let config: Config = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let known: Vec<&str> = interlock_adapter::hosts().iter().map(|h| h.name()).collect();
        if let Some(host) = config.host_policy.keys().find(|h| !known.contains(&h.as_str())) {
            return Err(format!(
                "{}: [host_policy] names an unknown host `{host}`; known hosts: {}",
                path.display(),
                known.join(", ")
            ));
        }
        if let Some(host) = config.pins.keys().find(|h| !known.contains(&h.as_str())) {
            return Err(format!(
                "{}: [pins] names an unknown host `{host}`; known hosts: {}",
                path.display(),
                known.join(", ")
            ));
        }
        if let Some((host, _)) = config.pins.iter().find(|(_, v)| v.trim().is_empty()) {
            return Err(format!("{}: [pins] {host} has an empty version", path.display()));
        }
        Ok(config)
    }

    pub fn pin(&self, host: &str) -> Option<&str> {
        self.pins.get(host).map(String::as_str)
    }

    /// The host side of the effective grant: open, narrowed by `[host_policy.<host>]`.
    pub fn host_policy(&self, host: &str) -> HostPolicy {
        let mut policy = HostPolicy::open();
        if let Some(limits) = self.host_policy.get(host) {
            if let Some(classes) = &limits.classes {
                policy.action_classes.retain(|c| classes.contains(c));
            }
            for d in &limits.deny {
                if !policy.tools.deny.contains(d) {
                    policy.tools.deny.push(d.clone());
                }
            }
        }
        policy
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
        std::fs::write(Config::path(dir.path()), "[pins]\nclaude = \"2.1.289\"\n").unwrap();
        let err = Config::load(dir.path()).unwrap_err();
        assert!(err.contains("unknown host `claude`; known hosts: copilot, claude-code"), "{err}");
        std::fs::write(Config::path(dir.path()), "[env]\npass = [\"MY_TOOL_*\"]\nkeep = 1\n").unwrap();
        assert!(Config::load(dir.path()).is_err(), "unknown keys in [env] are refused");
        std::fs::write(Config::path(dir.path()), "[env]\npass = [\"MY_TOOL_*\"]\n").unwrap();
        assert_eq!(Config::load(dir.path()).unwrap().env.pass, vec!["MY_TOOL_*"]);
    }
}
