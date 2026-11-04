#!/usr/bin/env python3
"""AC-062-05: SQL writes and SQLite write calls only in the Persistence module.

Scans the product sources (crates/*/src/**/*.rs) and web/ (when present). A hit is
- a string literal holding a SQL write statement (INSERT/REPLACE INTO, UPDATE ... SET, DELETE FROM, UPSERT),
- a rusqlite write call (.execute( .execute_batch( .transaction( .transaction_with_behavior(
  .unchecked_transaction( .savepoint( .pragma_update() or any use of rusqlite,
- a rusqlite dependency of any crate but hoh-core, or an SQLite dependency of the web client,
outside the allowed path crates/hoh-core/src/persistence/. Comments and doc comments are ignored; only code and
string literals are looked at. Scanning no file at all fails (no empty scan).

Prints a JSON summary; with --out appends one test-results.jsonl line for TEST-062-05.
"""
from __future__ import annotations
import argparse, json, re, sys
from pathlib import Path

ALLOWED = ('crates/hoh-core/src/persistence/',)
SQL_WRITE = re.compile(
    r'\b(?:INSERT(?:\s+OR\s+\w+)?\s+INTO|REPLACE\s+INTO|UPDATE\s+(?:OR\s+\w+\s+)?[\w."`\[\]]+\s+SET|DELETE\s+FROM|UPSERT)\b',
    re.IGNORECASE)
WRITE_CALL = re.compile(
    r'\.(?:execute|execute_batch|transaction|transaction_with_behavior|unchecked_transaction|savepoint|pragma_update)\s*\(')


def split_rust(text: str) -> tuple[str, list[str]]:
    """(code with comments and literals blanked, string literal contents)."""
    code, literals, i, n = [], [], 0, len(text)
    while i < n:
        c = text[i]
        if text.startswith('//', i):
            j = text.find('\n', i)
            i = n if j < 0 else j
            continue
        if text.startswith('/*', i):
            depth, i = 1, i + 2
            while i < n and depth:
                if text.startswith('/*', i):
                    depth, i = depth + 1, i + 2
                elif text.startswith('*/', i):
                    depth, i = depth - 1, i + 2
                else:
                    i += 1
            continue
        raw = re.match(r'b?r(#*)"', text[i:])
        if raw and (i == 0 or not (text[i - 1].isalnum() or text[i - 1] == '_')):
            close = '"' + raw.group(1)
            start = i + raw.end()
            end = text.find(close, start)
            end = n if end < 0 else end
            literals.append(text[start:end])
            code.append(' ')
            i = end + len(close)
            continue
        if c == '"' or (c == 'b' and text.startswith('b"', i)):
            start = i + (2 if c == 'b' else 1)
            j, buf = start, []
            while j < n and text[j] != '"':
                if text[j] == '\\' and j + 1 < n:
                    if text[j + 1] == '\n':  # line continuation; read as a space so no statement is missed
                        j += 2
                        while j < n and text[j] in ' \t\n':
                            j += 1
                        buf.append(' ')
                        continue
                    buf.append(text[j:j + 2])
                    j += 2
                    continue
                buf.append(text[j])
                j += 1
            literals.append(''.join(buf))
            code.append(' ')
            i = j + 1
            continue
        if c == "'":
            # Char literal or lifetime: skip a char literal so a quote inside it does not open a string.
            m = re.match(r"'(?:\\.|[^\\'])'", text[i:])
            if m:
                code.append(' ')
                i += m.end()
                continue
        code.append(c)
        i += 1
    return ''.join(code), literals


def scan(root: Path) -> dict:
    hits, files = [], 0
    for path in sorted(root.glob('crates/*/src/**/*.rs')):
        rel = path.relative_to(root).as_posix()
        files += 1
        if rel.startswith(ALLOWED):
            continue
        code, literals = split_rust(path.read_text(encoding='utf-8', errors='replace'))
        for lit in literals:
            m = SQL_WRITE.search(lit)
            if m:
                hits.append({'path': rel, 'kind': 'sql', 'match': m.group(0)})
        for m in WRITE_CALL.finditer(code):
            hits.append({'path': rel, 'kind': 'call', 'match': m.group(0)})
        if re.search(r'\brusqlite\b', code):
            hits.append({'path': rel, 'kind': 'rusqlite', 'match': 'rusqlite'})
    for manifest in sorted(root.glob('crates/*/Cargo.toml')):
        rel = manifest.relative_to(root).as_posix()
        files += 1
        if rel != 'crates/hoh-core/Cargo.toml' and re.search(r'(?m)^\s*(rusqlite|libsqlite3-sys)\s*=', manifest.read_text(encoding='utf-8')):
            hits.append({'path': rel, 'kind': 'dependency', 'match': 'rusqlite'})
    web = root / 'web'
    web_files = 0
    if web.is_dir():
        for path in sorted(p for p in web.rglob('*') if p.is_file() and 'node_modules' not in p.parts):
            rel = path.relative_to(root).as_posix()
            web_files += 1
            text = path.read_text(encoding='utf-8', errors='replace')
            if path.name == 'package.json':
                try:
                    data = json.loads(text)
                except ValueError:
                    data = {}
                for section in ('dependencies', 'devDependencies', 'optionalDependencies', 'peerDependencies'):
                    for name in (data.get(section) or {}):
                        if 'sqlite' in name.lower() or name.lower() in ('sql.js', 'better-sqlite3'):
                            hits.append({'path': rel, 'kind': 'web-dependency', 'match': name})
            elif re.search(r'''(?:from|require\()\s*['"][^'"]*sqlite''', text):
                hits.append({'path': rel, 'kind': 'web-import', 'match': 'sqlite'})
    files += web_files
    return {'files_scanned': files, 'web_files_scanned': web_files, 'web_present': web.is_dir(), 'hits': hits,
            'status': 'PASS' if files > 0 and not hits else 'FAIL'}


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[2])
    p.add_argument('--out', type=Path, help='test-results.jsonl to append the TEST-062-05 line to')
    a = p.parse_args(argv)
    result = scan(a.root)
    print(json.dumps(result, ensure_ascii=False, indent=1))
    if a.out:
        detail = f"{result['files_scanned']} files scanned, {len(result['hits'])} hits outside {ALLOWED[0]}"
        line = {'schema_version': '1.0', 'test_id': 'TEST-062-05', 'name': 'scripts/product/persistence_scan.py',
                'status': result['status'], 'detail': detail[:4096]}
        with a.out.open('a', encoding='utf-8') as f:
            f.write(json.dumps(line, ensure_ascii=False) + '\n')
    return 0 if result['status'] == 'PASS' else 1


if __name__ == '__main__':
    sys.exit(main())
