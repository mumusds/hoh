#!/usr/bin/env python3
"""docs/adr/ADR-031.md, 设计修正 2, E: every place in the product's non-test code that starts or replaces a process,
and every place that makes the plain spawner, is listed with its owner, and none of them is in a client's or doctor's
source but the start gate.

The places are found by construction, not by text: a clippy run of its own (the members' libraries and binaries, so no
test code) with a configuration that names the calls that start or replace a process (`Command::spawn`, `output`,
`status`, `CommandExt::exec`, `pre_exec`), the type `hoh_core::spawn::Direct` (made only by `Direct::plain` or
`Direct::shared`, so every making names its type) and the host-only functions listed in spawn-sites.json. Clippy
resolves each with type information, so an alias of `exec`, a re-export or a method called through a trait is found
as well; a thread's `spawn` or an HTTP response's `status` is not one of them. The run caps lints at warn (so every
crate is checked, whatever a hit in an earlier one) and builds into a target directory of its own.

spawn-sites.json lists each (file, item) with its count, its owner and its reason:
- `spawner`: the plain spawner itself (crates/hoh-core/src/spawn.rs only);
- `gate`: the client's start gate, which starts through a plain spawner under its tickets (crates/hoh-app/src/owned.rs
  only);
- `host-only`: code only the host runs (`__host`, `__gate`), which makes its own plain spawner;
- `entry`: an internal entry of its own process that registers no client signal state (`__unit`);
- `test-seam`: a library function only tests call (compat registration).
The scan fails on a place not listed, a count that differs, an entry no longer found, an owner not allowed for its
file, a client file (the list `client_files`) holding any listed place but the gate's, and a call of a host-only
function from a client file. The list is a check beside the type system, not the proof of ownership: library code
takes a `Spawner` with no default, so a caller that leaves it out does not compile.
"""

import argparse
import collections
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SITES = ROOT / "scripts" / "product" / "spawn-sites.json"

# Calls that start or replace a process.
SPAWN_METHODS = (
    "std::process::Command::spawn",
    "std::process::Command::output",
    "std::process::Command::status",
    "std::os::unix::process::CommandExt::exec",
    "std::os::unix::process::CommandExt::pre_exec",
)
DIRECT = "hoh_core::spawn::Direct"
OWNERS = {
    "spawner": ("crates/hoh-core/src/spawn.rs",),
    "gate": ("crates/hoh-app/src/owned.rs",),
    "host-only": None,
    "entry": None,
    "test-seam": None,
}
LINTS = ("clippy::disallowed_methods", "clippy::disallowed_types")


def configuration(host_only):
    """The clippy configuration of the scan's own run."""
    methods = "".join(f'    {{ path = "{m}", reason = "spawn-scan" }},\n' for m in (*SPAWN_METHODS, *host_only))
    return (
        f"disallowed-methods = [\n{methods}]\n"
        f'disallowed-types = [\n    {{ path = "{DIRECT}", reason = "spawn-scan" }},\n]\n'
    )


def is_client(path, client_files):
    return any(path == c or (c.endswith("/") and path.startswith(c)) for c in client_files)


def judge(found, manifest):
    """The scan's findings: `found` is [(file, item)] as clippy reported them (item: the method or type path), one per
    place; `manifest` is spawn-sites.json. A list of problems, empty when the places are as listed."""
    problems = []
    client_files = manifest["client_files"]
    host_only = set(manifest["host_only_functions"])
    counted = collections.Counter((f, i) for f, i in found if i not in host_only)
    for f, i in found:
        if i in host_only and is_client(f, client_files):
            problems.append(f"{f}: a client file calls the host-only function {i}")
    listed = {}
    for entry in manifest["sites"]:
        key = (entry["file"], entry["item"])
        if key in listed:
            problems.append(f"{key[0]}: {key[1]} is listed twice")
        listed[key] = entry
        owner = entry.get("owner")
        if owner not in OWNERS:
            problems.append(f"{key[0]}: {key[1]} has no known owner ({owner!r})")
        elif OWNERS[owner] is not None and key[0] not in OWNERS[owner]:
            problems.append(f"{key[0]}: the owner {owner} is only for {', '.join(OWNERS[owner])}")
        if not str(entry.get("reason", "")).strip():
            problems.append(f"{key[0]}: {key[1]} is listed without a reason")
        if is_client(key[0], client_files) and owner != "gate":
            problems.append(f"{key[0]}: a client file starts processes or makes a plain spawner ({key[1]}) outside the gate")
    for key, count in sorted(counted.items()):
        if is_client(key[0], client_files) and key[0] not in OWNERS["gate"]:
            problems.append(f"{key[0]}: a client file starts processes or makes a plain spawner ({key[1]}) outside the gate")
        entry = listed.get(key)
        if entry is None:
            problems.append(f"{key[0]}: {key[1]} x{count} is not listed in {SITES.name}")
        elif entry.get("count") != count:
            problems.append(f"{key[0]}: {key[1]} is listed x{entry.get('count')} but found x{count}")
    for key in sorted(set(listed) - set(counted)):
        problems.append(f"{key[0]}: {key[1]} is listed but no longer found")
    return list(dict.fromkeys(problems))


def found_in(messages):
    """[(file, item)] from cargo's JSON messages: one per place a scan lint names, and the configuration's own
    complaints (an entry that does not resolve) as problems."""
    found, problems, seen = [], [], set()
    for line in messages.splitlines():
        try:
            message = json.loads(line)
        except ValueError:
            continue
        if message.get("reason") != "compiler-message":
            continue
        diagnostic = message["message"]
        text = diagnostic.get("message", "")
        if "does not refer to a reachable function" in text or "does not refer to an existing" in text:
            problems.append(f"the scan's clippy configuration: {text}")
            continue
        if (diagnostic.get("code") or {}).get("code") not in LINTS:
            continue
        span = next(s for s in diagnostic["spans"] if s["is_primary"])
        item = text.split("`")[1] if text.count("`") >= 2 else text
        place = (span["file_name"], span["line_start"], span["column_start"], item)
        if place in seen:  # the binary and the library both compile a shared module
            continue
        seen.add(place)
        found.append((span["file_name"], item))
    return found, problems


def run_clippy(host_only):
    """The scan's clippy run: its JSON messages, or SystemExit when it does not finish."""
    env = {k: v for k, v in os.environ.items()
           if not (k.startswith("CARGO_PROFILE_") or k.endswith("RUSTFLAGS") or "WRAPPER" in k)}
    env.pop("RUSTC", None)
    with tempfile.TemporaryDirectory() as conf:
        Path(conf, "clippy.toml").write_text(configuration(host_only))
        env.update(CLIPPY_CONF_DIR=conf, RUSTFLAGS="--cap-lints warn")
        done = subprocess.run(
            ["cargo", "clippy", "--workspace", "--lib", "--bins", "--locked", "--message-format=json",
             "--target-dir", str(ROOT / "target" / "spawn-scan")],
            cwd=ROOT, env=env, capture_output=True, text=True, check=False)
    if done.returncode != 0:
        sys.stderr.write(done.stderr[-4000:])
        raise SystemExit("spawn-scan: the scan's clippy run failed")
    return done.stdout


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--sites", type=Path, default=SITES)
    args = parser.parse_args(argv)
    manifest = json.loads(args.sites.read_text())
    found, problems = found_in(run_clippy(manifest["host_only_functions"]))
    problems = list(dict.fromkeys(problems + judge(found, manifest)))
    if problems:
        for problem in problems:
            print(f"spawn-scan: {problem}", file=sys.stderr)
        return 1
    host_only = set(manifest["host_only_functions"])
    calls = sum(1 for _, item in found if item in host_only)
    print(f"spawn-scan: {len(found) - calls} places, each listed with its owner; {calls} calls of host-only functions, "
          "none from a client file")
    return 0


if __name__ == "__main__":
    sys.exit(main())
