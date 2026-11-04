#!/usr/bin/env python3
"""AC-062-05: SQL writes and SQLite access only in the Persistence module (crates/hoh-core/src/persistence/).

The boundary is Rust's module visibility; this scan checks that nothing opens it (M2T01R-03/-04/-06/-07/-13/-14).
Product sources are all .rs files of the workspace members' directories except tests/, benches/ and examples/.
1. Dependencies, by package identity from `cargo metadata` (renamed packages and workspace inheritance resolved):
   no workspace member but hoh-core reaches a SQLite package (any package whose name contains "sqlite", such as
   rusqlite and libsqlite3-sys) through normal or build dependencies, other than through hoh-core itself; hoh-core
   has it only as a normal dependency (so its build script cannot use it). Dev-dependencies (test code) are allowed
   and listed.
2. The crate names under which hoh-core sees SQLite packages, and the aliases hoh-core defines for them
   (`extern crate X as Y`, `use X as Y`, `type Y = X…`), occur only in persistence/: in the code of every product
   source (comments and string literals do not count) and also in the string literals of proc-macro crates.
3. Nothing in persistence/ that is visible outside it mentions a SQLite name: items and fields declared `pub`,
   `pub(crate)`, `pub(in …)` outside persistence, or `pub(super)` in persistence/mod.rs. A file's SQLite names are
   the crate names, everything its `use` statements import from them, and every alias or re-export of one defined
   anywhere in persistence/. A `type` alias of a SQLite type (associated types of trait impls included), a trait
   impl whose header names one, and a `#[macro_export]` macro in persistence/ are hits too.
4. `#[path = …]` and `include!` are not used in product sources, so every compiled file is one this scan reads.
5. Defence in depth: string literals outside persistence/ that hold a SQL write statement (INSERT/REPLACE INTO,
   UPDATE ... SET, DELETE FROM, UPSERT, ON CONFLICT ... DO UPDATE), decoded as Rust does (escapes, line
   continuations), with literals separated only by commas joined (concat!), are hits.
6. web/ (when present): no SQLite package in package.json dependencies (by name or npm: alias spec), nowhere in a
   lockfile (every key and string value), and no import of one.
This checks source text against ordinary and accidental violations; identifiers assembled at compile time (e.g.
generated from strings by a build script) are beyond it and left to review.
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


VISIBLE = re.compile(r'(?m)^[ \t]*(pub(?:[ \t]*\((?:crate|super|self|in[ \t]+[\w:]+)\))?)[ \t]')


def leaks(vis: str, rel: str) -> bool:
    """Whether an item with visibility `vis` in persistence file `rel` is visible outside persistence/."""
    vis = ' '.join(vis.split()).replace('( ', '(').replace(' )', ')')
    if vis in ('pub', 'pub(crate)'):
        return True
    if vis == 'pub(super)':
        return rel == PERSISTENCE + 'mod.rs'
    if vis.startswith('pub(in'):
        return not re.match(r'pub\(in (?:crate::persistence|self|super)\b', vis)
    return False  # pub(self)


def visible_items(code: str, rel: str) -> list[str]:
    """Signatures of items and fields visible outside persistence/ (from the visibility to the first `{` or `;`; a
    field to the end of its line)."""
    sigs = []
    for m in VISIBLE.finditer(code):
        if not leaks(m.group(1), rel):
            continue
        rest = code[m.start():]
        cut = min([p for p in (rest.find('{'), rest.find(';')) if p >= 0] or [len(rest)])
        sig = rest[:cut]
        if re.match(r'[ \t]*pub[^:]*?[ \t]\w+[ \t]*:(?!:)', sig):
            sig = sig.split('\n', 1)[0]
        sigs.append(' '.join(sig.split()))
    return sigs


def imported_names(code: str, names: set[str]) -> set[str]:
    """Everything the `use` statements of a file import from (or through) one of `names`."""
    if not names:
        return set()
    token = re.compile(r'\b(?:' + '|'.join(map(re.escape, sorted(names))) + r')\b')
    out = set()
    for m in re.finditer(r'\buse\s+([^;]*);', code):
        if token.search(m.group(1)):
            out.update(w for w in re.findall(r'\b[A-Za-z_]\w*\b', m.group(1))
                       if w not in ('as', 'self', 'super', 'crate', '_'))
    return out


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
    skip = {'tests', 'benches', 'examples', 'target'}
    files = {p for p in root.glob('crates/*/**/*.rs') if not skip & set(p.relative_to(root).parts[2:-1])}
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
    rel_of = {path: path.relative_to(root).as_posix() for path in files}
    persistence = [p for p in files if rel_of[p].startswith(PERSISTENCE)]
    # Aliases and re-exports of SQLite items made anywhere in persistence/ count in all of it.
    shared = set(names)
    for path in persistence:
        code = parsed[path][0]
        local = names | imported_names(code, names)
        local_token = re.compile(r'\b(?:' + '|'.join(map(re.escape, sorted(local))) + r')\b') if local else None
        for m in re.finditer(r'\btype\s+(\w+)\b[^=;]*=\s*([^;]*);', code):
            if local_token and local_token.search(m.group(2)):
                shared.add(m.group(1))
                # Persistence needs no alias of a SQLite type; one (an associated type of a trait impl such as
                # `Deref::Target` included) can hand a connection to other code.
                hits.append({'path': rel_of[path], 'kind': 'sqlite-type-alias', 'match': ' '.join(m.group(0).split())[:200]})
        for m in re.finditer(r'(?m)^[ \t]*pub\b[^;\n]*?\buse\s+([^;]*);', code):
            if local_token and local_token.search(m.group(1)):
                shared.update(imported_names('use ' + m.group(1) + ';', local))
    for path in files:
        rel = rel_of[path]
        scanned += 1
        code, literals = parsed[path]
        if re.search(r'#\s*!?\[\s*path\s*=', code) or re.search(r'\binclude!\s*[(\[{]', code):
            hits.append({'path': rel, 'kind': 'path-or-include', 'match': '#[path] or include!'})
        if rel.startswith(PERSISTENCE):
            visible = shared | imported_names(code, shared)
            vis_token = re.compile(r'\b(?:' + '|'.join(map(re.escape, sorted(visible))) + r')\b') if visible else None
            for sig in visible_items(code, rel):
                if vis_token and vis_token.search(sig):
                    hits.append({'path': rel, 'kind': 'visible-api', 'match': sig[:200]})
            for m in re.finditer(r'(?m)^[ \t]*impl\b[^{;]*', code):
                if vis_token and vis_token.search(m.group(0)):
                    hits.append({'path': rel, 'kind': 'trait-impl', 'match': ' '.join(m.group(0).split())[:200]})
            if re.search(r'#\s*\[\s*macro_export\b', code):
                hits.append({'path': rel, 'kind': 'macro-export', 'match': '#[macro_export]'})
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
