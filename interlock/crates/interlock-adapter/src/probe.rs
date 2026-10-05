use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Finds and runs host binaries. Pure parsing lives in each adapter so it can
/// be tested against captured output.
#[derive(Debug, Clone)]
pub struct Probe {
    /// Overrides by host name, for example from `INTERLOCK_COPILOT_BIN`.
    pub overrides: Vec<(String, PathBuf)>,
    /// How long one probe may take before it is killed.
    pub timeout: Duration,
}

impl Default for Probe {
    fn default() -> Self {
        Probe { overrides: vec![], timeout: Duration::from_secs(30) }
    }
}

impl Probe {
    pub fn from_env() -> Probe {
        let mut overrides = Vec::new();
        for (host, var) in [("copilot", "INTERLOCK_COPILOT_BIN"), ("claude-code", "INTERLOCK_CLAUDE_BIN")] {
            if let Some(path) = std::env::var_os(var) {
                overrides.push((host.to_string(), PathBuf::from(path)));
            }
        }
        Probe { overrides, ..Probe::default() }
    }

    pub fn locate(&self, host: &str, binary: &str) -> Option<PathBuf> {
        if let Some((_, p)) = self.overrides.iter().find(|(h, _)| h == host) {
            return p.exists().then(|| p.clone());
        }
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path).map(|dir| dir.join(binary)).find(|p| p.is_file())
    }

    /// Runs a command and returns stdout and stderr together. `None` if it
    /// could not start or did not finish within the timeout; a hung probe is
    /// killed with its whole process group.
    pub fn run(&self, bin: &PathBuf, args: &[&str]) -> Option<(bool, String)> {
        let mut cmd = Command::new(bin);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("NO_COLOR", "1")
            .env("COPILOT_AUTO_UPDATE", "false");
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        let mut child = cmd.spawn().ok()?;
        let drain = |mut pipe: Box<dyn Read + Send>| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = pipe.read_to_end(&mut buf);
                buf
            })
        };
        let out = drain(Box::new(child.stdout.take()?));
        let err = drain(Box::new(child.stderr.take()?));
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().ok()? {
                break status;
            }
            if started.elapsed() >= self.timeout {
                #[cfg(unix)]
                let _ = Command::new("kill")
                    .args(["-s", "KILL", "--", &format!("-{}", child.id())])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut text = String::from_utf8_lossy(&out.join().unwrap_or_default()).into_owned();
        text.push_str(&String::from_utf8_lossy(&err.join().unwrap_or_default()));
        Some((status.success(), text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hung_probe_is_killed() {
        let probe = Probe { timeout: Duration::from_millis(300), ..Probe::default() };
        let started = Instant::now();
        assert!(probe.run(&PathBuf::from("/bin/sh"), &["-c", "sleep 30"]).is_none());
        assert!(started.elapsed() < Duration::from_secs(5));
        let (ok, out) = probe.run(&PathBuf::from("/bin/sh"), &["-c", "echo hi; echo err >&2"]).unwrap();
        assert!(ok && out.contains("hi") && out.contains("err"));
    }
}
