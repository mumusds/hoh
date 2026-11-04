#!/usr/bin/env python3
"""AC-062-05: SQL writes and SQLite access only in the Persistence module (crates/hoh-core/src/persistence/).

Product sources are every workspace member's src/**/*.rs and build script (tests, benches and examples are not
product code). The checks (M2T01R-03/-04/-06/-07):
1. Dependencies, by package identity from `cargo metadata` (renamed packages and workspace inheritance resolved):
   no workspace member but hoh-core reaches a SQLite package (any package whose name contains "sqlite", such as
   rusqlite and libsqlite3-sys) through normal or build dependencies, other than through hoh-core itself; hoh-core
   has it only as a normal dependency (so its build script cannot use it). Dev-dependencies (test code) are allowed
   and listed.
2. SQLite names: the crate names under which hoh-core sees SQLite packages, closed over the aliases hoh-core's
   sources define for them (`extern crate X as Y`, `use ... as Y` in a use naming one, `type Y = ...` naming one).
   They occur only in persistence/ — in code (comments and string literals do not count) in any product source,
   and also in string literals of proc-macro crates (which can emit code).
3. No public item of persistence/ (plain `pub`, not `pub(crate)`/`pub(super)`/`pub(in ..)`) mentions a SQLite name
   or a connection type, so no other code can obtain a SQLite connection.
4. Defence in depth: string literals outside persistence/ that hold a SQL write statement (INSERT/REPLACE INTO,
   UPDATE ... SET, DELETE FROM, UPSERT, ON CONFLICT ... DO UPDATE), decoded as Rust does (escapes, line
   continuations), with literals separated only by commas joined (concat!), are hits.
5. web/ (when present): no SQLite package in package.json dependencies (by name or npm: alias spec), nowhere in a
   lockfile (every key and string value is looked at: package paths, names, nested dependencies), and no import of
   one.
This is a check of source text against ordinary and accidental violations; identifiers assembled at compile time
(e.g. from strings in a build script or by `include!` of non-Rust files) are beyond it and left to review.
Scanning no file at all fails (no empty scan). Prints a JSON summary; with --out appends one test-results.jsonl
line for TEST-062-05. --metadata reads `cargo metadata` JSON from a file (tests).
"""
from __future__ import annotations
import argparse, json, re, subprocess, sys
from pathlib import Path

PERSISTENCE = 'crates/hoh-core/src/persistence/'
CORE = 'hoh-core'
SQL_WRITE = re.compile(
    r'\b(?:INSERT\s+(?:OR\s+\w+\s+)?INTO|REPLACE\s+INTO|UPDATE\s+(?:OR\s+\w+\s+)?\S+\s+SET|DELETE\s+FROM|UPSERT'
    r'|ON\s+CONFLICT\b[^;]*?\bDO\s+UPDATE)\b', re.IGNORECASE)
CONNECTION_TYPES = re.compile(r'\b(?:Connection|Transaction|Savepoint|Statement|CachedStatement|Batch|Rows?)\b')
ESCAPE = {'n': '\n', 't': '\t', 'r': '\r', '0': '\0', '\\': '\\', '"': '"', "'": "'"}


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


def public_items(code: str) -> list[str]:
    """Signatures of plainly public items (from `pub` to the first `{` or `;`) and fields (to the end of the line)."""
    sigs = []
    for m in re.finditer(r'(?m)^[ \t]*pub\b(?![ \t]*\()', code):
        rest = code[m.start():]
        cut = min([p for p in (rest.find('{'), rest.find(';')) if p >= 0] or [len(rest)])
        sig = rest[:cut]
        if re.match(r'[ \t]*pub[ \t]+\w+[ \t]*:', sig):
            sig = sig.split('\n', 1)[0]
        sigs.append(' '.join(sig.split()))
    return sigs


def load_metadata(root: Path, path: Path | None) -> dict:
    if path:
        return json.loads(path.read_text(encoding='utf-8'))
    r = subprocess.run(['cargo', 'metadata', '--format-version', '1', '--locked'], cwd=root,
                       capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError(f'cargo metadata failed: {r.stderr.strip()[-2000:]}')
    return json.loads(r.stdout)


def dependency_checks(metadata: dict, hits: list) -> tuple[set[str], list[dict]]:
    """Check 1; returns hoh-core's crate names for SQLite packages and the allowed dev-dependency uses."""
    packages = {p['id']: p for p in metadata['packages']}
    nodes = {n['id']: n for n in metadata['resolve']['nodes']}
    members = metadata['workspace_members']
    core_ids = {m for m in members if packages[m]['name'] == CORE}
    core_names, dev_uses = set(), []

    def edges(node_id: str, include_dev: bool):
        for dep in nodes[node_id]['deps']:
            kinds = {k['kind'] for k in dep['dep_kinds']}
            normal = bool(kinds & {None, 'build'})
            if normal or (include_dev and 'dev' in kinds):
                yield dep, normal
    for core in core_ids:
        for dep, _ in edges(core, False):
            if sqlite_package(packages[dep['pkg']]['name']):
                core_names.add(dep['name'])
                if any(k['kind'] == 'build' for k in dep['dep_kinds']):
                    hits.append({'path': packages[core]['manifest_path'], 'kind': 'build-dependency',
                                 'match': f"{CORE} build -> {packages[dep['pkg']]['name']}"})
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
    return core_names, dev_uses


def product_files(root: Path, metadata: dict) -> tuple[list[Path], set[Path]]:
    """(every product source, the files of proc-macro crates)."""
    files = set(root.glob('crates/*/src/**/*.rs')) | set(root.glob('crates/*/build.rs'))
    macro_dirs = []
    for package in metadata.get('packages', []):
        for target in package.get('targets') or []:
            src = Path(target.get('src_path', ''))
            kinds = set(target.get('kind') or [])
            if 'custom-build' in kinds and src.is_file() and src.resolve().is_relative_to(root.resolve()):
                files.add(src)
            if 'proc-macro' in kinds:
                macro_dirs.append(src.parent.resolve())
    macro_files = {f for f in files if any(f.resolve().is_relative_to(d) for d in macro_dirs)}
    return sorted(files), macro_files


def alias_closure(names: set[str], sources: list[str]) -> set[str]:
    """`names` plus every alias the sources define for one of them."""
    names = set(names)
    while True:
        token = re.compile(r'\b(?:' + '|'.join(map(re.escape, sorted(names))) + r')\b')
        found = set()
        for code in sources:
            for m in re.finditer(r'\bextern\s+crate\s+(\w+)\s+as\s+(\w+)', code):
                if m.group(1) in names:
                    found.add(m.group(2))
            for m in re.finditer(r'\buse\s+([^;]*);', code):
                if token.search(m.group(1)):
                    found.update(re.findall(r'\bas\s+(\w+)', m.group(1)))
            for m in re.finditer(r'\btype\s+(\w+)\b[^=;]*=\s*([^;]*);', code):
                if token.search(m.group(2)):
                    found.add(m.group(1))
        found.discard('_')
        if found <= names:
            return names
        names |= found


def scan(root: Path, metadata: dict) -> dict:
    hits, scanned = [], 0
    core_names, dev_uses = dependency_checks(metadata, hits)
    files, macro_files = product_files(root, metadata)
    parsed = {path: split_rust(path.read_text(encoding='utf-8', errors='replace')) for path in files}
    core_sources = [code for path, (code, _) in parsed.items() if path.relative_to(root).as_posix().startswith('crates/hoh-core/')]
    names = alias_closure(core_names, core_sources) if core_names else set()
    token = re.compile(r'\b(?:' + '|'.join(map(re.escape, sorted(names))) + r')\b') if names else None
    for path in files:
        rel = path.relative_to(root).as_posix()
        scanned += 1
        code, literals = parsed[path]
        if rel.startswith(PERSISTENCE):
            for sig in public_items(code):
                if (token and token.search(sig)) or CONNECTION_TYPES.search(sig):
                    hits.append({'path': rel, 'kind': 'public-api', 'match': sig[:200]})
            continue
        if token and (m := token.search(code)):
            hits.append({'path': rel, 'kind': 'sqlite-name', 'match': m.group(0)})
        if token and path in macro_files:
            for _, _, value in literals:
                if m := token.search(value):
                    hits.append({'path': rel, 'kind': 'sqlite-name-in-macro', 'match': m.group(0)})
        for value in literal_strings(code, literals):
            if m := SQL_WRITE.search(value):
                hits.append({'path': rel, 'kind': 'sql', 'match': ' '.join(m.group(0).split())})
    web = root / 'web'
    web_files = 0
    if web.is_dir():
        for path in sorted(p for p in web.rglob('*') if p.is_file() and 'node_modules' not in p.parts):
            rel = path.relative_to(root).as_posix()
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
    scanned += web_files
    return {'files_scanned': scanned, 'web_files_scanned': web_files, 'web_present': web.is_dir(),
            'sqlite_names': sorted(names), 'dev_dependency_uses': dev_uses, 'hits': hits,
            'status': 'PASS' if scanned > 0 and not hits else 'FAIL'}


LOCKFILES = ('package-lock.json', 'npm-shrinkwrap.json')


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
    a = p.parse_args(argv)
    try:
        result = scan(a.root, load_metadata(a.root, a.metadata))
    except (RuntimeError, KeyError, ValueError, OSError) as exc:
        result = {'files_scanned': 0, 'hits': [], 'status': 'FAIL', 'error': str(exc)}
    print(json.dumps(result, ensure_ascii=False, indent=1))
    if a.out:
        detail = result.get('error') or (f"{result['files_scanned']} files scanned, {len(result['hits'])} hits "
                                         f"outside {PERSISTENCE}")
        line = {'schema_version': '1.0', 'test_id': 'TEST-062-05', 'name': 'scripts/product/persistence_scan.py',
                'status': result['status'], 'detail': detail[:4096]}
        with a.out.open('a', encoding='utf-8') as f:
            f.write(json.dumps(line, ensure_ascii=False) + '\n')
    return 0 if result['status'] == 'PASS' else 1


if __name__ == '__main__':
    sys.exit(main())
