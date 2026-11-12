use super::*;
use std::os::unix::fs::MetadataExt as _;

#[test]
fn the_description_is_the_same_bytes_for_the_same_environment_and_names_it()
-> Result<(), Box<dyn std::error::Error>> {
    // Decision 5: sorted keys, compact, a final newline, nothing that changes between two hosts' starts.
    let one = describe("git version 2.53.0");
    assert_eq!(one, describe("git version 2.53.0"));
    assert_ne!(one, describe("git version 2.54.0"));
    assert!(one.ends_with(b"\n") && !one[..one.len() - 1].contains(&b'\n'));
    let value: serde_json::Value = serde_json::from_slice(&one)?;
    assert_eq!(value["schema_version"], "1.0");
    assert_eq!(value["tools"]["git"], "git version 2.53.0");
    assert_eq!(value["os"]["sysname"], std::env::consts::OS);
    assert_eq!(value["os"]["machine"], std::env::consts::ARCH);
    assert_eq!(value["hoh"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        String::from_utf8(one.clone())?,
        format!("{}\n", serde_json::to_string(&value)?)
    );
    Ok(())
}

#[test]
fn ensuring_it_again_leaves_the_file_alone_and_a_different_one_is_replaced()
-> Result<(), Box<dyn std::error::Error>> {
    // Decision 5: a host restart does not rewrite it (same inode, same mtime); content that differs is replaced whole.
    let dir = tempfile::tempdir()?;
    let path = ensure(dir.path())?;
    assert_eq!(path, dir.path().join(PATH));
    assert!(dir.path().join(".hoh/.gitignore").is_file());
    let first = std::fs::metadata(&path)?;
    let bytes = std::fs::read(&path)?;
    std::thread::sleep(std::time::Duration::from_millis(20));
    ensure(dir.path())?;
    let again = std::fs::metadata(&path)?;
    assert_eq!(
        (again.ino(), again.mtime_nsec()),
        (first.ino(), first.mtime_nsec())
    );
    assert_eq!(std::fs::read(&path)?, bytes);
    std::fs::write(&path, b"{\"edited\": true}\n")?;
    ensure(dir.path())?;
    assert_eq!(std::fs::read(&path)?, bytes);
    Ok(())
}
