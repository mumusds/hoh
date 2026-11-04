//! Where the database may live (REQ-062): never on a network or shared file system, whose locking SQLite's WAL mode
//! cannot rely on. The file system is read from `/proc/self/mountinfo` (or a given text, for tests and diagnosis):
//! the mount whose mount point is the longest path prefix of the location decides, a later mount over the same point
//! winning. File systems not on the list (virtiofs included) are accepted here and must pass the WAL self-test.

use std::path::{Path, PathBuf};

/// Network and shared file systems: a database there is refused with `DB_ON_NETWORK_FS`.
pub const NETWORK_FS: &[&str] = &[
    "nfs",
    "nfs4",
    "cifs",
    "smb3",
    "smbfs",
    "fuse.sshfs",
    "sshfs",
    "9p",
    "afs",
    "ceph",
    "fuse.ceph",
    "glusterfs",
    "fuse.glusterfs",
    "lustre",
    "gpfs",
    "ocfs2",
    "gfs2",
    "davfs",
    "fuse.davfs2",
];

/// One mount from mountinfo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    pub mount_point: PathBuf,
    pub fstype: String,
}

/// The mounts listed in mountinfo text (`man 5 proc`): field 5 is the mount point (octal escapes such as `\040` for a
/// space), and the file system type follows the ` - ` separator. Malformed lines are skipped.
pub fn parse_mountinfo(text: &str) -> Vec<Mount> {
    text.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(' ').collect();
            let mount_point = fields.get(4)?;
            let separator = fields.iter().skip(6).position(|f| *f == "-")? + 6;
            let fstype = fields.get(separator + 1)?;
            Some(Mount {
                mount_point: PathBuf::from(unescape(mount_point)),
                fstype: (*fstype).to_owned(),
            })
        })
        .collect()
}

fn unescape(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let octal = bytes
            .get(i + 1..i + 4)
            .filter(|d| bytes[i] == b'\\' && d.iter().all(|b| (b'0'..=b'7').contains(b)));
        match octal {
            Some(d) => {
                let value = d.iter().fold(0u32, |v, b| v * 8 + u32::from(b - b'0'));
                out.push(u8::try_from(value).unwrap_or(b'?'));
                i += 4;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The file system type of an absolute `path` among `mounts`: the longest mount point that is a component-wise
/// prefix of the path; on equal length the later mount (it is on top).
pub fn fs_type<'a>(path: &Path, mounts: &'a [Mount]) -> Option<&'a str> {
    let mut best: Option<(&Mount, usize)> = None;
    for mount in mounts {
        if path.starts_with(&mount.mount_point) {
            let depth = mount.mount_point.components().count();
            if best.is_none_or(|(_, d)| depth >= d) {
                best = Some((mount, depth));
            }
        }
    }
    best.map(|(m, _)| m.fstype.as_str())
}

/// The deepest existing directory at or above `path`, canonicalised (a database directory that does not exist yet
/// lies on the same file system as it).
pub fn existing_ancestor(path: &Path) -> std::io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut candidate = absolute.as_path();
    loop {
        match candidate.canonicalize() {
            Ok(found) => return Ok(found),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                candidate = candidate.parent().ok_or(e)?;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Whether `path` (absolute, canonical) is on a network or shared file system according to `mountinfo`: `Err` with
/// the file system type if it is, otherwise the type found (if any).
pub fn check(path: &Path, mountinfo: &str) -> Result<Option<String>, String> {
    let mounts = parse_mountinfo(mountinfo);
    let fstype = fs_type(path, &mounts).map(str::to_owned);
    match fstype {
        Some(t) if NETWORK_FS.contains(&t.as_str()) => Err(t),
        other => Ok(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOUNTINFO: &str = "\
22 1 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw
30 22 0:26 / /p1 rw,relatime shared:2 - nfs4 server:/export rw,vers=4.2
31 22 0:27 / /p2 rw,relatime shared:3 - cifs //server/share rw
32 22 0:28 / /p3 rw,nosuid,nodev,relatime shared:4 - fuse.sshfs user@host:/ rw
33 22 8:2 / /p4 rw,relatime shared:5 - ext4 /dev/sdb1 rw
34 22 0:29 / /p5 rw,nosuid,nodev shared:6 - tmpfs tmpfs rw
35 22 0:30 / /with\\040space rw,relatime - 9p share rw
36 30 8:3 / /p1/local rw,relatime master:7 - ext4 /dev/sdc1 rw
37 22 0:31 / /p5 rw,relatime shared:8 - nfs server:/over rw
38 22 0:32 / /v rw,relatime - virtiofs tag rw
";

    #[test]
    fn network_file_systems_are_refused_and_local_ones_pass() {
        // TEST-062-04 needs the host; the location check itself is covered here.
        let at = |p: &str| check(Path::new(p), MOUNTINFO);
        assert_eq!(at("/p1/project/.hoh"), Err("nfs4".to_owned()));
        assert_eq!(at("/p2/x"), Err("cifs".to_owned()));
        assert_eq!(at("/p3"), Err("fuse.sshfs".to_owned()));
        assert_eq!(at("/p4/project/.hoh"), Ok(Some("ext4".to_owned())));
        // A local mount below a network one decides for paths under it.
        assert_eq!(at("/p1/local/project"), Ok(Some("ext4".to_owned())));
        // Escaped mount points match their real path.
        assert_eq!(at("/with space/project"), Err("9p".to_owned()));
        // A later mount over the same point is the one in effect.
        assert_eq!(at("/p5/project"), Err("nfs".to_owned()));
        // Component-wise prefixes only: /p4x is not under /p4.
        assert_eq!(at("/p4x/project"), Ok(Some("ext4".to_owned())));
        // virtiofs is not refused here; the WAL self-test decides.
        assert_eq!(at("/v/project"), Ok(Some("virtiofs".to_owned())));
    }

    #[test]
    fn malformed_mountinfo_lines_are_skipped() {
        let text =
            "garbage\n1 2 3:4 / /x rw no-separator ext4\n40 1 0:1 / /ok rw - ext4 /dev/x rw\n";
        assert_eq!(
            parse_mountinfo(text),
            vec![Mount {
                mount_point: PathBuf::from("/ok"),
                fstype: "ext4".to_owned()
            }]
        );
    }

    #[test]
    fn missing_directories_resolve_to_their_existing_ancestor() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let deep = dir.path().join("a/b/.hoh");
        assert_eq!(existing_ancestor(&deep)?, dir.path().canonicalize()?);
        Ok(())
    }
}
