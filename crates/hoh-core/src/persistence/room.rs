//! The room probe (docs/adr/ADR-016.md, the note's rounds 2 to 4; docs/09 fault matrix item 7): after a write to a
//! store file failed, whether the store's directory has room is seen directly, never told by an errno read after
//! SQLite returns (SQLite does more I/O inside the call that failed, a rollback's, so that errno may be a later
//! call's). The WAL self-test and the store's commits judge by it.

use super::private::{Leftover, Private, names, told};
use std::io::Write as _;
use std::path::Path;

/// Bytes the probe writes: twice the largest single allocation of the WAL self-test (the -shm's 32 KiB).
pub(super) const BYTES: usize = 64 << 10;

/// What an I/O error of a call that writes, grows or creates a store file did, by its extended result code; `None` for
/// any other code. Besides SQLITE_FULL these are the failures a full file system or an exceeded quota gives (an open
/// that creates a file, SQLITE_CANTOPEN, fails so without an inode or a directory block: F7-302), and the probe
/// settles which they are. The -shm's mapping (SQLITE_IOERR_SHMMAP) is not one: it is WAL's (AC-062-06).
pub(super) fn growing_write(extended: i32) -> Option<&'static str> {
    use rusqlite::ffi;
    match extended {
        ffi::SQLITE_IOERR_WRITE => Some("a write"),
        ffi::SQLITE_IOERR_FSYNC => Some("an fsync"),
        ffi::SQLITE_IOERR_TRUNCATE => Some("a truncate"),
        ffi::SQLITE_IOERR_SHMSIZE => Some("the -shm's growth"),
        ffi::SQLITE_CANTOPEN => Some("an open"),
        _ => None,
    }
}

/// What the probe saw: whether its write and fsync went through (the OS error that refused them, if one did), and
/// every path of its private directory that could not be removed (F7-202, F7-302).
#[derive(Debug)]
pub(super) struct Seen {
    pub(super) written: std::io::Result<()>,
    pub(super) left: Vec<Leftover>,
}

impl Seen {
    /// The probe's leftovers, to follow the rest of a message: `; and <path> is left, not removed: <error>…`, or
    /// nothing.
    pub(super) fn left_told(&self) -> String {
        if self.left.is_empty() {
            String::new()
        } else {
            format!("; and {}", told(&self.left))
        }
    }
}

/// Whether the directory of `base` has room now, seen directly: a private directory of this call's
/// (`<base><pid>-<random>`, [`Private`]) is made there, a file in it takes [`BYTES`] of non-repeating bytes (a
/// compressing file system must allocate them too) and an fsync, and the directory is removed with it; what cannot be
/// removed is told.
pub(super) fn probe(base: &Path) -> Seen {
    let private =
        match names(base, 8).and_then(|names| Private::make_first(names).map_err(|(_, e)| e)) {
            Ok(private) => private,
            Err(e) => {
                return Seen {
                    written: Err(e),
                    left: Vec::new(),
                };
            }
        };
    let mut bytes = vec![0u8; BYTES];
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    for chunk in bytes.chunks_mut(8) {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        chunk.copy_from_slice(&x.to_le_bytes()[..chunk.len()]);
    }
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(private.path().join("room"))
        .and_then(|mut file| file.write_all(&bytes).and_then(|()| file.sync_all()));
    Seen {
        written,
        left: private.remove(),
    }
}

/// Whether `e`, from [`probe`], says there is no room: no space left, or the quota exceeded.
pub(super) fn none(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::StorageFull | std::io::ErrorKind::QuotaExceeded
    )
}
