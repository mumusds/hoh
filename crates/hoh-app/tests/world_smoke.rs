//! The M2-T13 test world on its own (docs/adr/ADR-031.md decision 14, slice S5): the fixtures the eleven TESTs of
//! M2-T13 share, checked before any TEST uses them. These are tests of the fixtures, not of an acceptance criterion:
//! the vt100 screen snapshot and its erase counter-example.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod pty;

use pty::R;

#[test]
fn the_screen_snapshot_shows_only_what_erases_and_overwrites_left() -> R {
    // Decision 14: the snapshot executes cursor moves, erases and overwrites, so text that is no longer on the screen
    // never satisfies an assertion. The helper passes its own counter-examples ...
    pty::erase_check(&pty::screen)?;
    // ... which a helper that only strips control sequences and joins the text (pty::squash) fails.
    let squash_like = |bytes: &[u8], _: u16, _: u16| -> R<String> { Ok(pty::squash(bytes)) };
    assert!(pty::erase_check(&squash_like).is_err());
    // A screen as a TUI draws it: a frame, then a redraw in place.
    let drawn = b"\x1b[?1049h\x1b[2J\x1b[1;1Hhoh  demo  PAUSED\x1b[2;1Hrun 0123456789\x1b[1;11HRUNNING\x1b[K";
    assert_eq!(
        pty::snapshot(drawn, 5, 40)?,
        "hoh  demo RUNNING\nrun 0123456789"
    );
    Ok(())
}
