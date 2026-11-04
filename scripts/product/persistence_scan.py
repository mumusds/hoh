#!/usr/bin/env python3
"""AC-062-05: SQLite is opened and SQL executed only by the Persistence module (crates/hoh-core/src/persistence/).

The boundary is enforced by the compiler, and this scan checks that nothing switches it off (M2T01R-16..21):
1. Dependencies, by package identity from `cargo metadata` (renamed packages and workspace inheritance resolved):
   no workspace member but hoh-core reaches a SQLite package (any package whose name contains "sqlite", such as
   rusqlite and libsqlite3-sys) through normal or build dependencies, other than through hoh-core itself; hoh-core
   has it only as a normal dependency (its build script cannot use it). Dev-dependencies (test code) are allowed and
   listed. rusqlite may not enable features beyond the known set (others add APIs that write the database).
2. Inside hoh-core, clippy's `disallowed-methods` (clippy.toml) forbids every rusqlite call that opens a database or
   executes SQL. Clippy resolves each call with type information, so it holds however a connection was obtained
   (aliases, re-exports, generics, closures, trait methods, macros); the static check runs clippy with -D warnings
   and fails on an entry that does not resolve. Iterating a result that persistence itself prepared and handed out
   (a `MappedRows`) goes through `Iterator::next` and cannot be told apart: persistence hands out no SQLite object.
   This scan checks that clippy.toml lists all REQUIRED methods
   without `allow-invalid`, that no other clippy configuration exists, that no Cargo `[lints]` table or
   cargo configuration that applies (the repository's, its ancestors', `$CARGO_HOME`'s) sets rustflags, a compiler or
   wrapper, compiler environment or aliases of the check's commands, and that no source outside persistence/ allows or expects
   `clippy::disallowed_methods` or a group holding it (`clippy::style`, `clippy::all`, `warnings`).
3. Lint levels: every target root of every workspace member (from `cargo metadata`, wherever it lies) starts — in
   its leading inner attributes, which alone apply to the whole crate — with `#![forbid(unsafe_code)]` (no way around
   through SQLite's C interface) and `#![forbid(clippy::disallowed_methods)]`, except hoh-core's library root, which
   holds `#![forbid(unsafe_code)]`, only module declarations, each but `persistence` with
   `#[forbid(clippy::disallowed_methods)]`, and the VERSION constant. The workspace has no build scripts (one could
   give its crate another clippy configuration through `cargo:rustc-env`). static.sh lints with `--all-features`, so
   targets with required-features are compiled too. `forbid` makes the compiler
   reject any later `allow` of the lint, however it is written or generated (raw identifiers, `cfg_attr`, macros).
   Every .rs file of the member directories but the build output is also read: no `allow`/`expect` of the lint or a
   group holding it outside persistence/ (an early, readable signal), and no `#[path = …]` (also in `cfg_attr`) or
   `include!`.
   scripts/product/static.sh clears rustflags, compiler and wrappers from the environment (an empty wrapper variable
   overrides any configured one), makes the repository's clippy.toml the configuration (CLIPPY_CONF_DIR), and
   compiles crates/hoh-core/src/clippy_canary.rs, which calls the CANARY_REQUIRED methods outside persistence;
   --canary-log checks that clippy refused each of them there and failed for no other reason. This proves the lint in
   force for hoh-core's library in the environment the check runs in; for the other targets the forbid levels and
   the configuration checks of 2 hold it.
4. Defence in depth: string literals outside persistence/ (and outside tests, benches, examples) that hold a SQL write statement (INSERT/REPLACE INTO,
   UPDATE ... SET, DELETE FROM, UPSERT, ON CONFLICT ... DO UPDATE), decoded as Rust does (escapes, line
   continuations), with literals separated only by commas joined (concat!), are hits.
5. web/ (when present): no SQLite package in package.json dependencies (by name or npm: alias spec), nowhere in a
   lockfile (every key and string value), and no import of one.
Scanning no file at all fails (no empty scan). Prints a JSON summary; with --out appends one test-results.jsonl
line for TEST-062-05. --metadata reads `cargo metadata` JSON from a file (tests).
"""
from __future__ import annotations
import argparse, json, re, subprocess, sys, tomllib
from pathlib import Path

PERSISTENCE = 'crates/hoh-core/src/persistence/'
CORE = 'hoh-core'
SQL_WRITE = re.compile(
    r'\b(?:INSERT\s+(?:OR\s+\w+\s+)?INTO|REPLACE\s+INTO|UPDATE\s+(?:OR\s+\w+\s+)?\S+\s+SET|DELETE\s+FROM|UPSERT'
    r'|ON\s+CONFLICT\b[^;]*?\bDO\s+UPDATE)\b', re.IGNORECASE)
ESCAPE = {'n': '\n', 't': '\t', 'r': '\r', '0': '\0', '\\': '\\', '"': '"', "'": "'"}
LOCKFILES = ('package-lock.json', 'npm-shrinkwrap.json')
# Every rusqlite API that opens a database or executes SQL with the features below.
REQUIRED = {f'rusqlite::{ty}::{m}' for ty, ms in {
    'Connection': ['open', 'open_with_flags', 'open_with_flags_and_vfs', 'open_in_memory', 'open_in_memory_with_flags',
                   'open_in_memory_with_flags_and_vfs', 'execute', 'execute_batch', 'prepare', 'prepare_cached',
                   'prepare_with_flags', 'query_row', 'query_row_and_then', 'query_one', 'pragma', 'pragma_query',
                   'pragma_query_value', 'pragma_update', 'pragma_update_and_check', 'transaction',
                   'transaction_with_behavior', 'unchecked_transaction', 'savepoint', 'savepoint_with_name'],
    'Statement': ['execute', 'insert', 'raw_execute', 'query', 'query_map', 'query_and_then', 'query_row', 'query_one',
                  'exists', 'raw_query'],
    'Transaction': ['new', 'new_unchecked', 'commit', 'rollback', 'finish', 'savepoint', 'savepoint_with_name',
                    'set_drop_behavior'],
    'Savepoint': ['new', 'with_name', 'commit', 'rollback', 'finish', 'savepoint', 'savepoint_with_name',
                  'set_drop_behavior'],
    'Batch': ['new'],
    # Stepping a statement (M2T01R-22).
    'Rows': ['next', 'map', 'mapped', 'and_then'],
}.items() for m in ms}
# The canary calls each of these; the iterator-trait methods below are checked to resolve but not each called.
CANARY_REQUIRED = REQUIRED | {'fallible_streaming_iterator::FallibleStreamingIterator::advance',
                              'fallible_streaming_iterator::FallibleStreamingIterator::next',
                              'fallible_iterator::FallibleIterator::next'}
# Every method of the iterator traits rusqlite's results implement, provided ones included (`count`, `nth`, …;
# M2T01R-26): rusqlite alone uses these traits in this dependency graph.
TRAIT_METHODS = {
    'fallible_streaming_iterator::FallibleStreamingIterator': [
        'advance', 'all', 'any', 'by_ref', 'count', 'filter', 'find', 'for_each', 'fuse', 'get', 'map', 'map_err',
        'map_ref', 'next', 'nth', 'position', 'size_hint', 'skip', 'skip_while', 'take', 'take_while'],
    'fallible_iterator::FallibleIterator': [
        'all', 'any', 'by_ref', 'chain', 'cloned', 'cmp', 'collect', 'count', 'cycle', 'enumerate', 'eq', 'filter',
        'filter_map', 'find', 'find_map', 'flat_map', 'flatten', 'fold', 'for_each', 'fuse', 'ge', 'gt', 'inspect',
        'iterator', 'last', 'le', 'lt', 'map', 'map_err', 'max', 'max_by', 'max_by_key', 'min', 'min_by', 'min_by_key',
        'ne', 'next', 'nth', 'partial_cmp', 'partition', 'peekable', 'position', 'rev', 'scan', 'size_hint', 'skip',
        'skip_while', 'step_by', 'take', 'take_while', 'try_fold', 'unwrap', 'unzip', 'zip'],
}
REQUIRED = CANARY_REQUIRED | {f'{trait}::{m}' for trait, ms in TRAIT_METHODS.items() for m in ms}
KNOWN_RUSQLITE_FEATURES = {'bundled', 'cache', 'default', 'modern_sqlite', 'ffi-sqlite-wasm-rs', 'hashlink'}
# Matched on attribute and Cargo lint text with whitespace removed and `-` read as `_` (M2T01R-23); `disallowed_method`
# is the lint's former name, still accepted by clippy.
SUPPRESSING = re.compile(r'(?<![\w:])(?:clippy::(?:disallowed_methods?|style|all)|warnings)(?![\w:])')


def sqlite_package(name: str) -> bool:
    return 'sqlite' in name.lower()


def decode(body: str) -> str:
    """A Rust (non-raw) string literal body as the compiler reads it."""
    out, i, n = [], 0, len(body)
    while i < n:
        c = body[i]
        if c != '\\' or i + 1 >= n:
            out.append(c)
            i += 1
            continue
        e = body[i + 1]
        if e == '\n':  # continuation: the newline and the next line's leading whitespace are dropped
            i += 2
            while i < n and body[i] in ' \t\n\r':
                i += 1
        elif e == 'x' and re.match(r'[0-7][0-9a-fA-F]', body[i + 2:i + 4]):
            out.append(chr(int(body[i + 2:i + 4], 16)))
            i += 4
        elif e == 'u' and (m := re.match(r'\{([0-9a-fA-F_]{1,8})\}', body[i + 2:])):
            out.append(chr(int(m.group(1).replace('_', ''), 16)))
            i += 2 + m.end()
        else:
            out.append(ESCAPE.get(e, e))
            i += 2
    return ''.join(out)


def split_rust(text: str) -> tuple[str, list[tuple[int, int, str]]]:
    """(code with comments and literals blanked to spaces, [(start, end, decoded literal)])."""
    code, literals, i, n = list(text), [], 0, len(text)

    def blank(a: int, b: int) -> None:
        for k in range(a, min(b, n)):
            if code[k] != '\n':
                code[k] = ' '
    while i < n:
        if text.startswith('//', i):
            j = text.find('\n', i)
            j = n if j < 0 else j
            blank(i, j)
            i = j
            continue
        if text.startswith('/*', i):
            start, depth, i = i, 1, i + 2
            while i < n and depth:
                if text.startswith('/*', i):
                    depth, i = depth + 1, i + 2
                elif text.startswith('*/', i):
                    depth, i = depth - 1, i + 2
                else:
                    i += 1
            blank(start, i)
            continue
        ident_before = i > 0 and (text[i - 1].isalnum() or text[i - 1] == '_')
        raw = None if ident_before else re.match(r'(?:b|c)?r(#*)"', text[i:])
        if raw:
            close = '"' + raw.group(1)
            body_start = i + raw.end()
            end = text.find(close, body_start)
            end = n if end < 0 else end
            literals.append((i, end + len(close), text[body_start:end]))
            blank(i, end + len(close))
            i = end + len(close)
            continue
        quote = re.match(r'"' if ident_before else r'(?:b|c)?"', text[i:])
        if quote:
            j = i + quote.end()
            while j < n and text[j] != '"':
                j += 2 if text[j] == '\\' else 1
            literals.append((i, j + 1, decode(text[i + quote.end():j])))
            blank(i, j + 1)
            i = j + 1
            continue
        if text[i] == "'":
            m = re.match(r"'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F_]+\}|.)|[^\\'\n])'", text[i:])
            if m:
                blank(i, i + m.end())
                i += m.end()
                continue
        i += 1
    return ''.join(code), literals


def literal_strings(code: str, literals: list[tuple[int, int, str]]) -> list[str]:
    """Each literal, and runs of literals separated only by commas and whitespace joined (as concat! does)."""
    out, run = [], []
    for k, (start, _end, value) in enumerate(literals):
        out.append(value)
        if run and re.fullmatch(r'[\s,]*', code[literals[k - 1][1]:start]):
            run.append(value)
        else:
            if len(run) > 1:
                out.append(''.join(run))
            run = [value]
    if len(run) > 1:
        out.append(''.join(run))
    return out


def attributes(code: str) -> list[str]:
    """The text of every attribute (`#[…]` and `#![…]`, brackets balanced) in comment- and literal-free code."""
    out = []
    for m in re.finditer(r'#\s*!?\s*\[', code):
        depth, i = 0, m.end() - 1
        while i < len(code):
            depth += {'[': 1, ']': -1}.get(code[i], 0)
            if depth == 0:
                break
            i += 1
        out.append(' '.join(code[m.start():i + 1].split()))
    return out


def load_metadata(root: Path, path: Path | None) -> dict:
    if path:
        return json.loads(path.read_text(encoding='utf-8'))
    r = subprocess.run(['cargo', 'metadata', '--format-version', '1', '--locked'], cwd=root,
                       capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError(f'cargo metadata failed: {r.stderr.strip()[-2000:]}')
    return json.loads(r.stdout)


def dependency_checks(metadata: dict, hits: list) -> list[dict]:
    """Check 1; returns the allowed dev-dependency uses."""
    packages = {p['id']: p for p in metadata['packages']}
    nodes = {n['id']: n for n in metadata['resolve']['nodes']}
    members = metadata['workspace_members']
    core_ids = {m for m in members if packages[m]['name'] == CORE}
    dev_uses = []

    def edges(node_id: str, include_dev: bool):
        for dep in nodes[node_id]['deps']:
            kinds = {k['kind'] for k in dep['dep_kinds']}
            normal = bool(kinds & {None, 'build'})
            if normal or (include_dev and 'dev' in kinds):
                yield dep, normal
    for core in core_ids:
        for dep, _ in edges(core, False):
            if sqlite_package(packages[dep['pkg']]['name']) and any(k['kind'] == 'build' for k in dep['dep_kinds']):
                hits.append({'path': packages[core]['manifest_path'], 'kind': 'build-dependency',
                             'match': f"{CORE} build -> {packages[dep['pkg']]['name']}"})
    for node in nodes.values():
        if packages[node['id']]['name'] == 'rusqlite':
            extra = sorted(set(node.get('features') or []) - KNOWN_RUSQLITE_FEATURES)
            if extra:
                hits.append({'path': 'Cargo.lock', 'kind': 'rusqlite-feature',
                             'match': f"features {extra} may add write APIs: extend clippy.toml and this scan"})
    for member in members:
        if member in core_ids:
            continue
        name = packages[member]['name']
        for dep, normal in edges(member, True):
            if not normal and sqlite_package(packages[dep['pkg']]['name']):
                dev_uses.append({'member': name, 'package': packages[dep['pkg']]['name']})
        seen = set()
        stack = [(dep['pkg'], [packages[dep['pkg']]['name']]) for dep, _ in edges(member, False)]
        while stack:
            pkg, via = stack.pop()
            # Other workspace members are checked on their own; hoh-core is the allowed provider.
            if pkg in seen or pkg in members:
                continue
            seen.add(pkg)
            if sqlite_package(packages[pkg]['name']):
                hits.append({'path': packages[member]['manifest_path'], 'kind': 'dependency',
                             'match': ' -> '.join([name] + via)})
                continue
            stack.extend((d['pkg'], via + [packages[d['pkg']]['name']]) for d, _ in edges(pkg, False))
    return dev_uses


def member_dirs(metadata: dict) -> list[Path]:
    packages = {p['id']: p for p in metadata['packages']}
    return sorted({Path(packages[m]['manifest_path']).parent for m in metadata['workspace_members']})


def rust_files(metadata: dict) -> list[Path]:
    """Every .rs file in the member directories but the build output (`cargo metadata`'s target directory). Which of
    them are compiled into the product depends on targets and module paths, so the lint checks look at all of them
    (M2T01R-25, -29)."""
    build = Path(metadata.get('target_directory') or '/nonexistent').resolve()
    files = set()
    for d in member_dirs(metadata):
        files |= {p for p in d.rglob('*.rs') if not p.resolve().is_relative_to(build)}
    return sorted(files)


FORBID = '#![forbid(clippy::disallowed_methods)]'
FORBID_UNSAFE = '#![forbid(unsafe_code)]'
CORE_LIB_ITEM = re.compile(r'(?P<attrs>(?:#\[[^\]]*\]\s*)*)(?:pub\s+)?mod\s+(?P<name>\w+)\s*;')
CORE_VERSION = re.compile(r'pub\s+const\s+VERSION\s*:\s*&\s*str\s*=\s*env!\s*\(\s*\)\s*;')


def forbid_checks(root: Path, metadata: dict, hits: list) -> None:
    """The compiler-enforced part of check 3 (M2T01R-27, -28, -29): a lint level of `forbid` makes any later `allow`
    of the lint a hard error (E0453), whether written, raw-identifier-spelled, behind `cfg_attr` or generated by a
    macro. Every target root of every workspace member (from `cargo metadata`, wherever it lies) starts with
    `#![forbid(clippy::disallowed_methods)]`, except hoh-core's library root, which holds only module declarations —
    each but `persistence` carrying `#[forbid(clippy::disallowed_methods)]` — and the VERSION constant."""
    members = set(metadata['workspace_members'])
    for package in metadata['packages']:
        if package['id'] not in members:
            continue
        for target in package.get('targets') or []:
            src = Path(target['src_path'])
            rel = rel_to(src, root)
            try:
                code, _ = split_rust(src.read_text(encoding='utf-8', errors='replace'))
            except OSError as exc:
                hits.append({'path': rel, 'kind': 'target-root', 'match': f'unreadable: {exc}'})
                continue
            kinds = set(target.get('kind') or [])
            if 'custom-build' in kinds:
                # A build script can give the crate's compilation its own clippy configuration
                # (`cargo:rustc-env=CLIPPY_CONF_DIR=…`); the workspace has none (M2T01R-33).
                hits.append({'path': rel, 'kind': 'build-script', 'match': f"{package['name']} build script"})
                continue
            leading = leading_inner_attributes(code)
            if FORBID_UNSAFE not in leading:
                # Without it SQLite's C interface is reachable through unsafe code (M2T01R-34).
                hits.append({'path': rel, 'kind': 'missing-forbid', 'match': f"{package['name']} {'/'.join(sorted(kinds))} root: unsafe_code"})
            if package['name'] == CORE and 'lib' in kinds:
                rest = code
                for m in CORE_LIB_ITEM.finditer(code):
                    attrs = re.sub(r'\s+', '', m.group('attrs'))
                    if m.group('name') != 'persistence' and '#[forbid(clippy::disallowed_methods)]' not in attrs:
                        hits.append({'path': rel, 'kind': 'missing-forbid', 'match': f"mod {m.group('name')}"})
                    rest = rest.replace(m.group(0), ' ', 1)
                rest = CORE_VERSION.sub(' ', rest)
                rest = re.sub(r'#\s*!\s*\[[^\]]*\]', ' ', rest, count=len(leading))
                if rest.strip():
                    hits.append({'path': rel, 'kind': 'core-root', 'match': ' '.join(rest.split())[:200]})
            elif FORBID not in leading:
                hits.append({'path': rel, 'kind': 'missing-forbid', 'match': f"{package['name']} {'/'.join(sorted(kinds))} root"})


def leading_inner_attributes(code: str) -> list[str]:
    """The inner attributes at the very start of a root file (comments already blanked), normalised: only these apply
    to the whole crate; one inside an inline module applies to that module alone (M2T01R-31)."""
    out, i = [], 0
    while True:
        m = re.match(r'\s*#\s*!\s*\[', code[i:])
        if not m:
            return out
        start, depth, j = i + m.end() - 1, 0, i + m.end() - 1
        while j < len(code):
            depth += {'[': 1, ']': -1}.get(code[j], 0)
            if depth == 0:
                break
            j += 1
        out.append(re.sub(r'\br#', '', re.sub(r'\s+', '', code[i:j + 1])))
        i = j + 1


def is_test_code(path: Path, metadata: dict) -> bool:
    """Under tests/, benches/ or examples/ of a member (only for the SQL-literal check)."""
    return any(path.is_relative_to(d) and {'tests', 'benches', 'examples'} & set(path.relative_to(d).parts[:-1])
               for d in member_dirs(metadata))


def lint_configuration_checks(root: Path, metadata: dict, hits: list) -> None:
    """Check 2 outside the sources: clippy.toml, other clippy configs, Cargo lint tables, rustflags."""
    config = root / 'clippy.toml'
    try:
        entries = tomllib.loads(config.read_text(encoding='utf-8')).get('disallowed-methods', [])
    except (OSError, tomllib.TOMLDecodeError) as exc:
        hits.append({'path': 'clippy.toml', 'kind': 'clippy-config', 'match': f'unreadable: {exc}'})
        entries = []
    listed = {e if isinstance(e, str) else e.get('path') for e in entries}
    for missing in sorted(REQUIRED - listed):
        hits.append({'path': 'clippy.toml', 'kind': 'clippy-config', 'match': f'{missing} not disallowed'})
    for e in entries:
        if isinstance(e, dict) and e.get('allow-invalid'):
            hits.append({'path': 'clippy.toml', 'kind': 'clippy-config', 'match': f"{e.get('path')}: allow-invalid"})
    dirs = member_dirs(metadata)
    places = {root} | set(dirs) | {p for d in dirs for p in d.parents if p.is_relative_to(root)}
    for d in sorted(places):
        for name in ('clippy.toml', '.clippy.toml'):
            p = d / name
            if p.is_file() and p != config:
                hits.append({'path': rel_to(p, root), 'kind': 'clippy-config', 'match': 'another clippy configuration'})
    for manifest in sorted({root / 'Cargo.toml'} | {d / 'Cargo.toml' for d in dirs}):
        try:
            data = tomllib.loads(manifest.read_text(encoding='utf-8'))
        except (OSError, tomllib.TOMLDecodeError):
            continue
        for table in (data.get('lints') or {}, (data.get('workspace') or {}).get('lints') or {}):
            for tool, lints in table.items():
                for lint, level in (lints.items() if isinstance(lints, dict) else []):
                    level = level.get('level') if isinstance(level, dict) else level
                    full = (lint if tool == 'rust' else f'{tool}::{lint}').replace('-', '_')
                    if level in ('allow', 'expect') and SUPPRESSING.fullmatch(full):
                        hits.append({'path': rel_to(manifest, root), 'kind': 'lint-table', 'match': f'{full} = {level}'})
    for cfg in cargo_configs(root):
        for key in forbidden_cargo_keys(cfg):
            hits.append({'path': rel_to(cfg, root), 'kind': 'cargo-config', 'match': key})


def cargo_configs(root: Path) -> list[Path]:
    """Cargo configuration files that apply when cargo runs in the repository: the repository's and every ancestor's
    `.cargo/config(.toml)`, and `$CARGO_HOME/config(.toml)` (M2T01R-30)."""
    import os
    homes = [Path(os.environ.get('CARGO_HOME') or Path.home() / '.cargo')]
    dirs = [d / '.cargo' for d in [root, *root.parents]] + homes
    return [d / name for d in dirs for name in ('config.toml', 'config') if (d / name).is_file()]


def forbidden_cargo_keys(cfg: Path) -> list[str]:
    """Settings that can change how the static check compiles or lints: rustflags, compiler and wrappers, environment
    for the compiler, and aliases for the check's commands."""
    try:
        data = tomllib.loads(cfg.read_text(encoding='utf-8'))
    except (OSError, tomllib.TOMLDecodeError) as exc:
        return [f'unreadable: {exc}']
    found = []
    build = data.get('build') or {}
    found += [f'build.{k}' for k in ('rustflags', 'rustc', 'rustc-wrapper', 'rustc-workspace-wrapper', 'rustdocflags') if k in build]
    found += [f'target.{name}.rustflags' for name, t in (data.get('target') or {}).items() if isinstance(t, dict) and 'rustflags' in t]
    found += ['env' for _ in [1] if data.get('env')]
    found += [f'alias.{a}' for a in (data.get('alias') or {}) if a in ('clippy', 'check', 'build', 'metadata', 'deny', 'fmt')]
    found += ['unstable' for _ in [1] if data.get('unstable')]
    return found


def rel_to(path: Path, root: Path) -> str:
    return path.relative_to(root).as_posix() if path.is_relative_to(root) else path.as_posix()


def scan(root: Path, metadata: dict) -> dict:
    hits = []
    dev_uses = dependency_checks(metadata, hits)
    lint_configuration_checks(root, metadata, hits)
    files = rust_files(metadata)
    forbid_checks(root, metadata, hits)
    for path in files:
        rel = rel_to(path, root)
        code, literals = split_rust(path.read_text(encoding='utf-8', errors='replace'))
        # Whitespace removed and raw identifiers read as plain ones (M2T01R-23, -27).
        compact = [re.sub(r'\br#', '', re.sub(r'\s+', '', attr)) for attr in attributes(code)]
        if any(re.search(r'[\[(,]path=', attr) for attr in compact) or re.search(r'\binclude\s*!\s*[(\[{]', code):
            hits.append({'path': rel, 'kind': 'path-or-include', 'match': '#[path] (also in cfg_attr) or include!'})
        if rel.startswith(PERSISTENCE):
            continue
        for attr in compact:
            if re.search(r'(?:allow|expect)\(', attr) and SUPPRESSING.search(attr):
                hits.append({'path': rel, 'kind': 'lint-suppressed', 'match': attr[:200]})

        if is_test_code(path, metadata):
            continue
        for value in literal_strings(code, literals):
            if m := SQL_WRITE.search(value):
                hits.append({'path': rel, 'kind': 'sql', 'match': ' '.join(m.group(0).split())})
    web = root / 'web'
    web_files = 0
    if web.is_dir():
        for path in sorted(p for p in web.rglob('*') if p.is_file() and 'node_modules' not in p.parts):
            rel = rel_to(path, root)
            web_files += 1
            text = path.read_text(encoding='utf-8', errors='replace')
            if path.name == 'package.json' or path.name in LOCKFILES:
                try:
                    data = json.loads(text)
                except ValueError:
                    hits.append({'path': rel, 'kind': 'web-manifest', 'match': 'not JSON'})
                    continue
                if path.name in LOCKFILES:
                    found = [s for s in json_strings(data) if sqlite_package(s) or s.endswith('sql.js')]
                else:
                    specs = {}
                    for section in ('dependencies', 'devDependencies', 'optionalDependencies', 'peerDependencies'):
                        specs.update(data.get(section) or {})
                    found = [f'{d} {s}'.strip() for d, s in specs.items()
                             if sqlite_package(d) or (isinstance(s, str) and sqlite_package(s)) or d.endswith('sql.js')]
                hits.extend({'path': rel, 'kind': 'web-dependency', 'match': s[:200]} for s in found)
            elif re.search(r'''(?:\bfrom|\bimport|require\()\s*\(?\s*['"][^'"]*sqlite''', text, re.IGNORECASE):
                hits.append({'path': rel, 'kind': 'web-import', 'match': 'sqlite'})
    scanned = len(files) + web_files
    return {'files_scanned': scanned, 'web_files_scanned': web_files, 'web_present': web.is_dir(),
            'dev_dependency_uses': dev_uses, 'hits': hits, 'status': 'PASS' if scanned > 0 and not hits else 'FAIL'}


CANARY = 'crates/hoh-core/src/clippy_canary.rs'


def canary_verdict(log: str) -> dict:
    """Clippy on the canary must refuse every REQUIRED method there, and fail for no other reason."""
    refused, other = set(), []
    lines = log.splitlines()
    for i, line in enumerate(lines):
        if m := re.match(r'error: use of a disallowed method `([^`]+)`', line):
            where = next((l for l in lines[i + 1:i + 4] if '-->' in l), '')
            if CANARY in where:
                refused.add(m.group(1))
            else:
                other.append(f'{m.group(1)} reported at {where.strip()}')
        elif line.startswith('error') and not line.startswith('error: could not compile'):
            other.append(line[:200])
    problems = [f'not refused: {p}' for p in sorted(CANARY_REQUIRED - refused)] + other
    return {'refused': len(refused), 'required': len(CANARY_REQUIRED), 'problems': problems}


def json_strings(value) -> list[str]:
    """Every key and string value in a JSON document."""
    if isinstance(value, dict):
        return [s for k, v in value.items() for s in [k, *json_strings(v)]]
    if isinstance(value, list):
        return [s for v in value for s in json_strings(v)]
    return [value] if isinstance(value, str) else []


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[2])
    p.add_argument('--metadata', type=Path, help='cargo metadata JSON to use instead of running cargo')
    p.add_argument('--out', type=Path, help='test-results.jsonl to append the TEST-062-05 line to')
    p.add_argument('--canary-log', type=Path, help='output of clippy on the canary (scripts/product/static.sh)')
    a = p.parse_args(argv)
    try:
        result = scan(a.root.resolve(), load_metadata(a.root, a.metadata))
        if a.canary_log:
            result['canary'] = canary_verdict(a.canary_log.read_text(encoding='utf-8', errors='replace'))
            if result['canary']['problems']:
                result['status'] = 'FAIL'
    except (RuntimeError, KeyError, ValueError, OSError) as exc:
        result = {'files_scanned': 0, 'hits': [], 'status': 'FAIL', 'error': str(exc)}
    print(json.dumps(result, ensure_ascii=False, indent=1))
    if a.out:
        canary = result.get('canary')
        detail = result.get('error') or (f"{result['files_scanned']} files scanned, {len(result['hits'])} hits; " + (
            f"canary: {canary['refused']}/{canary['required']} disallowed calls refused"
            + (f", problems: {canary['problems'][:5]}" if canary['problems'] else '') if canary
            else 'canary not run'))
        line = {'schema_version': '1.0', 'test_id': 'TEST-062-05', 'name': 'scripts/product/persistence_scan.py',
                'status': result['status'], 'detail': detail[:4096]}
        with a.out.open('a', encoding='utf-8') as f:
            f.write(json.dumps(line, ensure_ascii=False) + '\n')
    return 0 if result['status'] == 'PASS' else 1


if __name__ == '__main__':
    sys.exit(main())
