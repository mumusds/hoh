//! M2T09R-02, -150 (docs/adr/ADR-026.md decision 6): keeping a large raw log again, or repairing its damaged object,
//! checks the object by hashing it in pieces: the process's peak memory does not grow with the log's size. A test
//! binary of its own, so its peak (VmHWM) is this test's.

type R = Result<(), Box<dyn std::error::Error>>;

/// The process's peak resident memory, in KiB.
fn peak_kib() -> Result<u64, Box<dyn std::error::Error>> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    let line = status
        .lines()
        .find(|l| l.starts_with("VmHWM:"))
        .ok_or("no VmHWM")?;
    Ok(line.split_whitespace().nth(1).ok_or("no value")?.parse()?)
}

#[test]
fn keeping_a_large_log_again_or_repairing_it_does_not_read_it_whole() -> R {
    use std::io::Write as _;
    const MIB: usize = 1 << 20;
    let dir = tempfile::tempdir()?;
    let file = dir.path().join("stdout.jsonl");
    {
        let mut out = std::fs::File::create(&file)?;
        let chunk = vec![b'l'; MIB];
        for _ in 0..64 {
            out.write_all(&chunk)?;
        }
    }
    let (sha, size) = hoh_core::objects::put_file(dir.path(), &file)?;
    assert_eq!(size, 64 * MIB as u64);
    let before = peak_kib()?;
    // Kept again: the object there is checked.
    hoh_core::objects::put_file(dir.path(), &file)?;
    // Damaged, then kept again: repaired and checked.
    std::fs::write(
        dir.path().join(hoh_core::objects::relative_path(&sha)),
        b"damaged",
    )?;
    hoh_core::objects::put_file(dir.path(), &file)?;
    let after = peak_kib()?;
    assert!(
        after - before < 16 * 1024,
        "the peak grew by {} KiB keeping a 64 MiB log again",
        after - before
    );
    assert_eq!(
        hoh_core::objects::hash_file(&dir.path().join(hoh_core::objects::relative_path(&sha)))?.0,
        sha
    );
    Ok(())
}
