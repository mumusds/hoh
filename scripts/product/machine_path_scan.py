#!/usr/bin/env python3
"""Machine-specific home path scan (DC-M4L1-001, M4L1-100, M4L1-101).

Refuses machine-specific `/home/<user>` paths (quoted or unquoted, with or without a trailing slash,
any username) and `tempdir_in` calls targeting `/home` in Rust, Python, shell, TOML, YAML, and JSON sources unless
explicitly allowlisted in `scripts/product/machine-path-allowlist.json`.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
ALLOWLIST = ROOT / "scripts" / "product" / "machine-path-allowlist.json"
SCAN_DIRS = ("crates", "tests", "tools", "scripts", ".github")
EXTENSIONS = frozenset({".rs", ".py", ".sh", ".toml", ".yml", ".yaml", ".json"})
SKIP_DIRS = frozenset({".git", "target", ".hoh-dev", "node_modules", "__pycache__"})

# Matches `/home/<user>` (with optional trailing subpath) regardless of quoting or trailing slash,
# as well as `tempdir_in` targeting `/home` even without a username segment.
HOME_PATH = re.compile(
    r'(?<![A-Za-z0-9_])/home/(?P<user>[A-Za-z0-9._-]+)(?P<rest>/[^\s"\'<>|;)\]},`\\]*)?'
    r'|tempdir_in\(\s*"' + r'/home(?:/[^"]*)?"?'
)


def iter_sources(root: Path, allowlist_path: Path | None) -> list[Path]:
    resolved_allow = allowlist_path.resolve() if allowlist_path is not None else None
    found: list[Path] = []
    # Top-level configuration sources (e.g. Cargo.toml, clippy.toml, deny.toml).
    if root.is_dir():
        for child in sorted(root.iterdir()):
            if child.is_file() and child.suffix in EXTENSIONS:
                if resolved_allow is None or child.resolve() != resolved_allow:
                    found.append(child)
    for rel_dir in SCAN_DIRS:
        base = root / rel_dir
        if not base.is_dir():
            continue
        for path in sorted(base.rglob("*")):
            if not path.is_file() or path.suffix not in EXTENSIONS:
                continue
            if any(part in SKIP_DIRS for part in path.relative_to(root).parts):
                continue
            if resolved_allow is not None and path.resolve() == resolved_allow:
                continue
            found.append(path)
    return found


def load_allowlist(path: Path) -> tuple[dict[str, dict], dict[str, dict], list[str]]:
    problems: list[str] = []
    data = json.loads(path.read_text(encoding="utf-8"))
    files = data.get("files", {})
    directories = data.get("directories", {})
    if not isinstance(files, dict) or not isinstance(directories, dict):
        return {}, {}, [f"{path}: allowlist must have object 'files' and 'directories' fields"]
    for rel, rule in files.items():
        if not isinstance(rule, dict) or not str(rule.get("reason", "")).strip():
            problems.append(f"{rel}: allowlist file entry missing non-empty reason")
        elif not rule.get("allowed_literals") and not rule.get("allowed_users"):
            problems.append(f"{rel}: allowlist file entry must name allowed_literals or allowed_users")
    for rel, rule in directories.items():
        if not rel.endswith("/"):
            problems.append(f"{rel}: allowlist directory entry must end with '/'")
        if not isinstance(rule, dict) or not str(rule.get("reason", "")).strip():
            problems.append(f"{rel}: allowlist directory entry missing non-empty reason")
    return files, directories, problems


def match_allowed(rel: str, match: re.Match[str], files: dict[str, dict], directories: dict[str, dict],
                  used_files: set[str], used_dirs: set[str]) -> bool:
    for prefix in directories:
        if rel.startswith(prefix):
            used_dirs.add(prefix)
            return True
    rule = files.get(rel)
    if rule is None:
        return False
    literal = match.group(0)
    user = match.group("user")
    if literal in rule.get("allowed_literals", ()):
        used_files.add(rel)
        return True
    if user is not None and user in rule.get("allowed_users", ()):
        used_files.add(rel)
        return True
    return False


def scan(root: Path, allowlist_path: Path | None) -> list[str]:
    files_allow: dict[str, dict] = {}
    dirs_allow: dict[str, dict] = {}
    problems: list[str] = []
    if allowlist_path is not None:
        files_allow, dirs_allow, allow_problems = load_allowlist(allowlist_path)
        problems.extend(allow_problems)
    used_files: set[str] = set()
    used_dirs: set[str] = set()
    for path in iter_sources(root, allowlist_path):
        rel = path.relative_to(root).as_posix()
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError as exc:
            problems.append(f"{rel}: cannot read source: {exc}")
            continue
        for lineno, line in enumerate(text.splitlines(), 1):
            for match in HOME_PATH.finditer(line):
                if not match_allowed(rel, match, files_allow, dirs_allow, used_files, used_dirs):
                    problems.append(
                        f"{rel}:{lineno}: machine-specific home path {match.group(0)!r}; use tempfile defaults or TMPDIR"
                    )
    for rel in sorted(set(files_allow) - used_files):
        problems.append(f"{rel}: stale allowlist file entry (no matching path found)")
    for prefix in sorted(set(dirs_allow) - used_dirs):
        problems.append(f"{prefix}: stale allowlist directory entry (no matching path found)")
    return problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--allowlist", type=Path, default=ALLOWLIST)
    parser.add_argument("--no-allowlist", action="store_true")
    args = parser.parse_args(argv)
    allowlist = None if args.no_allowlist else args.allowlist
    problems = scan(args.root, allowlist)
    if problems:
        for problem in problems:
            print(f"machine-path-scan: {problem}", file=sys.stderr)
        return 1
    print("machine-path-scan: clean", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
