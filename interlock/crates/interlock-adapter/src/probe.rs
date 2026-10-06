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
    /// How long one `--version` or `--help` query may take. These print and
    /// exit in about a second, but Copilot CLI 1.0.91's `--help` was seen to
    /// hang (once in 80 calls), so a query that times out is tried again.
    pub query_timeout: Duration,
    /// How many times a query is tried before it counts as failed.
    pub query_tries: u32,
}

impl Default for Probe {
    fn default() -> Self {
        Probe {
            overrides: vec![],
            timeout: Duration::from_secs(30),
            query_timeout: Duration::from_secs(10),
            query_tries: 3,
        }
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

    /// Runs a read-only query such as `--version` or `--help`, trying again
    /// when an attempt hangs past `query_timeout`. `None` only if every try
    /// failed to start or hung.
    pub fn query(&self, bin: &PathBuf, args: &[&str]) -> Option<(bool, String)> {
        (0..self.query_tries.max(1)).find_map(|_| self.run_for(bin, args, self.query_timeout))
    }

    /// Runs a command and returns stdout and stderr together. `None` if it
    /// could not start or did not finish within the timeout; a hung probe is
    /// killed with its whole process group. Not retried, since it may do
    /// work (the sign-in check calls the model).
    pub fn run(&self, bin: &PathBuf, args: &[&str]) -> Option<(bool, String)> {
        self.run_for(bin, args, self.timeout)
    }

    fn run_for(&self, bin: &PathBuf, args: &[&str], timeout: Duration) -> Option<(bool, String)> {
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
            if started.elapsed() >= timeout {
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

    /// Copilot CLI's `--help` hangs now and then; one hang must not make the
    /// host look as if it lacked every flag.
    #[test]
    fn a_query_that_hangs_once_is_tried_again() {
        let dir = tempfile::tempdir().unwrap();
        let count = dir.path().join("count");
        let script = format!(
            "n=$(cat {c} 2>/dev/null || echo 0); echo $((n + 1)) > {c}; [ \"$n\" = 0 ] && sleep 30; echo '--prompt'",
            c = count.display()
        );
        let probe = Probe { query_timeout: Duration::from_millis(500), ..Probe::default() };
        let started = Instant::now();
        let (ok, out) = probe.query(&PathBuf::from("/bin/sh"), &["-c", &script]).expect("the second try answers");
        assert!(ok && out.contains("--prompt"), "{out}");
        assert_eq!(std::fs::read_to_string(&count).unwrap().trim(), "2");
        assert!(started.elapsed() < Duration::from_secs(10));

        let never = Probe { query_timeout: Duration::from_millis(200), query_tries: 2, ..Probe::default() };
        assert!(never.query(&PathBuf::from("/bin/sh"), &["-c", "sleep 30"]).is_none());
    }
}
