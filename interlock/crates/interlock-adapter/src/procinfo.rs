//! What interlock reads about other processes: each one's parent, process
//! group, start time and state, the environment it started with, and its
//! command line. On Linux from `/proc`; on macOS from libproc and `sysctl`.
//! Environments and command lines are read for the same user's processes,
//! which covers every process a session starts; parents and groups for any.

/// One process, as the operating system reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    pub parent: u32,
    pub group: u32,
    /// When the process started, in this platform's units: clock ticks since
    /// boot on Linux, microseconds since the epoch on macOS. Compare it only
    /// with start times read here, to tell a process from a later one that
    /// reuses its pid. macOS gives it only for the same user's processes.
    pub start: Option<u64>,
    /// It has exited and waits for its parent to collect it.
    pub zombie: bool,
}

/// Whether processes can be read here: on macOS always, on Linux when `/proc`
/// is mounted (a chroot or sandbox may leave it out). Where they cannot,
/// callers fall back to what a signal can tell them, and lose the rest.
pub fn supported() -> bool {
    imp::supported()
}

/// Every process id the system reports.
pub fn pids() -> Vec<u32> {
    imp::pids()
}

/// A process's parent, group, start time and state.
pub fn stat(pid: u32) -> Option<Stat> {
    imp::stat(pid)
}

/// The environment a process started with, as `NAME=value` entries. Later
/// changes a process makes to its own environment are not seen, on either
/// platform.
pub fn environ(pid: u32) -> Option<Vec<Vec<u8>>> {
    imp::environ(pid)
}

/// A process's command line, one entry per argument.
pub fn command_line(pid: u32) -> Option<Vec<String>> {
    imp::command_line(pid)
}

/// Whether a process started with `name` set to a non-empty value, or, with
/// `value`, to exactly that.
pub fn started_with(pid: u32, name: &str, value: Option<&str>) -> bool {
    let prefix = format!("{name}=");
    environ(pid).is_some_and(|env| {
        env.iter().any(|kv| match (kv.strip_prefix(prefix.as_bytes()), value) {
            (Some(v), Some(want)) => v == want.as_bytes(),
            (Some(v), None) => !v.is_empty(),
            (None, _) => false,
        })
    })
}

#[cfg(target_os = "linux")]
mod imp {
    use super::Stat;

    pub fn supported() -> bool {
        std::path::Path::new("/proc/self/stat").exists()
    }

    /// NUL-separated strings, without the empty ones.
    fn entries(raw: &[u8]) -> Vec<Vec<u8>> {
        raw.split(|b| *b == 0).filter(|s| !s.is_empty()).map(<[u8]>::to_vec).collect()
    }

    pub fn pids() -> Vec<u32> {
        let Ok(dir) = std::fs::read_dir("/proc") else { return vec![] };
        dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok()).collect()
    }

    pub fn stat(pid: u32) -> Option<Stat> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // The command name is in parentheses and may contain spaces; the
        // fields after it start at the state, the third field of proc(5).
        let fields: Vec<&str> = stat.get(stat.rfind(')')? + 1..)?.split_whitespace().collect();
        let state = fields.first()?.chars().next()?;
        Some(Stat {
            parent: fields.get(1)?.parse().ok()?,
            group: fields.get(2)?.parse().ok()?,
            start: Some(fields.get(19)?.parse().ok()?),
            zombie: matches!(state, 'Z' | 'X'),
        })
    }

    pub fn environ(pid: u32) -> Option<Vec<Vec<u8>>> {
        std::fs::read(format!("/proc/{pid}/environ")).ok().map(|raw| entries(&raw))
    }

    pub fn command_line(pid: u32) -> Option<Vec<String>> {
        let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        Some(entries(&raw).iter().map(|a| String::from_utf8_lossy(a).into_owned()).collect())
    }
}

// The only unsafe code in interlock: five calls into Apple's process API, each
// with the buffer it is given checked against the size it is told, and the
// zeroed structures they fill.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
#[warn(clippy::undocumented_unsafe_blocks)]
mod imp {
    use std::ffi::c_void;
    use std::mem::size_of;

    use nix::libc;

    use super::Stat;

    type Strings = Vec<Vec<u8>>;

    /// `SZOMB` in `<sys/proc.h>`.
    const ZOMBIE: u32 = 5;

    pub fn supported() -> bool {
        true
    }

    pub fn pids() -> Vec<u32> {
        // SAFETY: with no buffer, proc_listallpids only counts.
        let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        if n <= 0 {
            return vec![];
        }
        // Room for processes started between the count and the listing.
        let mut buf: Vec<libc::c_int> = vec![0; n as usize + 256];
        let bytes = (buf.len() * size_of::<libc::c_int>()) as libc::c_int;
        // SAFETY: the buffer holds `bytes` bytes of c_int, as the call requires.
        let got = unsafe { libc::proc_listallpids(buf.as_mut_ptr().cast::<c_void>(), bytes) };
        if got <= 0 {
            return vec![];
        }
        buf.truncate((got as usize).min(buf.len()));
        buf.into_iter().filter(|p| *p > 0).map(|p| p as u32).collect()
    }

    /// The full record, with the start time, for the same user's processes;
    /// the short one, without it, for anyone's. A non-zero `arg` asks for
    /// zombies too.
    pub fn stat(pid: u32) -> Option<Stat> {
        let pid = libc::pid_t::try_from(pid).ok()?;
        // SAFETY: proc_bsdinfo is plain data; all zeroes is a valid value.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = size_of::<libc::proc_bsdinfo>() as libc::c_int;
        // SAFETY: the buffer is one proc_bsdinfo of `size` bytes.
        let got = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 1, (&raw mut info).cast::<c_void>(), size) };
        if got == size {
            return Some(Stat {
                parent: info.pbi_ppid,
                group: info.pbi_pgid,
                start: Some(info.pbi_start_tvsec.saturating_mul(1_000_000).saturating_add(info.pbi_start_tvusec)),
                zombie: info.pbi_status == ZOMBIE,
            });
        }
        // SAFETY: proc_bsdshortinfo is plain data; all zeroes is a valid value.
        let mut short: libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
        let size = size_of::<libc::proc_bsdshortinfo>() as libc::c_int;
        // SAFETY: the buffer is one proc_bsdshortinfo of `size` bytes.
        let got = unsafe {
            libc::proc_pidinfo(pid, libc::PROC_PIDT_SHORTBSDINFO, 1, (&raw mut short).cast::<c_void>(), size)
        };
        (got == size).then_some(Stat {
            parent: short.pbsi_ppid,
            group: short.pbsi_pgid,
            start: None,
            zombie: short.pbsi_status == ZOMBIE,
        })
    }

    /// The process's argument area from `KERN_PROCARGS2`: argc, the
    /// executable's path, NUL padding, the arguments, then the environment,
    /// each string NUL-terminated.
    fn procargs(pid: u32) -> Option<Vec<u8>> {
        let pid = libc::c_int::try_from(pid).ok()?;
        let mut argmax: libc::c_int = 0;
        let mut size = size_of::<libc::c_int>();
        let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
        // SAFETY: the output buffer is one c_int and `size` says so.
        let ok = unsafe {
            libc::sysctl(mib.as_mut_ptr(), 2, (&raw mut argmax).cast::<c_void>(), &mut size, std::ptr::null_mut(), 0)
        };
        if ok != 0 || argmax <= 0 {
            return None;
        }
        let mut buf = vec![0u8; argmax as usize];
        let mut size = buf.len();
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        // SAFETY: the output buffer is `size` bytes long.
        let ok = unsafe {
            libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast::<c_void>(), &mut size, std::ptr::null_mut(), 0)
        };
        if ok != 0 {
            return None;
        }
        buf.truncate(size);
        Some(buf)
    }

    /// The arguments and the environment in a `KERN_PROCARGS2` area. The
    /// environment ends at the first empty string. As in ps(1), an empty
    /// first argument is taken for padding, which shifts the rest by one; and
    /// where the kernel leaves the environment out (a process whose code
    /// signature forbids reading it), it reads as empty, which is never a
    /// marker.
    pub(super) fn split(area: &[u8]) -> Option<(Strings, Strings)> {
        let argc = usize::try_from(i32::from_ne_bytes(area.get(..4)?.try_into().ok()?)).ok()?;
        let rest = area.get(4..)?;
        let path_end = rest.iter().position(|b| *b == 0)?;
        let rest = &rest[path_end..];
        let rest = &rest[rest.iter().position(|b| *b != 0).unwrap_or(rest.len())..];
        let mut strings = rest.split(|b| *b == 0);
        let args: Vec<Vec<u8>> = strings.by_ref().take(argc).map(<[u8]>::to_vec).collect();
        let env: Vec<Vec<u8>> = strings.take_while(|s| !s.is_empty()).map(<[u8]>::to_vec).collect();
        Some((args, env))
    }

    pub fn environ(pid: u32) -> Option<Vec<Vec<u8>>> {
        split(&procargs(pid)?).map(|(_, env)| env)
    }

    pub fn command_line(pid: u32) -> Option<Vec<String>> {
        let (args, _) = split(&procargs(pid)?)?;
        Some(args.iter().map(|a| String::from_utf8_lossy(a).into_owned()).collect())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod imp {
    use super::Stat;

    pub fn pids() -> Vec<u32> {
        vec![]
    }

    pub fn stat(_pid: u32) -> Option<Stat> {
        None
    }

    pub fn environ(_pid: u32) -> Option<Vec<Vec<u8>>> {
        None
    }

    pub fn command_line(_pid: u32) -> Option<Vec<String>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn this_process_is_seen_with_its_parent_group_and_start() {
        let me = std::process::id();
        assert!(pids().contains(&me));
        let s = stat(me).expect("this process");
        assert_eq!(s.parent, std::os::unix::process::parent_id());
        assert_eq!(s.group, nix::unistd::getpgrp().as_raw() as u32);
        assert!(supported());
        assert!(s.start.is_some_and(|t| t > 0) && !s.zombie);
        assert_eq!(stat(me).unwrap().start, s.start, "a start time does not move");
        // Another user's process (pid 1 is init or launchd) is seen too.
        assert!(stat(1).is_some());
        assert!(stat(u32::MAX - 1).is_none());
    }

    #[test]
    fn a_child_is_seen_with_the_environment_and_arguments_it_started_with() {
        let mut child = Command::new("sleep")
            .arg("30")
            .env_clear()
            .env("INTERLOCK_PROBE", "marker-1")
            .env("OTHER", "x=y")
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let s = stat(pid).expect("the child");
        assert_eq!(s.parent, std::process::id());
        assert!(stat(std::process::id()).unwrap().start.unwrap() <= s.start.unwrap(), "started after its parent");
        let env: Vec<String> = environ(pid).unwrap().iter().map(|e| String::from_utf8_lossy(e).into_owned()).collect();
        assert!(env.contains(&"INTERLOCK_PROBE=marker-1".to_string()), "{env:?}");
        assert!(env.contains(&"OTHER=x=y".to_string()), "{env:?}");
        assert!(started_with(pid, "INTERLOCK_PROBE", None));
        assert!(started_with(pid, "INTERLOCK_PROBE", Some("marker-1")));
        assert!(!started_with(pid, "INTERLOCK_PROBE", Some("marker")));
        assert!(!started_with(pid, "INTERLOCK_PROB", None), "a name is matched whole");
        assert_eq!(command_line(pid).unwrap(), ["sleep", "30"]);
        child.kill().unwrap();
        // Killed and not yet collected: a zombie, or already gone.
        let start = std::time::Instant::now();
        while stat(pid).is_some_and(|s| !s.zombie) && start.elapsed().as_secs() < 5 {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(stat(pid).is_none_or(|s| s.zombie));
        child.wait().unwrap();
        assert!(stat(pid).is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_procargs_area_splits_into_arguments_and_environment() {
        // argc, the executable's path and its padding, two arguments, two
        // variables, then the empty string that ends them and what follows.
        let mut area = 2i32.to_ne_bytes().to_vec();
        area.extend_from_slice(b"/bin/sleep\0\0\0\0sleep\x0030\0A=1\0B=\0\0ptr_munge=\0");
        let (args, env) = imp::split(&area).unwrap();
        assert_eq!(args, [b"sleep".to_vec(), b"30".to_vec()]);
        assert_eq!(env, [b"A=1".to_vec(), b"B=".to_vec()]);
    }
}
