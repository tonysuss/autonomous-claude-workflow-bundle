//! One controller per checkout, on local disk. SQLite's locks, and the
//! controller lock beside the store, do not hold on a network filesystem,
//! where two machines can each believe they hold them. So the store refuses
//! to open in a directory on NFS, SMB/CIFS, another network or cluster
//! filesystem the kernel names, or a FUSE filesystem known to reach over the
//! network, or layered on a directory that is on one.
//!
//! On Linux the check reads the directory's filesystem with `statfs`, and for
//! FUSE the mount's type and source from `/proc/self/mountinfo`. On macOS it
//! reads the type name `statfs` reports. Elsewhere it is not made.

use std::path::Path;
#[cfg(any(target_os = "linux", test))]
use std::path::PathBuf;

/// Magic numbers `statfs` reports for network and cluster filesystems
/// (`statfs(2)`, `linux/magic.h`, and each filesystem's own sources).
#[cfg(any(target_os = "linux", test))]
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
    (0x1983_0326, "BeeGFS"),
    (0x2003_0528, "OrangeFS"),
    (0x7461_636F, "OCFS2"),
    (0x0116_1970, "GFS2"),
];

#[cfg(any(target_os = "linux", test))]
const FUSE: u32 = 0x6573_5546;

/// FUSE filesystems that reach over the network, by the subtype in their
/// mount's type (`fuse.<subtype>`). Other FUSE mounts, such as a container's
/// fuse-overlayfs, are local unless they are layered on a network directory.
#[cfg(any(target_os = "linux", test))]
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
    "gvfsd-fuse",
    "mfs",
    "lizardfs",
    "s3ql",
    "davfs",
    "keybase",
    "kbfsfuse",
    "fuse-nfs",
];

/// How many FUSE layers are followed down to the directory they sit on.
#[cfg(any(target_os = "linux", test))]
const LAYERS: usize = 4;

/// The network filesystem a `statfs` magic number names, if any.
#[cfg(any(target_os = "linux", test))]
fn network_magic(magic: u32) -> Option<String> {
    NETWORK.iter().find(|(m, _)| *m == magic).map(|(_, name)| (*name).to_string())
}

/// Undoes mountinfo's octal escapes (`\040` for a space).
#[cfg(any(target_os = "linux", test))]
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

/// A mount from `/proc/self/mountinfo`.
#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct Mount {
    point: PathBuf,
    fstype: String,
    source: String,
}

/// The mount that holds `path`, from the text of `/proc/self/mountinfo`: the
/// deepest mount point above it, and of mounts stacked on one point, the last.
#[cfg(any(target_os = "linux", test))]
fn mount_of(mountinfo: &str, path: &Path) -> Option<Mount> {
    let mut best: Option<Mount> = None;
    for line in mountinfo.lines() {
        // `id parent dev root mount-point options [optional...] - type source super-options`
        let Some((left, right)) = line.split_once(" - ") else { continue };
        let mut right = right.split(' ');
        let (Some(point), Some(fstype), Some(source)) = (left.split(' ').nth(4), right.next(), right.next()) else {
            continue;
        };
        let point = PathBuf::from(unescape(point));
        let depth = point.components().count();
        if path.starts_with(&point) && best.as_ref().is_none_or(|b| depth >= b.point.components().count()) {
            best = Some(Mount { point, fstype: fstype.to_string(), source: unescape(source) });
        }
    }
    best
}

/// The network filesystem `path` is on, if any, given how to read a path's
/// `statfs` magic number and the mount table. A FUSE filesystem layered on a
/// directory (gocryptfs, bindfs and others that name it as their source) is
/// judged by that directory too.
#[cfg(any(target_os = "linux", test))]
fn classify_path(
    path: &Path,
    magic_of: &dyn Fn(&Path) -> Option<u32>,
    mountinfo: &str,
    layers: usize,
) -> Option<String> {
    let magic = magic_of(path)?;
    if let Some(kind) = network_magic(magic) {
        return Some(kind);
    }
    if magic != FUSE {
        return None;
    }
    let mount = mount_of(mountinfo, path)?;
    let subtype = mount.fstype.strip_prefix("fuse.").unwrap_or(&mount.fstype);
    if NETWORK_FUSE.contains(&subtype) {
        return Some(format!("FUSE {subtype}"));
    }
    let lower = Path::new(&mount.source);
    if layers == 0 || !lower.is_absolute() || lower.starts_with(&mount.point) {
        return None;
    }
    classify_path(lower, magic_of, mountinfo, layers - 1).map(|kind| format!("FUSE {subtype} over {kind}"))
}

/// The network filesystem `dir` is on, if any. A directory that does not
/// exist yet is judged by the nearest one above it that does.
#[cfg(target_os = "linux")]
pub fn network_filesystem(dir: &Path) -> Option<String> {
    let existing = nearest_existing(dir)?.canonicalize().ok()?;
    let mountinfo = std::fs::read("/proc/self/mountinfo").map(|m| String::from_utf8_lossy(&m).into_owned());
    // The kernel's magic numbers are 32 bits, whatever width libc gives the field.
    let magic_of = |p: &Path| nix::sys::statfs::statfs(p).ok().map(|s| s.filesystem_type().0 as u32);
    classify_path(&existing, &magic_of, mountinfo.as_deref().unwrap_or_default(), LAYERS)
}

/// The type names macOS's `statfs` reports for network filesystems. macFUSE
/// mounts report `macfuse` or `osxfuse` whatever they serve, so they are not
/// recognised.
#[cfg(target_os = "macos")]
const NETWORK_NAMES: &[&str] = &["nfs", "smbfs", "afpfs", "webdav", "ftp", "cifs"];

#[cfg(target_os = "macos")]
pub fn network_filesystem(dir: &Path) -> Option<String> {
    let stat = nix::sys::statfs::statfs(nearest_existing(dir)?).ok()?;
    let name = stat.filesystem_type_name();
    NETWORK_NAMES.contains(&name).then(|| name.to_string())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn network_filesystem(_dir: &Path) -> Option<String> {
    None
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn nearest_existing(dir: &Path) -> Option<&Path> {
    let dir = if dir.as_os_str().is_empty() { Path::new(".") } else { dir };
    dir.ancestors().find(|d| !d.as_os_str().is_empty() && d.exists())
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

    const MOUNTINFO: &str = "\
28 1 254:0 / / rw,relatime - ext4 /dev/vda rw
40 28 0:50 / /home/me/remote rw,nosuid shared:7 - fuse.sshfs me@build:/src rw,user_id=0
41 40 0:51 / /home/me/remote/cache rw - fuse.fuse-overlayfs fuse-overlayfs rw
42 28 0:52 / /mnt/with\\040space rw - fuse.rclone gdrive: rw
43 28 0:53 / /mnt/stacked rw - fuse.sshfs a:/x rw
44 28 0:54 / /mnt/stacked rw - ext4 /dev/vdb rw
45 28 0:55 / /home/me/plain rw - fuse.gocryptfs /home/me/remote/cipher rw
46 28 0:56 / /home/me/private rw - fuse.gocryptfs /home/me/cipher rw
47 28 0:57 / /home/me/loop rw - fuse.bindfs /home/me/loop/inner rw
48 28 0:58 / /mnt/nfs rw - nfs4 server:/export rw
49 28 0:59 / /home/me/mirror rw - fuse.bindfs /mnt/nfs/data rw
";

    /// `statfs` as it would answer for the mounts above.
    fn magic_of(path: &Path) -> Option<u32> {
        let mount = mount_of(MOUNTINFO, path)?;
        Some(match mount.fstype.as_str() {
            "ext4" => 0xEF53,
            "nfs4" => 0x6969,
            _ => FUSE,
        })
    }

    fn classify(path: &str) -> Option<String> {
        classify_path(Path::new(path), &magic_of, MOUNTINFO, LAYERS)
    }

    #[test]
    fn network_filesystems_are_named_by_their_magic_numbers() {
        for (magic, name) in [
            (0x6969, "NFS"),
            (0x517B, "SMB"),
            (0xFE53_4D42, "SMB2"),
            (0xFF53_4D42, "CIFS"),
            (0x5346_414F, "AFS"),
            (0x00C3_6400, "Ceph"),
            (0x1983_0326, "BeeGFS"),
            (0x2003_0528, "OrangeFS"),
            (0x7461_636F, "OCFS2"),
            (0x0116_1970, "GFS2"),
        ] {
            assert_eq!(network_magic(magic).as_deref(), Some(name), "{magic:#x}");
        }
        // ext4, xfs, btrfs, tmpfs, overlayfs and 9p (a virtual machine's shared folders) are local.
        for magic in [0xEF53, 0x5846_5342, 0x9123_683E, 0x0102_1994, 0x794C_7630, 0x0102_1997, FUSE] {
            assert_eq!(network_magic(magic), None, "{magic:#x}");
        }
    }

    #[test]
    fn fuse_counts_as_network_for_a_known_network_type() {
        for sub in ["sshfs", "rclone", "gvfsd-fuse", "mfs", "lizardfs", "s3ql", "davfs", "keybase", "fuse-nfs"] {
            let info = format!("50 28 0:60 / /mnt/x rw - fuse.{sub} remote rw\n");
            let found = classify_path(Path::new("/mnt/x/repo"), &|_| Some(FUSE), &info, LAYERS);
            assert_eq!(found, Some(format!("FUSE {sub}")), "{sub}");
        }
        let local = "50 28 0:60 / /mnt/x rw - fuse.fuse-overlayfs fuse-overlayfs rw\n";
        assert_eq!(classify_path(Path::new("/mnt/x"), &|_| Some(FUSE), local, LAYERS), None);
        let bare = "50 28 0:60 / /mnt/x rw - fuse remote rw\n";
        assert_eq!(
            classify_path(Path::new("/mnt/x"), &|_| Some(FUSE), bare, LAYERS),
            None,
            "no type, not assumed remote"
        );
    }

    #[test]
    fn the_mount_holding_a_path_is_the_deepest_and_last_one() {
        let fstype = |p: &str| mount_of(MOUNTINFO, Path::new(p)).map(|m| m.fstype);
        assert_eq!(fstype("/home/me/remote/repo/.interlock").as_deref(), Some("fuse.sshfs"));
        assert_eq!(fstype("/home/me/remote/cache/x").as_deref(), Some("fuse.fuse-overlayfs"), "the deeper mount");
        assert_eq!(fstype("/home/me/remoteness").as_deref(), Some("ext4"), "mount points match whole components");
        assert_eq!(fstype("/mnt/with space/repo").as_deref(), Some("fuse.rclone"), "escaped spaces are read");
        assert_eq!(fstype("/mnt/stacked/repo").as_deref(), Some("ext4"), "the later of two mounts on one point");
        assert_eq!(classify("/home/me/remote/repo").as_deref(), Some("FUSE sshfs"));
        assert_eq!(classify("/home/me/remote/cache/repo"), None, "a local FUSE mount inside a remote one");
        assert_eq!(classify("/mnt/nfs/repo").as_deref(), Some("NFS"));
        assert_eq!(classify("/srv"), None);
    }

    #[test]
    fn a_fuse_layer_over_a_network_directory_is_network_too() {
        assert_eq!(classify("/home/me/plain/repo").as_deref(), Some("FUSE gocryptfs over FUSE sshfs"));
        assert_eq!(classify("/home/me/mirror/repo").as_deref(), Some("FUSE bindfs over NFS"));
        assert_eq!(classify("/home/me/private/repo"), None, "a layer over a local directory is local");
        assert_eq!(classify("/home/me/loop/repo"), None, "a layer over itself ends");
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
