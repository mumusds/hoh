#!/usr/bin/env python3
"""docs/adr/ADR-031.md, 设计修正 2, E, as its corrections after the second implementation review leave it: the client's
boundary is the crate graph, the rest is listed.

1. The boundary is cargo's: the client crate `hoh-client` (bare `hoh`, `-p`, attach, doctor, their gate and stderr
   writer) has no normal dependency path to the host's crate `hoh-app` (host, verify, the agent gate, compat). The scan
   asserts it from `cargo metadata` (normal and build dependencies; a dev-dependency is only its tests'). Client code
   cannot name host code at all, so no text of it is parsed: the boundary guards against ordinary regressions, and the
   compiler enforces it.
2. Every place in the members' non-test code that starts or replaces a process (`Command::spawn`, `output`, `status`,
   `CommandExt::exec`, `pre_exec`, nix's `fork`, `exec*`, `daemon`, `posix_spawn*`) or makes the plain spawner
   (`hoh_core::spawn::Direct`), and every call of a host-only function, is found by a clippy run of the scan's own
   (type-resolved: an alias, a re-export or a call through a trait is found; a thread's `spawn` or an HTTP `status` is
   not), in the dev and the release profile, the places of both counted once. The scan's lints are forced
   (`--force-warn`), so no `allow` hides a place from it: persistence's allow for its database calls included (CLT-1450). tokio's
   `process` starts are listed too, though no member enables that feature (CLT-1454). spawn-sites.json lists each (file,
   item) with its count, owner and reason: `spawner` (hoh-core's spawn.rs), `gate` (the client's one start gate),
   `host-only`, `entry` and `test-seam` (only in `host_only_files`). A client file holds no listed place but the gate's
   and calls no host-only function.
3. stderr (C): in client, entry and library files, every call that writes fd 2 other than through the client's writer
   (`std::io::stderr`, `rustix::stdio::stderr`, `rustix::stdio::dup2_stderr`, `eprint!`, `eprintln!`, `dbg!`, and the
   client's own `stderr_functions`) is listed in `stderr_sites` with its count and reason; and a client file never names
   stderr by its path (`/dev/stderr`, `/proc/self/fd/2`, ...), the one place with no type to resolve.
4. Every `.rs` file under `crates/*/src` is in exactly one class (`client_files`, `host_only_files`, `entry_files`,
   `library_dirs`), else the scan fails.
The list is a check beside the type system, not the proof of ownership: library code takes a `Spawner` with no
default, `Helper::spawn` is private to hoh-core's spawn module, and the gate is made only by the signal state.
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
    "nix::unistd::fork",
    "nix::unistd::execv",
    "nix::unistd::execve",
    "nix::unistd::execvp",
    "nix::unistd::execvpe",
    "nix::unistd::fexecve",
    "nix::unistd::execveat",
    "nix::unistd::daemon",
    "nix::spawn::posix_spawn",
    "nix::spawn::posix_spawnp",
)
# Starts that exist only with a feature no member enables today (tokio's `process`): listed all the same, so enabling it
# makes every use a place the scan judges (CLT-1454); while their paths do not resolve, that is no problem.
OPTIONAL_METHODS = (
    "tokio::process::Command::spawn",
    "tokio::process::Command::output",
    "tokio::process::Command::status",
)
DIRECT = "hoh_core::spawn::Direct"
OWNERS = {
    "spawner": ("crates/hoh-core/src/spawn.rs",),
    "gate": ("crates/hoh-client/src/signals/owned.rs",),
    "host-only": None,
    "entry": None,
    "test-seam": None,
}
# Calls and macros that write stderr other than through a client's writer (C).
STDERR_METHODS = ("std::io::stderr", "rustix::stdio::stderr", "rustix::stdio::dup2_stderr")
STDERR_MACROS = ("std::eprint", "std::eprintln", "std::dbg")
STDERR_ITEMS = (*STDERR_METHODS, *STDERR_MACROS)
CLASSES = ("client_files", "host_only_files", "entry_files", "library_dirs")
# The crates of the boundary: the client never depends on the host's (CLT-1350).
CLIENT_CRATE, HOST_CRATE = "hoh-client", "hoh-app"
LINTS = ("clippy::disallowed_methods", "clippy::disallowed_types", "clippy::disallowed_macros")


def configuration(host_only, stderr_functions=()):
    """The clippy configuration of the scan's own run."""
    methods = "".join(f'    {{ path = "{m}", reason = "spawn-scan" }},\n'
                      for m in (*SPAWN_METHODS, *OPTIONAL_METHODS, *STDERR_METHODS, *host_only, *stderr_functions))
    macros = "".join(f'    {{ path = "{m}", reason = "spawn-scan" }},\n' for m in STDERR_MACROS)
    return (
        f"disallowed-methods = [\n{methods}]\n"
        f'disallowed-types = [\n    {{ path = "{DIRECT}", reason = "spawn-scan" }},\n]\n'
        f"disallowed-macros = [\n{macros}]\n"
    )


def is_client(path, client_files):
    return any(path == c or (c.endswith("/") and path.startswith(c)) for c in client_files)


def stderr_items(manifest):
    """What writes fd 2 other than through the client's writer: std's and rustix's stderr, the stderr macros, and the
    functions that write fd 2 for the client (`stderr_functions`)."""
    return (*STDERR_ITEMS, *manifest.get("stderr_functions", []))


def stderr_problems(found, manifest):
    """The client files' direct stderr writes (C) against `stderr_sites`."""
    problems = []
    client_files = manifest["client_files"] + manifest.get("entry_files", []) + manifest.get("library_dirs", [])
    items = stderr_items(manifest)
    counted = collections.Counter((f, i) for f, i in found if i in items and is_client(f, client_files))
    listed = {}
    for entry in manifest.get("stderr_sites", []):
        key = (entry["file"], entry["item"])
        listed[key] = entry
        if not is_client(key[0], client_files):
            problems.append(f"{key[0]}: {key[1]} is listed as a stderr site but is not a client, entry or library file")
        if not str(entry.get("reason", "")).strip():
            problems.append(f"{key[0]}: {key[1]} is listed without a reason")
    for key, count in sorted(counted.items()):
        entry = listed.get(key)
        if entry is None:
            problems.append(f"{key[0]}: a client, entry or library file writes stderr directly ({key[1]} x{count}), "
                            "not through the client's writer")
        elif entry.get("count") != count:
            problems.append(f"{key[0]}: {key[1]} is listed x{entry.get('count')} as a stderr site but found x{count}")
    for key in sorted(set(listed) - set(counted)):
        problems.append(f"{key[0]}: {key[1]} is listed as a stderr site but no longer found")
    return problems


# stderr by its path: no type to resolve, so the client's sources are searched for the paths themselves (not a parse of
# them: a comment naming one is refused too). The one lexical check left (CLT-1254).
STDERR_PATHS = ("/dev/stderr", "/proc/self/fd/2", "/proc/thread-self/fd/2", "/dev/fd/2")


def path_problems(manifest, root=ROOT):
    """Client files that name stderr by its path."""
    problems = []
    for entry in manifest["client_files"]:
        base = root / entry
        for file in sorted(base.rglob("*.rs")) if entry.endswith("/") else [base]:
            text = file.read_text()
            for path in STDERR_PATHS:
                if path in text:
                    problems.append(f"{file.relative_to(root).as_posix()}: a client file names stderr by its path "
                                    f"({path}), around its writer")
    return problems


def closure_problems(metadata):
    """The boundary from `cargo metadata`'s resolve graph: the client crate's normal and build dependencies, followed
    transitively, never reach the host's crate (a dev-dependency is only the client's tests')."""
    names = {package["id"]: package["name"] for package in metadata["packages"]}
    by_name = {name: pid for pid, name in names.items()}
    if CLIENT_CRATE not in by_name or HOST_CRATE not in by_name:
        return [f"cargo metadata: no {CLIENT_CRATE} or no {HOST_CRATE} in the workspace"]
    deps = {node["id"]: [d["pkg"] for d in node.get("deps", [])
                         if any(kind.get("kind") in (None, "build") for kind in d.get("dep_kinds", []))]
            for node in metadata["resolve"]["nodes"]}
    seen, stack, path = set(), [by_name[CLIENT_CRATE]], {by_name[CLIENT_CRATE]: [CLIENT_CRATE]}
    while stack:
        node = stack.pop()
        if node in seen:
            continue
        seen.add(node)
        for dep in deps.get(node, []):
            path.setdefault(dep, path[node] + [names[dep]])
            stack.append(dep)
    host = by_name[HOST_CRATE]
    if host in seen:
        return [f"{CLIENT_CRATE} depends on {HOST_CRATE} ({' -> '.join(path[host])}): client code could name host code"]
    return []


def run_metadata():
    """`cargo metadata` of the workspace, or SystemExit."""
    done = subprocess.run(["cargo", "metadata", "--format-version", "1", "--locked"], cwd=ROOT, capture_output=True,
                          text=True, check=False)
    if done.returncode != 0:
        sys.stderr.write(done.stderr[-4000:])
        raise SystemExit("spawn-scan: cargo metadata failed")
    return json.loads(done.stdout)


def classify(manifest, root=ROOT):
    """Each non-test `.rs` file under `crates/*/src` with its class, and the problems: a file in no class, or in more
    than one."""
    classes, problems = {}, []
    for file in sorted((root / "crates").glob("*/src/**/*.rs")):
        rel = file.relative_to(root).as_posix()
        hits = [cls for cls in CLASSES if is_client(rel, manifest.get(cls, []))]
        if not hits:
            problems.append(f"{rel}: in no class of {SITES.name} (client, host-only, entry or library): "
                            "classify it first")
        elif len(hits) > 1:
            problems.append(f"{rel}: in more than one class ({', '.join(hits)})")
        else:
            classes[rel] = hits[0]
    return classes, problems


def judge(found, manifest):
    """The scan's findings: `found` is [(file, item)] as clippy reported them (item: the method or type path), one per
    place; `manifest` is spawn-sites.json. A list of problems, empty when the places are as listed."""
    problems = []
    client_files = manifest["client_files"]
    host_only = set(manifest["host_only_functions"])
    host_only_files = manifest["host_only_files"]
    writes = stderr_items(manifest)
    counted = collections.Counter((f, i) for f, i in found if i not in host_only and i not in writes)
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
        elif OWNERS[owner] is None and not is_client(key[0], host_only_files):
            problems.append(f"{key[0]}: the owner {owner} is only for the host-only modules (host_only_files)")
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
    problems += stderr_problems(found, manifest)
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
            named = text.split("`")[1] if text.count("`") >= 2 else ""
            if named not in OPTIONAL_METHODS:
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


def rustflags():
    """The scan's clippy flags: lints capped at warn, so every crate is checked whatever a hit in an earlier one; the
    scan's own lints forced, so no allow hides a place (CLT-1450, CLT-1452)."""
    return "--cap-lints warn " + " ".join(f"--force-warn {lint}" for lint in LINTS)


def run_clippy(host_only, stderr_functions=(), release=False):
    """The scan's clippy run, in the dev or the release profile: its JSON messages, or SystemExit when it does not
    finish."""
    env = {k: v for k, v in os.environ.items()
           if not (k.startswith("CARGO_PROFILE_") or k.endswith("RUSTFLAGS") or "WRAPPER" in k)}
    env.pop("RUSTC", None)
    with tempfile.TemporaryDirectory() as conf:
        Path(conf, "clippy.toml").write_text(configuration(host_only, stderr_functions))
        # Forced: an allow of the scan's lints (persistence's allow for its database calls, or one a file adds) hides nothing from
        # the scan (CLT-1450, CLT-1452).
        env.update(CLIPPY_CONF_DIR=conf, RUSTFLAGS=rustflags())
        done = subprocess.run(
            ["cargo", "clippy", "--workspace", "--lib", "--bins", "--locked", "--message-format=json",
             "--target-dir", str(ROOT / "target" / "spawn-scan"), *(["--release"] if release else [])],
            cwd=ROOT, env=env, capture_output=True, text=True, check=False)
    if done.returncode != 0:
        sys.stderr.write(done.stderr[-4000:])
        raise SystemExit("spawn-scan: the scan's clippy run failed")
    return done.stdout


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[1])
    parser.add_argument("--sites", type=Path, default=SITES)
    args = parser.parse_args(argv)
    manifest = json.loads(args.sites.read_text())
    host_only, writers = manifest["host_only_functions"], manifest.get("stderr_functions", [])
    problems = closure_problems(run_metadata())
    # Both profiles: code under cfg(not(debug_assertions)) is compiled in the release one only; a place both compile is
    # one place.
    found, configured = found_in(run_clippy(host_only, writers) + "\n" + run_clippy(host_only, writers, release=True))
    classes, classified = classify(manifest)
    problems = list(dict.fromkeys(problems + configured + judge(found, manifest) + classified + path_problems(manifest)))
    if problems:
        for problem in problems:
            print(f"spawn-scan: {problem}", file=sys.stderr)
        return 1
    calls = sum(1 for _, item in found if item in set(host_only))
    writes = sum(1 for _, item in found if item in stderr_items(manifest)) - sum(
        1 for f, item in found if item in stderr_items(manifest) and is_client(f, manifest["host_only_files"]))
    places = sum(1 for _, item in found if item not in set(host_only) and item not in stderr_items(manifest))
    print(f"spawn-scan: {CLIENT_CRATE} does not depend on {HOST_CRATE}; {places} places, each listed with its owner; "
          f"{calls} calls of host-only functions, none from a client file; {writes} direct stderr writes in client, "
          f"entry and library files, each listed; {len(classes)} files each in one class")
    return 0


if __name__ == "__main__":
    sys.exit(main())
