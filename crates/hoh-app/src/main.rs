//! `hoh` binary. Placeholder until the CLI lands in M1-T06: it reports NOT_IMPLEMENTED
//! on stderr and exits 3 so no script can mistake it for a working product.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

fn main() {
    // Internal mode: the second process of the WAL self-test (REQ-062); not a user command.
    if std::env::args_os().nth(1).as_deref()
        == Some(hoh_core::persistence::selftest::PROBE_ARG.as_ref())
    {
        std::process::exit(hoh_core::persistence::selftest::probe_main());
    }
    eprintln!(
        "hoh {} (core {}, agents {}): NOT_IMPLEMENTED - the product CLI arrives in M1-T06; see docs/08-development-plan.md",
        env!("CARGO_PKG_VERSION"),
        hoh_core::VERSION,
        hoh_agents::VERSION
    );
    std::process::exit(3);
}
