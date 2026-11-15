//! The compat suite's definition (docs/adr/ADR-030.md decision 10, "套件集摘要"): the files whose tree digest is
//! [`super::DEFINITION_SHA256`], beside the embedded fixtures ([`super::fixtures::FILES`]), each embedded here by a
//! literal path and named by its path in the workspace. They are every other file under this crate's src/ (the request
//! builders, the suite, the probe's validator, the adapters' replays, this file), the crate's Cargo.toml, the frame
//! registries the replays judge by (src/known.rs) and `definition/cargo-lock.sha256`, the sha256 of the workspace's
//! Cargo.lock, through which the dependencies count in. tests/compat_definition.rs fails on a file under src/ without
//! an entry here (or, for a fixture, in fixtures.rs), on an entry that differs from its file and on a lock digest that
//! is not the current lock's; refresh the digest, from the workspace root, with
//! `sha256sum Cargo.lock | cut -d' ' -f1 > crates/hoh-agents/src/compat/definition/cargo-lock.sha256`.

/// The definition files but the embedded fixtures, by their path in the workspace.
pub(crate) const FILES: &[(&str, &[u8])] = &[
    (
        "crates/hoh-agents/Cargo.toml",
        include_bytes!("../../Cargo.toml") as &[u8],
    ),
    (
        "crates/hoh-agents/src/adapter.rs",
        include_bytes!("../adapter.rs") as &[u8],
    ),
    (
        "crates/hoh-agents/src/adapter/tests.rs",
        include_bytes!("../adapter/tests.rs") as &[u8],
    ),
    (
        "crates/hoh-agents/src/capabilities.rs",
        include_bytes!("../capabilities.rs") as &[u8],
    ),
    (
        "crates/hoh-agents/src/claude.rs",
        include_bytes!("../claude.rs") as &[u8],
    ),
    (
        "crates/hoh-agents/src/claude/tests.rs",
        include_bytes!("../claude/tests.rs") as &[u8],
    ),
    (
        "crates/hoh-agents/src/clock.rs",
        include_bytes!("../clock.rs") as &[u8],
    ),
    (
        "crates/hoh-agents/src/codex.rs",
        include_bytes!("../codex.rs") as &[u8],
    ),
    (
        "crates/hoh-agents/src/codex/app_server.rs",
        include_bytes!("../codex/app_server.rs") as &[u8],
    ),
    (
        "crates/hoh-agents/src/codex/app_server/requests.rs",
        include_bytes!("../codex/app_server/requests.rs") as &[u8],
    ),
    (
        "crates/hoh-agents/src/codex/exec.rs",
        include_bytes!("../codex/exec.rs") as &[u8],
    ),
    (
        "crates/hoh-agents/src/codex/tests.rs",
        include_bytes!("../codex/tests.rs") as &[u8],
    ),
    (
        "crates/hoh-agents/src/compat.rs",
        include_bytes!("../compat.rs") as &[u8],
    ),
    (
        "crates/hoh-agents/src/compat/definition.rs",
        include_bytes!("definition.rs") as &[u8],
    ),
    (
        "crates/hoh-agents/src/compat/definition/cargo-lock.sha256",
        include_bytes!("definition/cargo-lock.sha256") as &[u8],
    ),
    (
        "crates/hoh-agents/src/compat/fixtures.rs",
        include_bytes!("fixtures.rs") as &[u8],
    ),
    (
        "crates/hoh-agents/src/compat/suite.rs",
        include_bytes!("suite.rs") as &[u8],
    ),
    (
        "crates/hoh-agents/src/compat/suite/tests.rs",
        include_bytes!("suite/tests.rs") as &[u8],
    ),
    (
        "crates/hoh-agents/src/compat/tests.rs",
        include_bytes!("tests.rs") as &[u8],
    ),
    (
        "crates/hoh-agents/src/frame.rs",
        include_bytes!("../frame.rs") as &[u8],
    ),
    (
        "crates/hoh-agents/src/identity.rs",
        include_bytes!("../identity.rs") as &[u8],
    ),
    (
        "crates/hoh-agents/src/known.rs",
        include_bytes!("../known.rs") as &[u8],
    ),
    (
        "crates/hoh-agents/src/launch.rs",
        include_bytes!("../launch.rs") as &[u8],
    ),
    (
        "crates/hoh-agents/src/lib.rs",
        include_bytes!("../lib.rs") as &[u8],
    ),
    (
        "crates/hoh-agents/src/login.rs",
        include_bytes!("../login.rs") as &[u8],
    ),
    (
        "crates/hoh-agents/src/pi.rs",
        include_bytes!("../pi.rs") as &[u8],
    ),
    (
        "crates/hoh-agents/src/pi/tests.rs",
        include_bytes!("../pi/tests.rs") as &[u8],
    ),
    (
        "crates/hoh-agents/src/probe.rs",
        include_bytes!("../probe.rs") as &[u8],
    ),
    (
        "crates/hoh-agents/src/probe/capability_tests.rs",
        include_bytes!("../probe/capability_tests.rs") as &[u8],
    ),
    (
        "crates/hoh-agents/src/probe/compat_tests.rs",
        include_bytes!("../probe/compat_tests.rs") as &[u8],
    ),
    (
        "crates/hoh-agents/src/probe/conform.rs",
        include_bytes!("../probe/conform.rs") as &[u8],
    ),
    (
        "crates/hoh-agents/src/process.rs",
        include_bytes!("../process.rs") as &[u8],
    ),
    (
        "tests/fixtures/protocol/claude/2.1.318/known-frames.json",
        include_bytes!("../../../../tests/fixtures/protocol/claude/2.1.318/known-frames.json")
            as &[u8],
    ),
    (
        "tests/fixtures/protocol/codex/0.166.0/known-frames.json",
        include_bytes!("../../../../tests/fixtures/protocol/codex/0.166.0/known-frames.json")
            as &[u8],
    ),
    (
        "tests/fixtures/protocol/pi/1.5.0/known-frames.json",
        include_bytes!("../../../../tests/fixtures/protocol/pi/1.5.0/known-frames.json") as &[u8],
    ),
];
