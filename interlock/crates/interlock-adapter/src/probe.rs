use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Finds and runs host binaries. Pure parsing lives in each adapter so it can
/// be tested against captured output.
#[derive(Debug, Clone, Default)]
pub struct Probe {
    /// Overrides by host name, for example from `INTERLOCK_COPILOT_BIN`.
    pub overrides: Vec<(String, PathBuf)>,
}

impl Probe {
    pub fn from_env() -> Probe {
        let mut overrides = Vec::new();
        for (host, var) in [("copilot", "INTERLOCK_COPILOT_BIN"), ("claude-code", "INTERLOCK_CLAUDE_BIN")] {
            if let Some(path) = std::env::var_os(var) {
                overrides.push((host.to_string(), PathBuf::from(path)));
            }
        }
        Probe { overrides }
    }

    pub fn locate(&self, host: &str, binary: &str) -> Option<PathBuf> {
        if let Some((_, p)) = self.overrides.iter().find(|(h, _)| h == host) {
            return p.exists().then(|| p.clone());
        }
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path).map(|dir| dir.join(binary)).find(|p| p.is_file())
    }

    /// Runs a command and returns stdout and stderr together. `None` if it could not start.
    pub fn run(&self, bin: &PathBuf, args: &[&str]) -> Option<(bool, String)> {
        let out = Command::new(bin).args(args).stdin(Stdio::null()).env("NO_COLOR", "1").output().ok()?;
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        Some((out.status.success(), text))
    }
}
