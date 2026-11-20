#!/usr/bin/env python3
"""Check that the committed excerpts keep the structure of the recordings they were cut from (ADR-032 design
correction 4).

Every excerpt (the S0 runs' `<agent>/*.excerpt.jsonl`, and every JSON-lines file of the TEST-021-07 recordings
under `<agent>-live/`) has a header that names its source file, the source's sha256, the source line index of every
following line and the sha256 of every line's key skeleton (keys, nesting, array lengths, value types). For every
excerpt this checks:

- the header lists exactly one index and one skeleton hash per line, and each line's skeleton hash is the recorded one;
- when the raw source is present locally and still has the recorded sha256, each excerpt line has the same skeleton
  as the source line it names (the raw recordings are local only: .hoh-dev/evidence/commander-readonly/spike/);
- each meta file's snapshot_diff has the skeleton of the raw snapshot's diff, when that is present.

A raw source that is present but whose sha256 differs is a failure: the excerpt no longer describes it.

Usage: python3 tests/fixtures/commander-readonly/excerpt_check.py [--excerpts DIR] [--root REPO] [--recorded-only]
Exit 0 when every check holds, 1 otherwise; one line per problem on stderr, a summary on stdout.
"""
from __future__ import annotations
import argparse, importlib.util, json, sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location('excerpt', HERE / 'excerpt.py')
excerpt = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(excerpt)


def check(excerpts: Path, root: Path, recorded_only: bool) -> tuple[list[str], dict]:
    problems: list[str] = []
    stats = {'files': 0, 'lines': 0, 'against_raw': 0, 'against_recorded_only': 0, 'meta_against_raw': 0}
    # The S0 excerpts, and the TEST-021-07 recordings in the judge's layout (excerpt.py --live, design correction 8).
    for path in sorted([*excerpts.glob('*/*.excerpt.jsonl'), *excerpts.glob('*-live/**/*.jsonl')]):
        stats['files'] += 1
        rel = path.relative_to(excerpts)
        text = path.read_text().splitlines()
        header = json.loads(text[0]).get('_excerpt') if text else None
        if not isinstance(header, dict):
            problems.append(f'{rel}: no _excerpt header')
            continue
        lines = [json.loads(t) for t in text[1:]]
        index, hashes = header.get('lines'), header.get('skeleton_sha256')
        if not isinstance(index, list) or not isinstance(hashes, list) or not len(index) == len(hashes) == len(lines):
            problems.append(f'{rel}: header lists {len(index or [])} indexes and {len(hashes or [])} skeletons for {len(lines)} lines')
            continue
        for n, (line, want) in enumerate(zip(lines, hashes), start=2):
            if excerpt.skeleton_sha256(line) != want:
                problems.append(f'{rel}:{n}: key skeleton differs from the recorded one')
        stats['lines'] += len(lines)
        source = root / header['source']
        if recorded_only or not source.is_file():
            stats['against_recorded_only'] += 1
            continue
        raw = source.read_bytes()
        if excerpt.sha256(raw) != header['source_sha256']:
            problems.append(f'{rel}: the local source {header["source"]} is not the recorded one (sha256 differs)')
            continue
        raw_lines = raw.splitlines()
        for n, (line, i) in enumerate(zip(lines, index), start=2):
            if not 0 <= i < len(raw_lines):
                problems.append(f'{rel}:{n}: source line {i} does not exist')
            elif excerpt.skeleton(json.loads(raw_lines[i])) != excerpt.skeleton(line):
                problems.append(f'{rel}:{n}: key skeleton differs from source line {i}')
        stats['against_raw'] += 1
    for meta_path in sorted(excerpts.glob('*/*.meta.json')):
        meta = json.loads(meta_path.read_text())
        source = root / meta.get('snapshot_source', '')
        if recorded_only or not meta.get('snapshot_source') or not source.is_file():
            continue
        raw = source.read_bytes()
        if excerpt.sha256(raw) != meta.get('snapshot_sha256'):
            problems.append(f'{meta_path.relative_to(excerpts)}: the local snapshot is not the recorded one')
        elif excerpt.skeleton(json.loads(raw)['diff']) != excerpt.skeleton(meta['snapshot_diff']):
            problems.append(f'{meta_path.relative_to(excerpts)}: snapshot_diff differs in structure from the source')
        else:
            stats['meta_against_raw'] += 1
    return problems, stats


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--excerpts', type=Path, default=HERE / 'excerpts')
    ap.add_argument('--root', type=Path, default=HERE.parents[2])
    ap.add_argument('--recorded-only', action='store_true', help='ignore local raw recordings; check recorded skeletons only')
    args = ap.parse_args()
    problems, stats = check(args.excerpts, args.root, args.recorded_only)
    for p in problems:
        print(p, file=sys.stderr)
    print(json.dumps({**stats, 'problems': len(problems)}))
    return 1 if problems else 0


if __name__ == '__main__':
    sys.exit(main())
