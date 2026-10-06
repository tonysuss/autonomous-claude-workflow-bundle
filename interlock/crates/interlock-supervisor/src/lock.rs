//! One controller per checkout. The lock is the operating system's: an
//! exclusive lock on `.interlock/supervisor.lock`, held by an open file for
//! as long as the controller lives and released by the kernel when it exits,
//! however it exits. The pid in the file is only for people reading it.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::run::{Result, RunError};

#[derive(Debug)]
pub struct ControllerLock {
    _file: File,
    pub path: PathBuf,
}

impl ControllerLock {
    /// Takes the controller lock for the store in `store_dir`, or fails at
    /// once if another process holds it.
    pub fn acquire(store_dir: &Path) -> Result<ControllerLock> {
        std::fs::create_dir_all(store_dir)
            .map_err(|e| RunError::Other(format!("cannot create {}: {e}", store_dir.display())))?;
        let path = store_dir.join("supervisor.lock");
        let io = |e: std::io::Error| RunError::Other(format!("cannot open {}: {e}", path.display()));
        let mut file =
            OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path).map_err(io)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                let holder = std::fs::read_to_string(&path).unwrap_or_default();
                let holder = holder.trim();
                let who = if holder.is_empty() { String::new() } else { format!(" (pid {holder})") };
                return Err(RunError::Other(format!(
                    "another supervisor{who} holds the controller lock at {}",
                    path.display()
                )));
            }
            Err(TryLockError::Error(e)) => return Err(io(e)),
        }
        // Informational only; the lock, not this number, decides.
        let _ = file.set_len(0);
        let _ = file.write_all(std::process::id().to_string().as_bytes());
        let _ = file.flush();
        Ok(ControllerLock { _file: file, path })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lock_is_exclusive_and_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let first = ControllerLock::acquire(dir.path()).unwrap();
        let err = ControllerLock::acquire(dir.path()).unwrap_err().to_string();
        assert!(err.contains("another supervisor"), "{err}");
        assert!(err.contains(&format!("pid {}", std::process::id())), "{err}");
        drop(first);
        ControllerLock::acquire(dir.path()).unwrap();
    }

    #[test]
    fn a_stale_pid_in_the_file_does_not_hold_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        // A live pid that does not hold the lock, then garbage: neither counts.
        std::fs::write(dir.path().join("supervisor.lock"), "1").unwrap();
        drop(ControllerLock::acquire(dir.path()).unwrap());
        std::fs::write(dir.path().join("supervisor.lock"), "not a pid").unwrap();
        ControllerLock::acquire(dir.path()).unwrap();
    }
}
