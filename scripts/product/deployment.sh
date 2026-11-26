#!/usr/bin/env bash
# Check "deployment" (mode product): TEST-005-06 (AC-005-06; docs/adr/ADR-023.md decisions 9 and 11) and TEST-047-03
# (AC-047-03; docs/adr/ADR-034.md decision 12 as amended). Builds the release from this checkout (the candidate's commit
# under tools/run_check.py), packs the release bundle (v1) with bundle.sh into a temporary directory, then builds a
# second release (v2) from a temporary copy of the checkout's tracked files whose developer builtin contract and
# workspace version differ, in the same Cargo target directory (only the workspace's own crates are compiled again),
# and packs it too. Runs crates/hoh-app/tests/deployment.rs against them: install.sh into a fresh temporary HOME under a
# private systemd user manager, a detached run and an attach, then --uninstall (TEST-005-06); v1 installed, a run
# started on it, and v2 installed over it while the run runs (TEST-047-03). Writes nothing in the repository; the v2
# build leaves its executable at the target directory's release/hoh until the next release build links v1's again.
# Under tools/run_check.py the per-test results are written to $HOH_EVIDENCE_DIR/test-results.jsonl, also when tests
# fail. Fails if no test passed, or TEST-005-06 or the upgrade's TEST-047-03 did not pass.
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
executable="$(bash scripts/product/release-exe.sh)"
work="$(mktemp -d)"
log="$(mktemp)"
trap 'rm -rf "$work" "$log"' EXIT
bash scripts/product/bundle.sh "$work/hoh-release.tar.gz" "$executable"
echo "deployment: release bundle $(sha256sum "$work/hoh-release.tar.gz" | cut -c1-16)… of $HOH_BUILD_COMMIT" >&2
# v2 (TEST-047-03): the checkout's tracked files as they are, with one more line in the developer's builtin contract and
# the workspace's patch version one higher (in Cargo.lock too), so that its crates are new artifacts beside v1's and
# never v1's rebuilt in place. Built with v1's target directory: the dependencies are not compiled again. Its commit
# is v1's marked dirty: a tree that differs from the commit (build-commit.sh, ADR-023 decision 8).
target="$(cargo metadata --locked --no-deps --format-version 1 | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')"
v2="$work/v2"
mkdir "$v2"
git ls-files -z | tar --null -T - -cf - | tar -xf - -C "$v2"
python3 - "$v2" <<'PY'
import pathlib, re, sys

root = pathlib.Path(sys.argv[1])
contract = root / "resources/prompts/developer.md"
contract.write_text(contract.read_text(encoding="utf-8") + "\n<!-- v2: 部署检查升级夹具（TEST-047-03）的内置契约变更 -->\n", encoding="utf-8")
manifest = root / "Cargo.toml"
text = manifest.read_text(encoding="utf-8")
match = re.search(r'(?m)^\[workspace\.package\]\nversion = "(\d+)\.(\d+)\.(\d+)"$', text)
if not match:
    sys.exit("deployment: no [workspace.package] version in Cargo.toml")
old = ".".join(match.groups())
new = f"{match[1]}.{match[2]}.{int(match[3]) + 1}"
manifest.write_text(text[: match.start()] + f'[workspace.package]\nversion = "{new}"' + text[match.end() :], encoding="utf-8")
lock = root / "Cargo.lock"
text, count = re.subn(r'(?m)^(name = "hoh-(?:core|agents|client|app)"\nversion = )"' + re.escape(old) + '"$', rf'\g<1>"{new}"', lock.read_text(encoding="utf-8"))
if count != 4:
    sys.exit(f"deployment: {count} workspace packages of version {old} in Cargo.lock, not 4")
lock.write_text(text, encoding="utf-8")
PY
executable2="$(cd "$v2" && CARGO_TARGET_DIR="$target" HOH_BUILD_COMMIT="${HOH_BUILD_COMMIT%-dirty}-dirty" bash scripts/product/release-exe.sh)"
(cd "$v2" && HOH_BUILD_COMMIT="${HOH_BUILD_COMMIT%-dirty}-dirty" bash scripts/product/bundle.sh "$work/hoh-release-v2.tar.gz" "$executable2")
echo "deployment: release bundle v2 $(sha256sum "$work/hoh-release-v2.tar.gz" | cut -c1-16)… ($("$executable2" --version | head -n 1))" >&2
rc=0
# The parsed log is cargo's stdout only: its stderr (build output, and diagnostics of the processes the tests start)
# stays on this script's stderr, where it cannot break a libtest result line (M3QA-018, DC-QA018).
HOH_RELEASE_BUNDLE="$work/hoh-release.tar.gz" HOH_RELEASE_BUNDLE_V2="$work/hoh-release-v2.tar.gz" \
  HOH_EXPECTED_COMMIT="$HOH_BUILD_COMMIT" \
  cargo test -p hoh-app --locked --test deployment -- --include-ignored --test-threads=1 | tee "$log" || rc=$?
if [[ -n "${HOH_EVIDENCE_DIR:-}" ]]; then
  python3 scripts/product/test_results.py --log "$log" --out "$HOH_EVIDENCE_DIR/test-results.jsonl"
fi
[[ "$rc" -eq 0 ]] || exit "$rc"
python3 scripts/product/test_results.py --log "$log" --check-stdout
passed="$(grep -Eo 'test result: [a-z]+\. [0-9]+ passed' "$log" | awk '{s+=$4} END {print s+0}')"
if [[ "$passed" -eq 0 ]] || ! grep -Eq '^test test_005_06_[a-z0-9_]+ \.\.\. ok$' "$log"; then
  echo "NO_TESTS: TEST-005-06 did not pass in the deployment check; an empty or all-ignored suite is not evidence." >&2
  exit 3
fi
if ! grep -Eq '^test resource_upgrade::test_047_03_[a-z0-9_]+ \.\.\. ok$' "$log"; then
  echo "NO_TESTS: the upgrade's TEST-047-03 did not pass in the deployment check." >&2
  exit 3
fi
echo "deployment: $passed tests passed." >&2
