//! The room probe (docs/adr/ADR-016.md, the note's rounds 2 and 3; docs/09 fault matrix item 7): after a write to a
//! store file failed, whether the store's directory has room is seen directly, never told by an errno read after
//! SQLite returns (SQLite does more I/O inside the call that failed, a rollback's, so that errno may be a later
//! call's). The WAL self-test and the store's commits judge by it.

use std::io::Write as _;
use std::path::Path;

/// Bytes the probe writes: twice the largest single allocation of the WAL self-test (the -shm's 32 KiB).
pub(super) const BYTES: usize = 64 << 10;

/// What an I/O error of a call that writes or grows a store file did, by its extended result code; `None` for any
/// other code. Besides SQLITE_FULL these are the failures a full file system or an exceeded quota gives, and the probe
/// settles which they are. The -shm's mapping (SQLITE_IOERR_SHMMAP) is not one: it is WAL's (AC-062-06).
pub(super) fn growing_write(extended: i32) -> Option<&'static str> {
    use rusqlite::ffi;
    match extended {
        ffi::SQLITE_IOERR_WRITE => Some("a write"),
        ffi::SQLITE_IOERR_FSYNC => Some("an fsync"),
        ffi::SQLITE_IOERR_TRUNCATE => Some("a truncate"),
        ffi::SQLITE_IOERR_SHMSIZE => Some("the -shm's growth"),
        _ => None,
    }
}

/// Whether the directory of `path` has room now, seen directly: `path`, a new file (made with create_new, so never
/// another's), takes [`BYTES`] of non-repeating bytes (a compressing file system must allocate them too) and an fsync,
/// and is removed. The OS error that refused it, if one did.
pub(super) fn probe(path: &Path) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let mut bytes = vec![0u8; BYTES];
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    for chunk in bytes.chunks_mut(8) {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        chunk.copy_from_slice(&x.to_le_bytes()[..chunk.len()]);
    }
    let written = file.write_all(&bytes).and_then(|()| file.sync_all());
    drop(file);
    let _ = std::fs::remove_file(path);
    written
}

/// Whether `e`, from [`probe`], says there is no room: no space left, or the quota exceeded.
pub(super) fn none(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::StorageFull | std::io::ErrorKind::QuotaExceeded
    )
}
