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
   give its crate another clippy configuration through `cargo:rustc-env`), no target root lies outside the member
   directories (M2T01R-40) and no product root outside its member's src/ (M2T01R-46), no path dependency is outside
   the workspace (M2T01R-48), static.sh lints the dev and the release profile (`cfg(debug_assertions)`, M2T01R-39)
   and no other profile or `panic` setting exists (M2T01R-44), persistence/ defines no macros (M2T01R-49),
   the members declare no Cargo features (so
   the one build clippy checks is the build, M2T01R-32/-35), and no product source has a `clippy` cfg predicate
   (`cfg(not(clippy))` would compile code clippy never sees, M2T01R-36) or one taken from a macro argument. `forbid` makes the compiler
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
4. String literals outside persistence/ in non-test code (test code: files under a member's tests/, benches/ or
   examples/ directory where no product target root lies), the files such code embeds with `include_str!` or
   `include_bytes!` (which may not be imported under another name), and the repository files its literals name (a
   script it runs), may not mention sqlite at all — no SQLite command line or interpreter module run as a
   subprocess (M2T01R-38) — and, in depth, may not hold a SQL write statement (INSERT/REPLACE INTO,
   UPDATE ... SET, DELETE FROM, UPSERT, ON CONFLICT ... DO UPDATE), decoded as Rust does (escapes, line
   continuations), with literals separated only by commas joined (concat!), are hits.
5. web/ (when present): no SQLite package in package.json dependencies (by name or npm: alias spec), nowhere in a
   lockfile (every key and string value), and no import of one.
6. Every other file of the candidate (git: tracked, and untracked but not ignored), wherever it lies and however
   product code would reach it (a joined path, `python3 -m`, a driver importing it, an embedded driver; M2T01R-51):
   if it mentions SQLite or holds a SQL write, it must be named in the whitelist configuration
   scripts/product/persistence-whitelist.json (AC-062-05), each entry with its reason; Markdown under docs/ and
   reports/ is prose and needs no entry. The whitelist names only files that need it (existing, outside persistence,
   not a member's Rust source, still mentioning SQLite). No file of the candidate is a symbolic link (M2T01R-52), and
   `include_str!`/`include_bytes!` may not reach outside the repository. Cargo configurations follow the profile rule
   of check 3 (M2T01R-50) and may not move the build output (`build.target-dir`; only git's ignore rules decide
   which files are the candidate's, M2T01R-55), and static.sh clears CARGO_PROFILE_* from its environment. The
   members' Rust sources of checks 3 and 4 are likewise the candidate's files (M2T01R-59). Every whitelist entry
   gives its reason (M2T01R-58). The value of a doc attribute (`#[doc = …]`, what `///` writes, also in `cfg_attr`)
   that is not inside a macro invocation is documentation: its literals, the files it embeds and the files its text
   names are not checked as in check 4 (M2T01R-56, -60); inside a macro invocation it is input the macro may use at
   run time and is checked (M2T01R-61), as are doc comments there (M2T01R-63); macro names are Rust identifiers
   (raw ones, keywords included, and any XID identifier, M2T01R-64/-69; `r#macro_rules!(…)` is an invocation,
   M2T01R-71); a doc attribute a `macro_rules!` definition
   writes documents the generated item (M2T01R-65). The body of an inline module under exactly `#[cfg(test)]` that is
   not inside a macro invocation (its name any XID identifier, M2T01R-72) is test code for the literal, include and
   named-file checks, like tests/ (M2T01R-66, -68, -70); module files are never exempt that way (`#[cfg(test)] mod tests;` follows the product rules, M2T01R-67).
   A literal may name a file the whitelist lists under read_as_data with its reason (CR-019): a JSON or Markdown
   file the product reads as data whose prose mentions SQLite (the specification), holding no SQL write; a listed
   script, a listed file holding a SQL write and an entry without a reason are hits:
   a unit test outside persistence that needs SQLite text belongs under tests/ or in an inline test module. A cargo configuration may not `include` another (M2T01R-62).
7. Append-only tables (docs/adr/ADR-029.md decision 6, J1): anywhere in the members' Rust sources (persistence and
   test code included), a literal (or a concat! run) holding an UPDATE, DELETE, REPLACE (INSERT OR REPLACE) or an
   upsert's DO UPDATE of acceptance_results or evidence is a hit, unless the whitelist's append_only_allow names its
   file, the function it is in and its statement, with a reason; an entry without a reason or no longer used is a hit.
   The database refuses such writes too (per-connection TEMP triggers); this check keeps the code from relying on it.
Out of reach of any of this (docs/22-tooling.md): a program, script or SQL that exists only at run time (a name or
text assembled from pieces, read from the environment or a file outside the candidate), conditional compilation
produced by a macro (M2T01R-53/54), and product code that uses a whitelisted development file (a kit test or tool)
as a run-time component (M2T01R-57). Writing such code is deliberate circumvention, which this check does not claim to
stop (docs/21: the checks guard against accidental drift, not against someone rewriting files on purpose); review
does.
Scanning no file at all fails (no empty scan). Prints a JSON summary; with --out appends one test-results.jsonl
line for TEST-062-05. --metadata reads `cargo metadata` JSON from a file (tests).
"""
from __future__ import annotations
import argparse, json, os, re, subprocess, sys, tomllib
from pathlib import Path

PERSISTENCE = 'crates/hoh-core/src/persistence/'
CORE = 'hoh-core'
SQL_WRITE = re.compile(
    r'\b(?:INSERT\s+(?:OR\s+\w+\s+)?INTO|REPLACE\s+INTO|UPDATE\s+(?:OR\s+\w+\s+)?\S+\s+SET|DELETE\s+FROM|UPSERT'
    r'|ON\s+CONFLICT\b[^;]*?\bDO\s+UPDATE)\b', re.IGNORECASE)
# Writes that change or remove a row of an append-only table (docs/adr/ADR-029.md decision 6, J1): UPDATE, DELETE,
# REPLACE (also INSERT OR REPLACE) and an upsert's DO UPDATE. A plain INSERT and INSERT OR IGNORE only add.
# An identifier as SQLite accepts it: bare, or quoted with "…", `…` or […] (M2T18R-52, -101).
IDENT = r'(?:"[^"]+"|`[^`]+`|\[[^\]]+\]|\w+)'
# The table: an optional schema (main, temp, quoted or not) and the name bare or quoted.
APPEND_ONLY_TABLES = (r'(?:(?:main|temp|"(?:main|temp)"|`(?:main|temp)`|\[(?:main|temp)\])\s*\.\s*)?'
                      r'(?:"(?:acceptance_results|evidence)"|`(?:acceptance_results|evidence)`'
                      r'|\[(?:acceptance_results|evidence)\]|(?:acceptance_results|evidence)\b)')
APPEND_ONLY = re.compile(
    r'\b(?:UPDATE\s+(?:OR\s+\w+\s+)?' + APPEND_ONLY_TABLES +
    r'(?:\s+(?:AS\s+)?(?!SET\b|INDEXED\b|NOT\b)' + IDENT + r')?(?:\s+(?:INDEXED\s+BY\s+' + IDENT +
    r'|NOT\s+INDEXED))?\s+SET'
    r'|DELETE\s+FROM\s+' + APPEND_ONLY_TABLES +
    r'|(?:INSERT\s+OR\s+)?REPLACE\s+INTO\s+' + APPEND_ONLY_TABLES +
    r'|INSERT\s+(?:OR\s+\w+\s+)?INTO\s+' + APPEND_ONLY_TABLES + r'[^;]*?\bON\s+CONFLICT\b[^;]*?\bDO\s+UPDATE)',
    re.IGNORECASE)
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


def mentions(text: str) -> bool:
    """Whether a text mentions SQLite or holds a SQL write statement (what check 6 looks for in every file)."""
    return bool(re.search(r'sqlite', text, re.IGNORECASE) or SQL_WRITE.search(text))


# AC-062-05's whitelist configuration: the files besides the Persistence module that may mention SQLite (check 6).
WHITELIST = 'scripts/product/persistence-whitelist.json'
# What a member literal may name although it mentions SQLite (CR-019): a JSON or Markdown file the product reads as
# data, listed with its reason in the whitelist's read_as_data, holding no SQL write.
DATA_SUFFIXES = ('.json', '.md')


def read_as_data(root: Path) -> dict:
    """The whitelist's read_as_data entries (path -> reason); none when it has none or cannot be read (check 6
    reports an unreadable whitelist)."""
    try:
        listed = json.loads((root / WHITELIST).read_text(encoding='utf-8')).get('read_as_data', {})
    except (OSError, ValueError, AttributeError):
        return {}
    return listed if isinstance(listed, dict) else {}
# Markdown here is prose about the system, not something the product runs or embeds.
DOCUMENTATION = ('docs/', 'reports/')


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


def split_rust(text: str, docs: list | None = None) -> tuple[str, list[tuple[int, int, str]]]:
    """(code with comments and literals blanked to spaces, [(start, end, decoded literal)]). With `docs`, the doc
    comments (`///`, `//!`, `/** */`, `/*! */`) are appended to it as (start, end, text): a macro may take them as
    `#[doc = $s:literal]` (M2T01R-63)."""
    code, literals, i, n = list(text), [], 0, len(text)

    def blank(a: int, b: int) -> None:
        for k in range(a, min(b, n)):
            if code[k] != '\n':
                code[k] = ' '
    while i < n:
        if text.startswith('//', i):
            j = text.find('\n', i)
            j = n if j < 0 else j
            if docs is not None and (text.startswith('//!', i) or (text.startswith('///', i) and not text.startswith('////', i))):
                docs.append((i, j, text[i + 3:j]))
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
            if docs is not None and (text.startswith('/*!', start) or (text.startswith('/**', start)
                                     and not text.startswith('/***', start) and not text.startswith('/**/', start))):
                docs.append((start, i, text[start + 3:i - 2]))
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


def attribute_spans(code: str) -> list[tuple[int, int]]:
    """(start, end) of every attribute (`#[…]`, `#![…]`, brackets balanced) in comment- and literal-free code."""
    out = []
    for m in re.finditer(r'#\s*!?\s*\[', code):
        depth, i = 0, m.end() - 1
        while i < len(code):
            depth += {'[': 1, ']': -1}.get(code[i], 0)
            if depth == 0:
                break
            i += 1
        out.append((m.start(), i + 1))
    return out


KEYWORDS = {'if', 'while', 'match', 'return', 'in', 'let', 'else', 'for', 'loop', 'mut', 'ref', 'as', 'where', 'move',
            'async', 'await', 'unsafe', 'dyn', 'impl', 'break', 'continue', 'yield'}


def identifier_before(code: str, end: int) -> tuple[int, str, bool]:
    """(start, name, raw) of the Rust identifier ending at `end` (XID rules, as `str.isidentifier`), `r#` included;
    name is '' when there is none."""
    i = end
    while i > 0 and ('a' + code[i - 1]).isidentifier():
        i -= 1
    name = code[i:end]
    if not name.isidentifier():
        return end, '', False
    raw = i >= 2 and code[i - 2:i] == 'r#' and (i == 2 or not ('a' + code[i - 3]).isidentifier())
    return (i - 2 if raw else i), name, raw


def macro_spans(code: str) -> list[tuple[int, int, str]]:
    """(start, end, kind) — kind 'definition' for `macro_rules! name {…}`, else 'invocation' — of every macro invocation
    and definition (`name!(…)`, `name![…]`, `name!{…}`,
    `macro_rules! name {…}`) in comment- and literal-free code. Names follow Rust identifiers: raw ones (`r#while!`,
    keywords included) and any XID identifier (`运行!`, a combining mark, M2T01R-64/-69); a keyword that is not raw
    (`if !(…)`) invokes nothing."""
    out = []
    pairs = {'(': ')', '[': ']', '{': '}'}
    for bang in re.finditer(r'!', code):
        j = bang.start()
        k = j
        while k > 0 and code[k - 1].isspace():
            k -= 1
        start, name, raw = identifier_before(code, k)
        if not name or (name in KEYWORDS and not raw):
            continue
        # An invocation has its delimiter right after `!`, so `count != { … }` compares (M2T01R-73); only a plain
        # `macro_rules!` followed by an identifier defines a macro, and `r#macro_rules!(…)` invokes one (M2T01R-71).
        after = code[j + 1:]
        named = re.match(r'\s*(r#)?([^\s(\[{;!=]+)\s*([(\[{])', after) if name == 'macro_rules' and not raw else None
        definition = bool(named and named.group(2).isidentifier())
        rest = named if definition else re.match(r'\s*([(\[{])', after)
        if not rest:
            continue
        opening = rest.group(rest.lastindex)
        closing, depth, i = pairs[opening], 0, j + 1 + rest.end() - 1
        while i < len(code):
            depth += 1 if code[i] == opening else -1 if code[i] == closing else 0
            if depth == 0:
                break
            i += 1
        out.append((start, i + 1, 'definition' if definition else 'invocation'))
    return out


def invocation_spans(code: str) -> list[tuple[int, int]]:
    """The macro invocations (not `macro_rules!` definitions): what lies inside them is input the macro may use at run
    time, whatever it looks like (M2T01R-61, -63)."""
    return [(a, b) for a, b, kind in macro_spans(code) if kind == 'invocation']


def documentation_spans(code: str) -> list[tuple[int, int]]:
    """The value of each doc attribute (`#[doc = …]`, also in `cfg_attr`) that is a real attribute: not inside a
    macro invocation, where `#[doc = "…"]` is only input the macro may use at run time (M2T01R-61); one a
    `macro_rules!` definition writes documents the generated item (M2T01R-65). Its literals, the
    files it embeds and the files its text names are documentation, what `///` writes (M2T01R-56, -60)."""
    macros = invocation_spans(code)
    out = []
    for a, b in attribute_spans(code):
        # A doc attribute that a macro definition writes documents what it generates (M2T01R-65); inside an
        # invocation it is the invoked macro's input.
        if any(x < a < y for x, y in macros):
            continue
        # The value starts right after `=` (literals are blanked to spaces in `code`).
        for m in re.finditer(r'[\[(,]\s*doc\s*=', code[a:b]):
            out.append((a + m.end(), b))
    return out


def append_only_allow(root: Path) -> list:
    """The whitelist's append_only_allow entries ({file, function, statement, reason}); none when it has none."""
    try:
        listed = json.loads((root / WHITELIST).read_text(encoding='utf-8')).get('append_only_allow', [])
    except (OSError, ValueError, AttributeError):
        return []
    return [e for e in listed if isinstance(e, dict)] if isinstance(listed, list) else []


def function_bodies(code: str) -> list[tuple[int, int, str]]:
    """Each `fn` with a body in `code` (comments and literals already blanked): (body start, body end, name), the body
    from its opening to its matching closing brace (M2T18R-02). A declaration ending in `;` has no body."""
    out = []
    for m in re.finditer(r'\bfn\s+(?:r#)?(\w+)', code):
        depth, i = 0, m.end()
        while i < len(code):
            c = code[i]
            if c in '([<':
                depth += 1
            elif c in ')]>' and depth > 0:
                depth -= 1
            elif c == ';' and depth == 0:
                break
            elif c == '{' and depth == 0:
                level, j = 0, i
                while j < len(code):
                    if code[j] == '{':
                        level += 1
                    elif code[j] == '}':
                        level -= 1
                        if level == 0:
                            break
                    j += 1
                out.append((i, j, m.group(1)))
                break
            i += 1
    return out


def enclosing_function(code: str, offset: int) -> str:
    """The name of the innermost function whose body holds `offset` in `code` (comments and literals already
    blanked); empty outside every function body (module level, a const or impl item after a function)."""
    inside = [(start, name) for start, end, name in function_bodies(code) if start < offset <= end]
    return max(inside)[1] if inside else ''


def literal_runs(code: str, literals: list[tuple[int, int, str]]) -> list[tuple[int, str]]:
    """Each literal with its start, and runs of literals separated only by commas and whitespace joined, with the
    run's start (as `literal_strings`, keeping offsets)."""
    out, run = [], []
    for k, (start, _end, value) in enumerate(literals):
        out.append((start, value))
        if run and re.fullmatch(r'[\s,]*', code[literals[k - 1][1]:start]):
            run.append((start, value))
        else:
            if len(run) > 1:
                out.append((run[0][0], ''.join(v for _, v in run)))
            run = [(start, value)]
    if len(run) > 1:
        out.append((run[0][0], ''.join(v for _, v in run)))
    return out


def append_only_checks(rel: str, code: str, literals: list[tuple[int, int, str]], allow: list, used: set,
                       hits: list) -> None:
    """Check 7: a write that changes or removes a row of acceptance_results or evidence, anywhere in the members' Rust
    sources (persistence and tests included), is a hit unless append_only_allow names its file, function and
    statement (docs/adr/ADR-029.md decision 6)."""
    seen = set()
    for start, value in literal_runs(code, literals):
        for m in APPEND_ONLY.finditer(value):
            statement = ' '.join(m.group(0).split()).upper()
            function = enclosing_function(code, start)
            if (function, statement) in seen:
                continue
            seen.add((function, statement))
            # An entry without its reason allows nothing (it is a hit itself).
            allowed = [k for k, e in enumerate(allow) if e.get('file') == rel and e.get('function') == function
                       and str(e.get('reason', '')).strip()
                       and ' '.join(str(e.get('statement', '')).split()).upper() in statement]
            if allowed:
                used.update(allowed)
            else:
                hits.append({'path': rel, 'kind': 'append-only', 'match': f'{function}: {statement}'})


def runtime_literals(code: str, literals: list[tuple[int, int, str]]) -> list[tuple[int, int, str]]:
    """The literals but those of documentation (`documentation_spans`)."""
    docs = documentation_spans(code)
    return [lit for lit in literals if not any(a <= lit[0] < b for a, b in docs)]


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
    # --all-features: the dependency features that any build could enable are the ones checked (M2T01R-37).
    r = subprocess.run(['cargo', 'metadata', '--format-version', '1', '--locked', '--all-features'], cwd=root,
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
    for p in packages.values():
        # A path dependency that is no workspace member would be compiled into the product without the workspace's
        # clippy run or the source checks (M2T01R-48).
        if p.get('source') is None and p['id'] not in members:
            hits.append({'path': p['manifest_path'], 'kind': 'path-dependency', 'match': f"{p['name']} is not a workspace member"})
    for member in members:
        if packages[member].get('features'):
            # A feature can compile code clippy never sees (`cfg(not(feature = …))` against an all-features lint, or
            # the reverse) and can switch on dependency APIs; the members declare none (M2T01R-35).
            hits.append({'path': packages[member]['manifest_path'], 'kind': 'features',
                         'match': f"{packages[member]['name']} declares {sorted(packages[member]['features'])}"})
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


def rust_files(root: Path, metadata: dict) -> list[Path]:
    """Every .rs file of the candidate in the member directories: in a git work tree the candidate's files (git's
    ignore rules alone decide, so a member beside a moved build output is read, M2T01R-59), elsewhere every file but
    the build output. Which of them are compiled into the product depends on targets and module paths, so the lint
    checks look at all of them (M2T01R-25, -29)."""
    dirs = member_dirs(metadata)
    if (root / '.git').exists():
        return [p for p in repository_files(root, metadata)
                if p.suffix == '.rs' and p.is_file() and not p.is_symlink() and any(p.is_relative_to(d) for d in dirs)]
    build = Path(metadata.get('target_directory') or '/nonexistent').resolve()
    files = set()
    for d in dirs:
        files |= {p for p in d.rglob('*.rs') if not p.resolve().is_relative_to(build)}
    return sorted(files)


FORBID = '#![forbid(clippy::disallowed_methods)]'
FORBID_UNSAFE = '#![forbid(unsafe_code)]'
CORE_LIB_ITEM = re.compile(r'(?P<attrs>(?:#\[[^\]]*\]\s*)*)(?:pub\s+)?mod\s+(?P<name>\w+)\s*;')
CORE_VERSION = re.compile(r'pub\s+const\s+VERSION\s*:\s*&\s*str\s*=\s*env!\s*\(\s*\)\s*;')


def product_roots_outside_src(metadata: dict) -> list[str]:
    """Product target roots (lib, bin, proc-macro) that do not lie under their member's src/: module files resolve
    below a root's directory, so a root elsewhere (e.g. `tool.rs` at the member's top) could `mod`-import files from
    tests/ that the checks treat as test code (M2T01R-46)."""
    members = set(metadata['workspace_members'])
    out = []
    for p in metadata['packages']:
        if p['id'] not in members:
            continue
        src = (Path(p['manifest_path']).parent / 'src').resolve()
        out += [t['src_path'] for t in p.get('targets') or []
                if not set(t.get('kind') or []) <= {'test', 'bench', 'example', 'custom-build'}
                and not Path(t['src_path']).resolve().is_relative_to(src)]
    return sorted(out)


def targets_outside_members(metadata: dict) -> list[str]:
    """Target roots of workspace members outside every member directory: the source checks read the member
    directories, so a root elsewhere would be compiled unread (M2T01R-40)."""
    dirs = [d.resolve() for d in member_dirs(metadata)]
    members = set(metadata['workspace_members'])
    return sorted(t['src_path'] for p in metadata['packages'] if p['id'] in members for t in p.get('targets') or []
                  if not any(Path(t['src_path']).resolve().is_relative_to(d) for d in dirs))


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


CFG_TEST = r'#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]'


def cfg_test_spans(code: str) -> list[tuple[int, int]]:
    """The bodies of inline modules under exactly `#[cfg(test)]` (other attributes may follow it, the name may be raw)
    in comment- and literal-free code: compiled only into test builds (M2T01R-66, -70). Inside a macro invocation the
    attribute is only input the macro may drop (M2T01R-68), and a module file (`#[cfg(test)] mod tests;`) is never
    exempt: which files a build compiles is not decided here (M2T01R-67)."""
    macros = invocation_spans(code)
    out = []
    pattern = CFG_TEST + r'(?:\s*#\s*\[[^\]]*\])*\s*(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+(?:r#)?([^\s{};]+)\s*\{'
    for m in re.finditer(pattern, code):
        # Any Rust (XID) identifier names the module, combining marks included (M2T01R-72).
        if not m.group(1).isidentifier() or any(a < m.start() < b for a, b in macros):
            continue
        depth, i = 0, m.end() - 1
        while i < len(code):
            depth += {'{': 1, '}': -1}.get(code[i], 0)
            if depth == 0:
                break
            i += 1
        out.append((m.start(), i + 1))
    return out


def is_test_code(path: Path, metadata: dict) -> bool:
    """Whether a file is test code: it lies under a member's own tests/, benches/ or examples/ directory and no product
    target root (lib, bin, proc-macro, build script) lies there. Product modules cannot reach such a file (module files
    resolve below their root and `#[path]` is refused), whereas a test target elsewhere (e.g. beside product modules in
    src/) exempts nothing (M2T01R-42)."""
    members = set(metadata['workspace_members'])
    product_roots = [Path(t['src_path']).resolve() for p in metadata['packages'] if p['id'] in members
                     for t in p.get('targets') or [] if not set(t.get('kind') or []) <= {'test', 'bench', 'example'}]
    path = path.resolve()
    for d in member_dirs(metadata):
        for name in ('tests', 'benches', 'examples'):
            area = (d / name).resolve()
            if path.is_relative_to(area) and not any(r.is_relative_to(area) for r in product_roots):
                return True
    return False


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
        for name, profile in (data.get('profile') or {}).items():
            # static.sh lints dev and release; another profile, or `panic`, would compile code it never sees
            # (`cfg(panic = "abort")`, M2T01R-44).
            if name not in ('dev', 'release'):
                hits.append({'path': rel_to(manifest, root), 'kind': 'profile', 'match': f'profile.{name}'})
            elif isinstance(profile, dict) and 'panic' in profile:
                hits.append({'path': rel_to(manifest, root), 'kind': 'profile', 'match': f'profile.{name}.panic'})
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
    found += [f'build.{k}' for k in ('rustflags', 'rustc', 'rustc-wrapper', 'rustc-workspace-wrapper', 'rustdocflags',
                                     'target-dir') if k in build]
    found += [f'target.{name}.rustflags' for name, t in (data.get('target') or {}).items() if isinstance(t, dict) and 'rustflags' in t]
    found += ['env' for _ in [1] if data.get('env')]
    found += [f'alias.{a}' for a in (data.get('alias') or {}) if a in ('clippy', 'check', 'build', 'metadata', 'deny', 'fmt')]
    found += ['unstable' for _ in [1] if data.get('unstable')]
    # An included configuration would apply without being read here (M2T01R-62).
    found += ['include' for _ in [1] if 'include' in data]
    # The profile rule of the manifests holds for configurations too (M2T01R-50).
    for name, profile in (data.get('profile') or {}).items():
        if name not in ('dev', 'release'):
            found.append(f'profile.{name}')
        elif isinstance(profile, dict) and 'panic' in profile:
            found.append(f'profile.{name}.panic')
    return found


def rel_to(path: Path, root: Path) -> str:
    return path.relative_to(root).as_posix() if path.is_relative_to(root) else path.as_posix()


def scan(root: Path, metadata: dict) -> dict:
    hits = []
    data = read_as_data(root)
    allow = append_only_allow(root)
    used_allow: set = set()
    dev_uses = dependency_checks(metadata, hits)
    lint_configuration_checks(root, metadata, hits)
    files = rust_files(root, metadata)
    forbid_checks(root, metadata, hits)
    for src in targets_outside_members(metadata):
        hits.append({'path': rel_to(Path(src), root), 'kind': 'target-outside-members',
                     'match': 'a target root outside the member directories'})
    for src in product_roots_outside_src(metadata):
        hits.append({'path': rel_to(Path(src), root), 'kind': 'product-root-outside-src',
                     'match': "a product target root outside its member's src/"})
    for path in files:
        rel = rel_to(path, root)
        doc_comments: list = []
        code, literals = split_rust(path.read_text(encoding='utf-8', errors='replace'), doc_comments)
        # Whitespace removed and raw identifiers read as plain ones (M2T01R-23, -27).
        compact = [re.sub(r'\br#', '', re.sub(r'\s+', '', attr)) for attr in attributes(code)]
        if any(re.search(r'[\[(,]path=', attr) for attr in compact) or re.search(r'\binclude\s*!\s*[(\[{]', code):
            hits.append({'path': rel, 'kind': 'path-or-include', 'match': '#[path] (also in cfg_attr) or include!'})
        append_only_checks(rel, code, runtime_literals(code, literals), allow, used_allow, hits)
        if rel.startswith(PERSISTENCE):
            if re.search(r'\bmacro_rules\s*!', code):
                # A macro made in persistence could carry its allowance or a cfg into other code (M2T01R-49).
                hits.append({'path': rel, 'kind': 'persistence-macro', 'match': 'macro_rules! in persistence'})
            continue
        if re.search(r'\buse\b[^;]*\binclude(?:_str|_bytes)?\b', code):
            # A renamed include macro (`use std::include_str as embed`) hides what is embedded (M2T01R-47).
            hits.append({'path': rel, 'kind': 'included-file', 'match': 'an include macro imported under another name'})
        for attr in compact:
            if re.search(r'(?:allow|expect)\(', attr) and SUPPRESSING.search(attr):
                hits.append({'path': rel, 'kind': 'lint-suppressed', 'match': attr[:200]})
        # Code compiled only outside clippy (`cfg(not(clippy))`) escapes the lint (M2T01R-36).
        for predicate in cfg_predicates(code):
            # A predicate taken from a macro argument (`cfg($cond)`) cannot be read here (M2T01R-43).
            if re.search(r'\bclippy\b', predicate) or '$' in predicate:
                hits.append({'path': rel, 'kind': 'clippy-cfg', 'match': predicate[:200]})
        if is_test_code(path, metadata):
            continue
        # Doc comments inside a macro invocation are its input (M2T01R-63); code under #[cfg(test)] is test code
        # (M2T01R-66).
        invocations = invocation_spans(code)
        tests = cfg_test_spans(code)
        def product(start: int) -> bool:
            return not any(a <= start < b for a, b in tests)
        runtime = sorted([lit for lit in runtime_literals(code, literals) if product(lit[0])]
                         + [d for d in doc_comments if product(d[0]) and any(a < d[0] < b for a, b in invocations)])
        docs = documentation_spans(code) + tests
        for kind, value in included_contents(path, code, literals, root, skip=docs):
            if kind == 'unverifiable':
                hits.append({'path': rel, 'kind': 'included-file', 'match': value})
            elif re.search(r'sqlite', value, re.IGNORECASE) or SQL_WRITE.search(value):
                hits.append({'path': rel, 'kind': 'included-file', 'match': f'{kind}: mentions SQLite or holds a SQL write'})
        for named, target, text in named_repository_files(root, path, runtime):
            # A literal naming a repository file that something runs or reads (`python3 scripts/db_helper.py`) is
            # checked like the literal itself (M2T01R-45), unless it is a data file listed as read as data that holds
            # no SQL write (CR-019).
            if target in data and target.endswith(DATA_SUFFIXES) and not SQL_WRITE.search(text):
                continue
            if re.search(r'sqlite', text, re.IGNORECASE) or SQL_WRITE.search(text):
                hits.append({'path': rel, 'kind': 'named-file', 'match': f'{named}: mentions SQLite or holds a SQL write'})
        sqlite_seen = []
        for value in literal_strings(code, runtime):
            if m := SQL_WRITE.search(value):
                hits.append({'path': rel, 'kind': 'sql', 'match': ' '.join(m.group(0).split())})
            elif re.search(r'sqlite', value, re.IGNORECASE) and not any(s in value for s in sqlite_seen):
                # e.g. `Command::new("sqlite3")` or `import sqlite3` for another interpreter (M2T01R-38); a joined run
                # is reported only when no literal of it was.
                sqlite_seen.append(value)
                hits.append({'path': rel, 'kind': 'sqlite-literal', 'match': value[:200]})
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
    for k, e in enumerate(allow):
        name = f"{str(e.get('file', '')).rsplit('/', 1)[-1]} {e.get('function', '')} {e.get('statement', '')}"
        if not str(e.get('reason', '')).strip():
            hits.append({'path': WHITELIST, 'kind': 'append-only-allow', 'match': f'{name}: no reason given'})
        elif k not in used_allow:
            hits.append({'path': WHITELIST, 'kind': 'append-only-allow', 'match': f'{name}: not used'})
    other_files = content_checks(root, metadata, hits)
    scanned = len(files) + web_files
    return {'files_scanned': scanned, 'web_files_scanned': web_files, 'web_present': web.is_dir(),
            'other_files_scanned': other_files,
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


def named_repository_files(root: Path, path: Path, literals: list[tuple[int, int, str]]) -> list[tuple[str, str, str]]:
    """(literal, the file relative to the repository, contents) for every string literal that names an existing file of
    the repository, relative to the repository, the source's directory or its member's directory."""
    out = []
    bases = [root, path.parent] + [p for p in path.parents if (p / 'Cargo.toml').is_file()][:1]
    for _, _, value in literals:
        if not value or len(value) > 255 or '\n' in value or '\0' in value:
            continue
        for base in bases:
            candidate = (base / value).resolve()
            if candidate.is_relative_to(root) and candidate.is_file():
                try:
                    out.append((value, rel_to(candidate, root), candidate.read_bytes().decode('utf-8', errors='replace')))
                except OSError:
                    pass
                break
    return out


def repository_files(root: Path, metadata: dict) -> list[Path]:
    """The candidate's files: in a git work tree the tracked and the untracked files that are not ignored (an ignored
    file is part of no candidate); elsewhere every file. Symbolic links are listed, not followed; the build output is
    left out."""
    target = Path(metadata.get('target_directory') or root / 'target').resolve()
    if (root / '.git').exists():
        # Only git's ignore rules decide; the build output is ignored like any generated file, and a candidate file
        # inside it is still read (M2T01R-55).
        out = subprocess.run(['git', '-C', str(root), 'ls-files', '-z', '--cached', '--others', '--exclude-standard'],
                             capture_output=True, check=True).stdout
        return sorted(p for p in (root / name for name in out.decode('utf-8', 'surrogateescape').split('\0') if name)
                      if os.path.lexists(p))
    else:
        paths = []
        for d, dirs, names in os.walk(root):
            dirs[:] = [n for n in dirs if n != '.git']
            paths += [Path(d) / n for n in names]
            paths += [Path(d) / n for n in dirs if (Path(d) / n).is_symlink()]
            dirs[:] = [n for n in dirs if not (Path(d) / n).is_symlink()]
    return sorted(p for p in paths if os.path.lexists(p) and not p.is_relative_to(target))


def content_checks(root: Path, metadata: dict, hits: list) -> int:
    """Check 6: every file of the candidate but the Persistence module and the members' Rust sources (checks 2-4) that
    mentions SQLite or holds a SQL write must be in the whitelist (Markdown under DOCUMENTATION is prose), so a helper
    is caught wherever it lies and however it is reached; the whitelist names only files that need it; no file is a
    symbolic link (a linked module would let product code run what the checks treat as test code, M2T01R-52).
    Returns the number of files read."""
    members = tuple(f'{rel_to(d, root)}/' for d in member_dirs(metadata))
    try:
        listed = json.loads((root / WHITELIST).read_text(encoding='utf-8'))['files']
        if not isinstance(listed, dict):
            raise ValueError('"files" is not an object of path -> reason')
    except (OSError, ValueError, KeyError, TypeError) as exc:
        hits.append({'path': WHITELIST, 'kind': 'whitelist', 'match': f'missing or unreadable: {exc}'})
        listed = {}
    for rel, why in sorted(listed.items()):
        # Each entry says why the file may mention SQLite (M2T01R-58).
        if not isinstance(why, str) or not why.strip():
            hits.append({'path': WHITELIST, 'kind': 'whitelist', 'match': f'{rel}: no reason given'})
    # Each read_as_data entry (CR-019): a reason, a JSON or Markdown file of the repository, no SQL write in it.
    for rel, why in sorted(read_as_data(root).items()):
        path = root / rel
        if not isinstance(why, str) or not why.strip():
            problem = 'no reason given'
        elif not rel.endswith(DATA_SUFFIXES):
            problem = 'not a JSON or Markdown file'
        elif path.is_symlink() or not path.is_file():
            problem = 'not a file of the repository'
        elif SQL_WRITE.search(path.read_bytes().decode('utf-8', errors='replace')):
            problem = 'holds a SQL write'
        else:
            continue
        hits.append({'path': WHITELIST, 'kind': 'whitelist', 'match': f'read_as_data {rel}: {problem}'})
    read = 0
    for path in repository_files(root, metadata):
        rel = rel_to(path, root)
        if path.is_symlink():
            hits.append({'path': rel, 'kind': 'symlink', 'match': f'-> {os.readlink(path)}'})
            continue
        if (rel.startswith(PERSISTENCE) or rel == WHITELIST or (rel.endswith('.rs') and rel.startswith(members))
                or not path.is_file()):
            continue
        read += 1
        if rel in listed or (rel.endswith('.md') and rel.startswith(DOCUMENTATION)):
            continue
        if mentions(path.read_bytes().decode('utf-8', errors='replace')):
            hits.append({'path': rel, 'kind': 'unlisted-file', 'match': 'mentions SQLite or holds a SQL write; not in the whitelist'})
    for rel in sorted(listed):
        path = root / rel
        if rel.startswith(PERSISTENCE):
            why = 'inside the Persistence module'
        elif rel.endswith('.rs') and rel.startswith(members):
            why = 'Rust sources of the members follow the literal and clippy rules'
        elif path.is_symlink() or not path.is_file():
            why = 'not a file of the repository'
        elif not mentions(path.read_bytes().decode('utf-8', errors='replace')):
            why = 'mentions no SQLite and holds no SQL write'
        else:
            continue
        hits.append({'path': WHITELIST, 'kind': 'whitelist', 'match': f'{rel}: {why}'})
    return read


def included_contents(path: Path, code: str, literals: list[tuple[int, int, str]], root: Path,
                      skip: list[tuple[int, int]] = ()) -> list[tuple[str, str]]:
    """The contents of the files a source embeds with `include_str!`/`include_bytes!` ((file, text) pairs), read like
    its literals: an embedded helper run by an interpreter is code too (M2T01R-41). A path that is not a plain literal
    cannot be read here and is reported as unverifiable."""
    out = []
    for m in re.finditer(r'\binclude_(?:str|bytes)\s*!\s*[(\[{]', code):
        if any(a <= m.start() < b for a, b in skip):
            continue  # embedded into documentation only
        lit = next((l for l in literals if l[0] >= m.end()), None)
        if lit is None or code[m.end():lit[0]].strip():
            out.append(('unverifiable', 'include_str!/include_bytes! of a computed path'))
            continue
        target = (path.parent / lit[2]).resolve()
        if not target.is_relative_to(root):
            out.append(('unverifiable', f'{lit[2]}: outside the repository'))
            continue
        try:
            out.append((target.name, target.read_bytes().decode('utf-8', errors='replace')))
        except OSError as exc:
            out.append(('unverifiable', f'{lit[2]}: {exc}'))
    return out


def cfg_predicates(code: str) -> list[str]:
    """The predicates of every `cfg(…)`, `cfg_attr(…)` and `cfg!(…)` in comment- and literal-free code."""
    out = []
    for m in re.finditer(r'\bcfg(_attr)?\s*!?\s*\(', code):
        depth, i, cut = 0, m.end() - 1, None
        while i < len(code):
            depth += {'(': 1, ')': -1}.get(code[i], 0)
            if depth == 0:
                break
            if depth == 1 and code[i] == ',' and m.group(1) and cut is None:
                cut = i  # cfg_attr(predicate, attributes…): only the predicate is a condition
            i += 1
        out.append(' '.join(code[m.end():cut if cut is not None else i].split()))
    return out


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
        detail = result.get('error') or (f"{result['files_scanned']} Rust/web files and {result.get('other_files_scanned', 0)} other "
                                         f"files scanned, {len(result['hits'])} hits; " + (
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
