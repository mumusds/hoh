#!/usr/bin/env python3
"""docs/adr/ADR-031.md, 设计修正 2, E: every place in the product's non-test code that starts or replaces a process,
and every place that makes the plain spawner, is listed with its owner, and none of them is in a client's or doctor's
source but the start gate; a client's or doctor's source writes stderr only where the list says (C), and never refers
to the host-only modules (A, CLT-1007).

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
The owners `host-only`, `entry` and `test-seam` are allowed only in the host-only modules (`host_only_files`: the
host, the verifier, the agent gate, the compat registration and the installer's `__unit`): the plain spawner is made
nowhere else (CTO decision, CLT-1007).

The scan fails on a place not listed, a count that differs, an entry no longer found, an owner not allowed for its
file, a client file (the list `client_files`) holding any listed place but the gate's, and a call of a host-only
function from a client file. The list is a check beside the type system, not the proof of ownership: library code
takes a `Spawner` with no default, so a caller that leaves it out does not compile.

stderr (C): the same clippy run names the calls that write stderr other than through the client's writer
(`std::io::stderr`, `rustix::stdio::stderr`, and the macros `eprint!`, `eprintln!`, `dbg!`); in a client file each one
is listed in `stderr_sites` with its count and reason (the writer's own descriptor, which also serves the direct write
before registration, the exit's bounded direct write after the writer ended, and the panic hook's words). Elsewhere
(the host, the gate, the libraries) they are not the client's.

The module boundary (A, CLT-1007): a client file (its non-test code: comments, strings and `#[cfg(test)]` modules left
out) never names `hoh_app::host`, `verify`, `gate` or `compat`, by a path or anywhere in a `use` tree (nested groups,
`self`, `*`, raw identifiers), never gives the crate another name (`hoh_app as …`), builds no `hoh_app::$…` path in a
macro, has no `#[path]` module, never refers to the installer's module, and opens no `/dev/stderr` or `/proc/self/fd/2`.
An entry or shared file (`main.rs`; `lib.rs`, `unit.rs`, `layout.rs`) never brings those modules' names in with `use`,
so nothing re-exports them to a client file.

Corrections after the implementation review (CLT-1250..1254): every non-test `.rs` file under `crates/*/src` is in
exactly one class (`client_files`, `host_only_files`, `shared_files`, `entry_files`, `library_dirs`), else the scan
fails, so a new module is classified before it passes; nix's `fork`, `exec*`, `daemon` and `posix_spawn*` are starts;
the functions that write fd 2 for the client (`stderr_functions`) are counted as direct stderr writes; and clippy runs
in the dev and the release profile, the places of both counted once.
"""

import argparse
import collections
import json
import os
import re
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
DIRECT = "hoh_core::spawn::Direct"
OWNERS = {
    "spawner": ("crates/hoh-core/src/spawn.rs",),
    "gate": ("crates/hoh-app/src/owned.rs",),
    "host-only": None,
    "entry": None,
    "test-seam": None,
}
# Calls and macros that write stderr other than through a client's writer (C).
STDERR_METHODS = ("std::io::stderr", "rustix::stdio::stderr")
STDERR_MACROS = ("std::eprint", "std::eprintln", "std::dbg")
STDERR_ITEMS = (*STDERR_METHODS, *STDERR_MACROS)
# The modules a client file never refers to (A, CLT-1007).
BOUNDARY = ("host", "verify", "gate", "compat")
CLASSES = ("client_files", "host_only_files", "shared_files", "entry_files", "library_dirs")
LINTS = ("clippy::disallowed_methods", "clippy::disallowed_types", "clippy::disallowed_macros")


def configuration(host_only, stderr_functions=()):
    """The clippy configuration of the scan's own run."""
    methods = "".join(f'    {{ path = "{m}", reason = "spawn-scan" }},\n'
                      for m in (*SPAWN_METHODS, *STDERR_METHODS, *host_only, *stderr_functions))
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
    client_files = manifest["client_files"] + manifest.get("entry_files", [])
    items = stderr_items(manifest)
    counted = collections.Counter((f, i) for f, i in found if i in items and is_client(f, client_files))
    listed = {}
    for entry in manifest.get("stderr_sites", []):
        key = (entry["file"], entry["item"])
        listed[key] = entry
        if not is_client(key[0], client_files):
            problems.append(f"{key[0]}: {key[1]} is listed as a stderr site but is not a client file")
        if not str(entry.get("reason", "")).strip():
            problems.append(f"{key[0]}: {key[1]} is listed without a reason")
    for key, count in sorted(counted.items()):
        entry = listed.get(key)
        if entry is None:
            problems.append(f"{key[0]}: a client file writes stderr directly ({key[1]} x{count}), not through its writer")
        elif entry.get("count") != count:
            problems.append(f"{key[0]}: {key[1]} is listed x{entry.get('count')} as a stderr site but found x{count}")
    for key in sorted(set(listed) - set(counted)):
        problems.append(f"{key[0]}: {key[1]} is listed as a stderr site but no longer found")
    return problems


def code_only(source, keep_strings=False):
    """`source` with its comments blanked, its string and character literals blanked (unless `keep_strings`), raw
    identifiers written plainly (`r#host` as `host`) and its `#[cfg(test)]` modules left out."""
    out, i, n = [], 0, len(source)

    def ident_before(i):
        return i > 0 and (source[i - 1].isalnum() or source[i - 1] == "_")

    while i < n:
        c = source[i]
        if source.startswith("//", i):
            j = source.find("\n", i)
            i = n if j < 0 else j
        elif source.startswith("/*", i):
            depth, i = 1, i + 2
            while i < n and depth:
                if source.startswith("/*", i):
                    depth, i = depth + 1, i + 2
                elif source.startswith("*/", i):
                    depth, i = depth - 1, i + 2
                else:
                    i += 1
            out.append(" ")
        elif c in "rb" and not ident_before(i) and re.match(r"(b?r#*\"|b\")", source[i:]):
            m = re.match(r"(b?)(r?)(#*)\"", source[i:])
            raw, hashes = m.group(2), m.group(3)
            start = i + m.end()
            if raw:
                end = source.find('"' + hashes, start)
                i = n if end < 0 else end + 1 + len(hashes)
            else:
                j = start
                while j < n and source[j] != '"':
                    j += 2 if source[j] == "\\" else 1
                i = j + 1
            out.append(source[start - 1:i] if keep_strings else '""')
        elif c == "r" and not ident_before(i) and re.match(r"r#[A-Za-z_]", source[i:]):
            i += 2  # a raw identifier: the name itself
        elif c == '"':
            j = i + 1
            while j < n and source[j] != '"':
                j += 2 if source[j] == "\\" else 1
            out.append(source[i:j + 1] if keep_strings else '""')
            i = j + 1
        elif c == "'" and i + 2 < n and (source[i + 1] == "\\" or source[i + 2] == "'"):
            j = source.find("'", i + 2 if source[i + 1] == "\\" else i + 1)
            i = n if j < 0 else j + 1
            out.append("' '")
        else:
            out.append(c)
            i += 1
    code = "".join(out)
    # `#[cfg(test)]` followed by an inline module: dropped with its braces.
    while True:
        m = re.search(r"#\[cfg\(test\)\]\s*(pub(\([^)]*\))?\s+)?mod\s+\w+\s*\{", code)
        if not m:
            return code
        depth, j = 1, m.end()
        while j < len(code) and depth:
            depth += {"{": 1, "}": -1}.get(code[j], 0)
            j += 1
        code = code[:m.start()] + code[j:]


def tree_heads(code, i):
    """The first names a path or `use` tree starting at `code[i]` reaches (every name of a group, nested groups
    included, `*` and `self` as themselves), and where the tree ends."""
    def skip(i):
        while i < len(code) and code[i].isspace():
            i += 1
        return i
    i = skip(i)
    if i < len(code) and code[i] == "{":
        heads, i = [], i + 1
        while True:
            i = skip(i)
            if i >= len(code) or code[i] == "}":
                return heads, i + 1
            if code[i] == ",":
                i += 1
                continue
            more, i = tree_heads(code, i)
            heads += more
    if i < len(code) and code[i] == "*":
        return ["*"], i + 1
    m = re.match(r"\w+", code[i:])
    if not m:
        return [""], i
    head, i = m.group(0), i + m.end()
    j = skip(i)
    if code.startswith("::", j):
        _, i = tree_heads(code, j + 2)
    return [head], i


def use_trees(code, root):
    """The heads of every path after `root ::` in `code`: (head, position)."""
    found = []
    for m in re.finditer(r"\b" + root + r"\s*::", code):
        heads, _ = tree_heads(code, m.end())
        found += [(head, m.start()) for head in heads]
    return found


def boundary_problems(path, source, cls="client_files"):
    """The places where the file `path` (its text `source`, of class `cls`) refers to a host-only module (A, CLT-1007,
    CLT-1251..1254)."""
    code, problems = code_only(source), []
    if cls in ("entry_files", "shared_files"):
        # Brought in with `use` (and so re-exportable): never; a full path in `main.rs` is its `__host`, `__gate` entry.
        for m in re.finditer(r"\buse\b([^;]*);", code):
            for root in ("hoh_app", "crate", "super", "self"):
                for head, _ in use_trees(m.group(1), root):
                    if head in BOUNDARY or head in ("*",):
                        problems.append(f"{path}: an {cls[:-6]} file brings {root}::{head} in with use, which would "
                                        "re-export a host-only module")
        return list(dict.fromkeys(problems))
    if cls != "client_files":
        return problems
    if re.search(r"\bhoh_app\s+as\b", code):
        problems.append(f"{path}: a client file gives hoh_app another name, which would hide a host-only module")
    if re.search(r"\bhoh_app\s*::\s*\$", code):
        problems.append(f"{path}: a client file builds a hoh_app path in a macro, which would hide a host-only module")
    if re.search(r"#\s*\[\s*path\b", code):
        problems.append(f"{path}: a client file has a #[path] module, which the scan cannot classify")
    if re.search(r"\b(crate|super)\s*::\s*installer\b", code):
        problems.append(f"{path}: a client file refers to the installer's module (__unit), host-only")
    strings = code_only(source, keep_strings=True)
    if re.search(r"/dev/stderr|/proc/self/fd/2\b|/dev/fd/2\b", strings):
        problems.append(f"{path}: a client file opens stderr by its path, around its writer")
    for head, _ in use_trees(code, "hoh_app"):
        if head in BOUNDARY:
            problems.append(f"{path}: a client file refers to the host-only module hoh_app::{head}")
        elif head in ("*", "self"):
            problems.append(f"{path}: a client file imports hoh_app::{head}, which reaches the host-only modules")
    return list(dict.fromkeys(problems))


def classify(manifest, root=ROOT):
    """Each non-test `.rs` file under `crates/*/src` with its class, and the problems: a file in no class, or in more
    than one."""
    classes, problems = {}, []
    for file in sorted((root / "crates").glob("*/src/**/*.rs")):
        rel = file.relative_to(root).as_posix()
        if file.name == "tests.rs" or "/tests/" in rel:
            continue
        hits = [cls for cls in CLASSES if is_client(rel, manifest.get(cls, []))]
        if not hits:
            problems.append(f"{rel}: in no class of {SITES.name} (client, host-only, shared, entry or library): "
                            "classify it first")
        elif len(hits) > 1:
            problems.append(f"{rel}: in more than one class ({', '.join(hits)})")
        else:
            classes[rel] = hits[0]
    return classes, problems


def sources(manifest, root=ROOT):
    """Each classified file's path, class and text, and the classification's problems."""
    classes, problems = classify(manifest, root)
    return [(rel, cls, (root / rel).read_text()) for rel, cls in classes.items()], problems


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


def run_clippy(host_only, stderr_functions=(), release=False):
    """The scan's clippy run, in the dev or the release profile: its JSON messages, or SystemExit when it does not
    finish."""
    env = {k: v for k, v in os.environ.items()
           if not (k.startswith("CARGO_PROFILE_") or k.endswith("RUSTFLAGS") or "WRAPPER" in k)}
    env.pop("RUSTC", None)
    with tempfile.TemporaryDirectory() as conf:
        Path(conf, "clippy.toml").write_text(configuration(host_only, stderr_functions))
        env.update(CLIPPY_CONF_DIR=conf, RUSTFLAGS="--cap-lints warn")
        done = subprocess.run(
            ["cargo", "clippy", "--workspace", "--lib", "--bins", "--locked", "--message-format=json",
             "--target-dir", str(ROOT / "target" / "spawn-scan"), *(["--release"] if release else [])],
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
    host_only, writers = manifest["host_only_functions"], manifest.get("stderr_functions", [])
    # Both profiles: code under cfg(not(debug_assertions)) is compiled in the release one only; a place both compile is
    # one place.
    found, problems = found_in(run_clippy(host_only, writers) + "\n" + run_clippy(host_only, writers, release=True))
    problems = list(dict.fromkeys(problems + judge(found, manifest)))
    files, classified = sources(manifest)
    problems += classified
    for path, cls, source in files:
        problems += boundary_problems(path, source, cls)
    problems = list(dict.fromkeys(problems))
    if problems:
        for problem in problems:
            print(f"spawn-scan: {problem}", file=sys.stderr)
        return 1
    calls = sum(1 for _, item in found if item in set(host_only))
    writes = sum(1 for f, item in found if item in stderr_items(manifest)
                 and is_client(f, manifest["client_files"] + manifest.get("entry_files", [])))
    places = sum(1 for _, item in found if item not in set(host_only) and item not in stderr_items(manifest))
    print(f"spawn-scan: {places} places, each listed with its owner; {calls} calls of host-only functions, "
          f"none from a client file; {writes} direct stderr writes in client and entry files, each listed; "
          f"{len(files)} files each in one class, none of the client, entry or shared ones reaching a host-only module")
    return 0


if __name__ == "__main__":
    sys.exit(main())
