#!/usr/bin/env python3
"""Turn libtest output into per-test evidence lines (contracts/test-result.schema.json).

A test covers TEST-NNN-MM when its name contains a `test_NNN_MM` segment, e.g.
`cli::tests::test_004_01_exit_code_table` -> TEST-004-01. Tests without such a
segment are ordinary tests: they still run and count, but are not AC evidence.
Writes nothing but the output file; the exit code is 0 unless the log is unreadable.
"""
from __future__ import annotations
import argparse, json, re, sys
from pathlib import Path

LINE = re.compile(r'^test (?P<name>.+?) \.\.\. (?P<result>ok|FAILED|ignored)\b')
TEST_ID = re.compile(r'(?:^|[^0-9A-Za-z])test_(\d{3})_(\d{2})(?![0-9])')
STATUS = {'ok': 'PASS', 'FAILED': 'FAIL', 'ignored': 'SKIPPED'}

def results(log: str) -> list[dict]:
    out = []
    for line in log.splitlines():
        m = LINE.match(line.strip())
        if not m:
            continue
        name = m['name']
        for req, num in sorted(set(TEST_ID.findall(name))):
            out.append({'schema_version': '1.0', 'test_id': f'TEST-{req}-{num}', 'name': name, 'status': STATUS[m['result']]})
    return out

def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument('--log', type=Path, required=True)
    p.add_argument('--out', type=Path, required=True)
    a = p.parse_args(argv)
    try:
        lines = results(a.log.read_text(encoding='utf-8', errors='replace'))
    except OSError as exc:
        print(f'test_results: cannot read {a.log}: {exc}', file=sys.stderr)
        return 1
    with a.out.open('a', encoding='utf-8') as f:
        for item in lines:
            f.write(json.dumps(item, ensure_ascii=False) + '\n')
    print(f'test_results: {len(lines)} AC-mapped results -> {a.out}', file=sys.stderr)
    return 0

if __name__ == '__main__':
    sys.exit(main())
