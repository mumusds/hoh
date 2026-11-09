#!/usr/bin/env bash
# Check "deployment" (mode product): TEST-005-06 (AC-005-06; docs/adr/ADR-023.md decisions 9 and 11). Builds the
# release from this checkout (the candidate's commit under tools/run_check.py), packs the release bundle with
# bundle.sh into a temporary directory, and runs crates/hoh-app/tests/deployment.rs against it: install.sh into a fresh
# temporary HOME under a private systemd user manager, a detached run and an attach, then --uninstall. Writes nothing in
# the repository. Under tools/run_check.py the per-test results are written to $HOH_EVIDENCE_DIR/test-results.jsonl,
# also when tests fail. Fails if no test passed or TEST-005-06 did not pass.
set -Eeuo pipefail
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."
# shellcheck source=scripts/product/build-commit.sh
source scripts/product/build-commit.sh
if [[ -n "${HOH_CANDIDATE_FILE:-}" ]]; then
  candidate="$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["git_commit"])' "$HOH_CANDIDATE_FILE")"
  if [[ "${HOH_BUILD_COMMIT:-}" != "$candidate" ]]; then
    echo "deployment: this checkout is ${HOH_BUILD_COMMIT:-not a Git commit}, not the candidate's commit $candidate" >&2
    exit 1
  fi
fi
if [[ -z "${HOH_BUILD_COMMIT:-}" ]]; then
  echo "deployment: not a Git checkout: no commit for hoh --version to name" >&2
  exit 1
fi
cargo build --workspace --release --locked
work="$(mktemp -d)"
log="$(mktemp)"
trap 'rm -rf "$work" "$log"' EXIT
bash scripts/product/bundle.sh "$work/hoh-release.tar.gz"
echo "deployment: release bundle $(sha256sum "$work/hoh-release.tar.gz" | cut -c1-16)… of $HOH_BUILD_COMMIT" >&2
rc=0
HOH_RELEASE_BUNDLE="$work/hoh-release.tar.gz" HOH_EXPECTED_COMMIT="$HOH_BUILD_COMMIT" \
  cargo test -p hoh-app --locked --test deployment -- --include-ignored --test-threads=1 2>&1 | tee "$log" || rc=$?
if [[ -n "${HOH_EVIDENCE_DIR:-}" ]]; then
  python3 scripts/product/test_results.py --log "$log" --out "$HOH_EVIDENCE_DIR/test-results.jsonl"
fi
[[ "$rc" -eq 0 ]] || exit "$rc"
passed="$(grep -Eo 'test result: [a-z]+\. [0-9]+ passed' "$log" | awk '{s+=$4} END {print s+0}')"
if [[ "$passed" -eq 0 ]] || ! grep -Eq '^test test_005_06_[a-z0-9_]+ \.\.\. ok$' "$log"; then
  echo "NO_TESTS: TEST-005-06 did not pass in the deployment check; an empty or all-ignored suite is not evidence." >&2
  exit 3
fi
echo "deployment: $passed tests passed." >&2
