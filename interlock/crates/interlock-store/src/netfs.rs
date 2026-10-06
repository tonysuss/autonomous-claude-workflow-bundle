//! One controller per checkout, on local disk. SQLite's locks, and the
//! controller lock beside the store, do not hold on a network filesystem,
//! where two machines can each believe they hold them. So the store refuses
//! to open in a directory on NFS, SMB/CIFS, another network filesystem the
//! kernel names, or a FUSE filesystem known to reach over the network.
//!
//! The check reads the directory's filesystem with `statfs`, and for FUSE
//! the mount's subtype from `/proc/self/mountinfo`. It runs on Linux only.

use std::path::{Path, PathBuf};

/// Magic numbers `statfs` reports for network filesystems (`statfs(2)`, `linux/magic.h`).
const NETWORK: &[(u32, &str)] = &[
    (0x0000_6969, "NFS"),
    (0x0000_517B, "SMB"),
    (0xFE53_4D42, "SMB2"),
    (0xFF53_4D42, "CIFS"),
    (0x5346_414F, "AFS"),
    (0x6B41_4653, "kAFS"),
    (0x7375_7245, "Coda"),
    (0x0000_564C, "NCP"),
    (0x00C3_6400, "Ceph"),
    (0x0BD0_0BD0, "Lustre"),
    (0x4750_4653, "GPFS"),
];

const FUSE: u32 = 0x6573_5546;

/// FUSE filesystems that reach over the network, by the subtype in their
/// mount's type (`fuse.<subtype>`). Other FUSE mounts, such as a container's
/// fuse-overlayfs, are local.
const NETWORK_FUSE: &[&str] = &[
    "sshfs",
    "rclone",
    "s3fs",
    "gcsfuse",
    "goofys",
    "blobfuse",
    "blobfuse2",
    "mountpoint-s3",
    "juicefs",
    "glusterfs",
    "ceph",
    "ceph-fuse",
    "curlftpfs",
    "smbnetfs",
];

/// The network filesystem a `statfs` magic number, and for FUSE the mount's
/// subtype, name; `None` for a local one.
fn classify(magic: u32, fuse_subtype: Option<&str>) -> Option<String> {
    if let Some((_, name)) = NETWORK.iter().find(|(m, _)| *m == magic) {
        return Some((*name).to_string());
    }
    match fuse_subtype {
        Some(sub) if magic == FUSE && NETWORK_FUSE.contains(&sub) => Some(format!("FUSE {sub}")),
        _ => None,
    }
}

/// Undoes mountinfo's octal escapes (`\040` for a space).
fn unescape(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let digits = &bytes[(i + 1).min(bytes.len())..(i + 4).min(bytes.len())];
        if bytes[i] == b'\\' && digits.len() == 3 && digits.iter().all(|d| (b'0'..=b'7').contains(d)) {
            out.push(digits.iter().fold(0u8, |n, d| n.wrapping_mul(8).wrapping_add(d - b'0')));
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The FUSE subtype of the mount that holds `path`, from the text of
/// `/proc/self/mountinfo`: the deepest mount point above it, and of mounts
/// stacked on one point, the last.
fn fuse_subtype(mountinfo: &str, path: &Path) -> Option<String> {
    let mut best: Option<(usize, &str)> = None;
    for line in mountinfo.lines() {
        // `id parent dev root mount-point options [optional...] - type source super-options`
        let Some((left, right)) = line.split_once(" - ") else { continue };
        let (Some(point), Some(fstype)) = (left.split(' ').nth(4), right.split(' ').next()) else { continue };
        let point = PathBuf::from(unescape(point));
        if path.starts_with(&point) && best.is_none_or(|(depth, _)| point.components().count() >= depth) {
            best = Some((point.components().count(), fstype));
        }
    }
    best.and_then(|(_, fstype)| fstype.strip_prefix("fuse.")).map(str::to_string)
}

/// The network filesystem `dir` is on, if any. A directory that does not
/// exist yet is judged by the nearest one above it that does.
#[cfg(target_os = "linux")]
pub fn network_filesystem(dir: &Path) -> Option<String> {
    let dir = if dir.as_os_str().is_empty() { Path::new(".") } else { dir };
    let existing = dir.ancestors().find(|d| !d.as_os_str().is_empty() && d.exists())?;
    let stat = nix::sys::statfs::statfs(existing).ok()?;
    // The kernel's magic numbers are 32 bits, whatever width libc gives the field.
    let magic = stat.filesystem_type().0 as u32;
    let subtype = if magic == FUSE {
        let real = existing.canonicalize().ok()?;
        let mountinfo = std::fs::read("/proc/self/mountinfo").ok()?;
        fuse_subtype(&String::from_utf8_lossy(&mountinfo), &real)
    } else {
        None
    };
    classify(magic, subtype.as_deref())
}

#[cfg(not(target_os = "linux"))]
pub fn network_filesystem(_dir: &Path) -> Option<String> {
    None
}

/// Set to `1` to open a store on a network filesystem anyway.
pub const ALLOW: &str = "INTERLOCK_ALLOW_NETWORK_FS";

/// Refuses a store directory on a network filesystem, unless the operator allowed it.
pub(crate) fn ensure_local(dir: &Path) -> crate::Result<()> {
    decide(dir, network_filesystem(dir), std::env::var(ALLOW).is_ok_and(|v| v == "1"))
}

fn decide(dir: &Path, kind: Option<String>, allowed: bool) -> crate::Result<()> {
    match kind {
        Some(kind) if !allowed => Err(crate::StoreError::NetworkFilesystem { dir: dir.display().to_string(), kind }),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_filesystems_are_named_by_their_magic_numbers() {
        for (magic, name) in [
            (0x6969, "NFS"),
            (0x517B, "SMB"),
            (0xFE53_4D42, "SMB2"),
            (0xFF53_4D42, "CIFS"),
            (0x5346_414F, "AFS"),
            (0x00C3_6400, "Ceph"),
        ] {
            assert_eq!(classify(magic, None).as_deref(), Some(name), "{magic:#x}");
        }
        // ext4, xfs, btrfs, tmpfs, overlayfs and 9p (a virtual machine's shared folders) are local.
        for magic in [0xEF53, 0x5846_5342, 0x9123_683E, 0x0102_1994, 0x794C_7630, 0x0102_1997] {
            assert_eq!(classify(magic, None), None, "{magic:#x}");
        }
    }

    #[test]
    fn fuse_counts_as_network_only_for_a_known_network_subtype() {
        assert_eq!(classify(FUSE, Some("sshfs")).as_deref(), Some("FUSE sshfs"));
        assert_eq!(classify(FUSE, Some("rclone")).as_deref(), Some("FUSE rclone"));
        assert_eq!(classify(FUSE, Some("fuse-overlayfs")), None);
        assert_eq!(classify(FUSE, None), None, "a FUSE mount with no subtype is not assumed remote");
        assert_eq!(classify(0xEF53, Some("sshfs")), None, "the subtype matters only for FUSE");
    }

    const MOUNTINFO: &str = "\
28 1 254:0 / / rw,relatime - ext4 /dev/vda rw
40 28 0:50 / /home/me/remote rw,nosuid shared:7 - fuse.sshfs me@build:/src rw,user_id=0
41 40 0:51 / /home/me/remote/cache rw - fuse.fuse-overlayfs fuse-overlayfs rw
42 28 0:52 / /mnt/with\\040space rw - fuse.rclone gdrive: rw
43 28 0:53 / /mnt/stacked rw - fuse.sshfs a:/x rw
44 28 0:54 / /mnt/stacked rw - ext4 /dev/vdb rw
";

    #[test]
    fn the_mount_holding_a_path_is_the_deepest_and_last_one() {
        let sub = |p: &str| fuse_subtype(MOUNTINFO, Path::new(p));
        assert_eq!(sub("/home/me/remote/repo/.interlock").as_deref(), Some("sshfs"));
        assert_eq!(sub("/home/me/remote/cache/x").as_deref(), Some("fuse-overlayfs"), "the deeper mount wins");
        assert_eq!(sub("/home/me/remoteness"), None, "mount points match whole components");
        assert_eq!(sub("/mnt/with space/repo").as_deref(), Some("rclone"), "escaped spaces are read");
        assert_eq!(sub("/mnt/stacked/repo"), None, "a later mount on the same point hides the earlier one");
        assert_eq!(sub("/srv"), None);
    }

    #[test]
    fn a_network_store_is_refused_with_the_reason_and_the_way_out() {
        let dir = Path::new("/mnt/nfs/repo/.interlock");
        let err = decide(dir, Some("NFS".into()), false).unwrap_err().to_string();
        for part in
            ["/mnt/nfs/repo/.interlock", "network filesystem (NFS)", "local disk", "INTERLOCK_ALLOW_NETWORK_FS=1"]
        {
            assert!(err.contains(part), "{part} in {err}");
        }
        assert!(decide(dir, Some("NFS".into()), true).is_ok(), "the operator may allow it");
        assert!(decide(dir, None, false).is_ok());
    }

    #[test]
    fn a_local_directory_is_not_a_network_filesystem() {
        let dir = std::env::temp_dir();
        assert_eq!(network_filesystem(&dir), None);
        assert_eq!(network_filesystem(&dir.join("not/made/yet")), None, "judged by the directory above");
        assert_eq!(network_filesystem(Path::new("")), None);
    }
}
