#!/usr/bin/env bash
# Check "static": formatting and lints with warnings denied.
set -Eeuo pipefail
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
