//! `hoh` binary. Placeholder until the CLI lands in M1-T06: it reports NOT_IMPLEMENTED
//! on stderr and exits 3 so no script can mistake it for a working product.

fn main() {
    eprintln!(
        "hoh {} (core {}, agents {}): NOT_IMPLEMENTED - the product CLI arrives in M1-T06; see docs/08-development-plan.md",
        env!("CARGO_PKG_VERSION"),
        hoh_core::VERSION,
        hoh_agents::VERSION
    );
    std::process::exit(3);
}
