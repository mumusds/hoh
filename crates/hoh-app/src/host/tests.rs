//! The project lock (`DirLock`).

use super::DirLock;

/// A lock released while a copy of its descriptor lives on (a child another thread forked and has not yet exec'd
/// holds one) is free: the lock belongs to the open file description, so closing one descriptor alone would leave it
/// held until the copy is closed (the doctor tests' "a host holds" flake, M2-T15 S6b).
#[test]
fn a_released_project_lock_is_free_while_a_copy_of_its_descriptor_lives() {
    let dir = tempfile::tempdir().expect("temp dir");
    let held = DirLock::try_take(dir.path())
        .expect("open")
        .expect("the lock is free");
    let copy = held.0.try_clone().expect("a copy of the descriptor");
    assert!(
        DirLock::try_take(dir.path()).expect("open").is_none(),
        "held while the lock lives"
    );
    drop(held);
    assert!(
        DirLock::try_take(dir.path()).expect("open").is_some(),
        "free once released, the copy still open"
    );
    drop(copy);
}
