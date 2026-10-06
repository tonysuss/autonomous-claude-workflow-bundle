use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
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
        Probe { overrides: vec![], timeout: Duration::from_secs(15) }
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

    /// Runs a command and returns stdout and stderr together, trying once more
    /// if the first try hangs. `None` if neither try finished in time.
    pub fn run(&self, bin: &PathBuf, args: &[&str]) -> Option<(bool, String)> {
        self.run_once(bin, args).or_else(|| self.run_once(bin, args))
    }

    /// One try. A hung probe is killed with its whole process group, and once
    /// the probe exits its output is read for at most two seconds more, so a
    /// background process that keeps the pipes open cannot hold interlock.
    fn run_once(&self, bin: &PathBuf, args: &[&str]) -> Option<(bool, String)> {
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
        let group = child.id();
        let kill_group = move || {
            #[cfg(unix)]
            let _ = Command::new("kill")
                .args(["-s", "KILL", "--", &format!("-{group}")])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        };
        // Readers append as they go, so whatever arrived is kept even if a pipe never closes.
        let drain = |mut pipe: Box<dyn Read + Send>| {
            let buf = Arc::new(Mutex::new(Vec::new()));
            let (sink, (tx, rx)) = (buf.clone(), mpsc::channel::<()>());
            std::thread::spawn(move || {
                let mut chunk = [0u8; 8192];
                while let Ok(n) = pipe.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    sink.lock().map(|mut b| b.extend_from_slice(&chunk[..n])).ok();
                }
                let _ = tx.send(());
            });
            (buf, rx)
        };
        let (out, out_done) = drain(Box::new(child.stdout.take()?));
        let (err, err_done) = drain(Box::new(child.stderr.take()?));
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().ok()? {
                break status;
            }
            if started.elapsed() >= self.timeout {
                kill_group();
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let grace = Duration::from_secs(2);
        if out_done.recv_timeout(grace).is_err() | err_done.recv_timeout(grace).is_err() {
            kill_group();
        }
        let take = |b: &Arc<Mutex<Vec<u8>>>| {
            String::from_utf8_lossy(&b.lock().map(|b| b.clone()).unwrap_or_default()).into_owned()
        };
        let mut text = take(&out);
        text.push_str(&take(&err));
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

    #[test]
    fn a_background_process_holding_the_pipes_cannot_hold_the_probe() {
        let probe = Probe { timeout: Duration::from_secs(10), ..Probe::default() };
        let started = Instant::now();
        let (ok, out) = probe.run(&PathBuf::from("/bin/sh"), &["-c", "echo version 1.0; sleep 60 & exit 0"]).unwrap();
        assert!(ok && out.contains("version 1.0"), "{out}");
        assert!(started.elapsed() < Duration::from_secs(6), "took {:?}", started.elapsed());
    }
}
