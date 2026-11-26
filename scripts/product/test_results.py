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
STRICT_LINE = re.compile(r'^test (?P<name>.+?) \.\.\. (?P<result>ok|FAILED|ignored)(?P<tail>, .*)?$')
RUNNING = re.compile(r'^running \d+ tests?$')
RUNNING_LONG = re.compile(r'^test .+? has been running for over \d+ seconds$')
DOCTEST_MERGED = re.compile(
    r'^all doctests ran in \d+(?:\.\d+)?s; merged doctests compilation took \d+(?:\.\d+)?s$'
)
SUMMARY = re.compile(
    r'^test result: (?:ok|FAILED)\. (?P<passed>\d+) passed; (?P<failed>\d+) failed; '
    r'(?P<ignored>\d+) ignored; \d+ measured; \d+ filtered out(?:; finished in .*)?$'
)
SECTION_HDR = re.compile(r'^---- .+ std(?:out|err) ----$')
TEST_ID = re.compile(r'(?:^|[^0-9A-Za-z])test_(\d{3})_(\d{2})(?![0-9])')
STATUS = {'ok': 'PASS', 'FAILED': 'FAIL', 'ignored': 'SKIPPED'}


def check_stdout(log: str) -> list[str]:
    """M3R-1001: verify libtest stdout contains only recognized libtest lines and no leaked child fd 1 output."""
    problems: list[str] = []
    seen = {'ok': 0, 'FAILED': 0, 'ignored': 0}
    summarized = {'ok': 0, 'FAILED': 0, 'ignored': 0}
    in_captured = False
    in_failure_list = False
    for lineno, raw in enumerate(log.splitlines(), 1):
        line = raw.rstrip('\r')
        if SUMMARY.match(line):
            m = SUMMARY.match(line)
            assert m is not None
            summarized['ok'] += int(m['passed'])
            summarized['FAILED'] += int(m['failed'])
            summarized['ignored'] += int(m['ignored'])
            in_captured = False
            in_failure_list = False
            continue
        if line == 'failures:':
            in_captured = False
            in_failure_list = True
            continue
        if SECTION_HDR.match(line):
            in_captured = True
            in_failure_list = False
            continue
        if in_captured:
            continue
        if in_failure_list:
            if line == '' or line.startswith('    '):
                if line == '':
                    in_failure_list = False
                continue
            in_failure_list = False
        if line == '' or RUNNING.match(line) or RUNNING_LONG.match(line) or DOCTEST_MERGED.match(line):
            continue
        m = STRICT_LINE.match(line)
        if m and (m['result'] == 'ignored' or not m['tail']):
            seen[m['result']] += 1
            continue
        problems.append(
            f'line {lineno}: unexpected libtest stdout (possible leaked child fd 1 output): {line[:200]!r}'
        )
    if seen != summarized:
        problems.append(
            f'result-line count mismatch on libtest stdout (possible leaked child fd 1 output): '
            f'seen {seen} vs test result summaries {summarized}'
        )
    return problems


def results(log: str) -> list[dict]:
    out = []
    for line in log.splitlines():
        m = LINE.match(line.strip())
        if not m:
            continue
        name = m['name']
        for req, num in sorted(set(TEST_ID.findall(name))):
            row = {'schema_version': '1.0', 'test_id': f'TEST-{req}-{num}', 'name': name, 'status': STATUS[m['result']]}
            # libtest prints the #[ignore] reason after a comma; retain it so another check's deliberate exclusion
            # (notably TEST-005-06, executed by deployment) is explicit without changing its status or AC binding.
            tail = line.strip()[m.end():].strip()
            if m['result'] == 'ignored' and tail.startswith(',') and tail[1:].strip():
                row['detail'] = tail[1:].strip()[:4096]
            out.append(row)
    return out


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument('--log', type=Path, required=True)
    p.add_argument('--out', type=Path)
    p.add_argument('--check-stdout', action='store_true',
                   help='Fail if libtest stdout contains non-libtest lines or lost result lines (M3R-1001 fd-1 guard).')
    a = p.parse_args(argv)
    if a.out is None and not a.check_stdout:
        p.error('at least one of --out or --check-stdout is required')
    try:
        text = a.log.read_text(encoding='utf-8', errors='replace')
    except OSError as exc:
        print(f'test_results: cannot read {a.log}: {exc}', file=sys.stderr)
        return 1
    if a.out is not None:
        lines = results(text)
        with a.out.open('a', encoding='utf-8') as f:
            for item in lines:
                f.write(json.dumps(item, ensure_ascii=False) + '\n')
        print(f'test_results: {len(lines)} AC-mapped results -> {a.out}', file=sys.stderr)
    if a.check_stdout:
        problems = check_stdout(text)
        if problems:
            for problem in problems[:20]:
                print(f'test_results: {problem}', file=sys.stderr)
            if len(problems) > 20:
                print(f'test_results: ... and {len(problems) - 20} more stdout issues', file=sys.stderr)
            return 1
    return 0

if __name__ == '__main__':
    sys.exit(main())
