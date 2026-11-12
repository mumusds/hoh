//! JSON Schema validation for the HoH contracts (`contracts/*.schema.json`), which stay the single source of truth for
//! the wire types (docs/04-architecture.md: Rust types and schema must not drift apart).
//!
//! Only the draft 2020-12 keywords these contracts use are implemented; a schema using any other keyword is refused
//! when it is loaded, so a contract change cannot silently stop being enforced. `$ref` names a local definition
//! (`#/$defs/<name>`) only, and a reference cycle that does not descend into the instance is refused; instances nested
//! deeper than [`MAX_DEPTH`] are refused by one iterative pass before anything recurses. Evaluation itself is bounded
//! separately (512 nested schema evaluations per thread, which a schema can need even for a shallow instance); running
//! out of it fails the whole validation, never a single branch of oneOf/anyOf/if. Numbers compare exactly
//! (integers beyond 2^53 included, 1 == 1.0); numbers serde_json cannot hold exactly (integers outside i64/u64, which
//! it rounds to f64) are refused instead of compared. Count keywords such as `minLength` accept integral floats.
//! `pattern` is limited to constructs that mean the same in ECMA-262 (which JSON Schema specifies) and the regex
//! crate; in both `$` is the end of input, while Python's `re` also matches before a final newline. `format` supports
//! `date-time` (RFC 3339 with ASCII digits and, like Python's checker, no second 60). Where Python knowingly differs,
//! the keyword corpus records it (`python_valid`).
//! The corpora in `tests/fixtures/schema/` (contract documents and keyword semantics) pin the verdicts to those of
//! Python jsonschema (the bundle test checks Python, `tests/schema_corpus.rs` this code).

use regex::Regex;
use serde_json::{Map, Number, Value};
use std::cell::Cell;
use std::collections::HashMap;
use std::fmt;
use std::sync::OnceLock;

/// Nested schema evaluations allowed on one thread; deeper is a violation, not a stack overflow.
const MAX_EVAL_DEPTH: usize = 512;

thread_local! {
    static EVAL_DEPTH: Cell<usize> = const { Cell::new(0) };
    /// Set when an evaluation ran out of depth: the whole validation then fails, whatever combinators made of the
    /// branch that ran out (M1T01R-22).
    static EXHAUSTED: Cell<bool> = const { Cell::new(false) };
}

/// One level of schema evaluation on this thread; released when dropped.
struct EvalDepth;

impl EvalDepth {
    fn enter() -> Option<Self> {
        EVAL_DEPTH.with(|d| {
            (d.get() < MAX_EVAL_DEPTH).then(|| {
                d.set(d.get() + 1);
                EvalDepth
            })
        })
    }
}

impl Drop for EvalDepth {
    fn drop(&mut self) {
        EVAL_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

/// Drop a value without recursion (serde_json drops nested values recursively).
fn dismantle(value: Value) {
    let mut stack = vec![value];
    while let Some(v) = stack.pop() {
        match v {
            Value::Array(items) => stack.extend(items),
            Value::Object(members) => stack.extend(members.into_iter().map(|(_, v)| v)),
            _ => {}
        }
    }
}

/// One way an instance fails a schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// JSON pointer into the instance ("" for the instance itself).
    pub instance_path: String,
    /// JSON pointer into the schema document of the keyword that failed.
    pub schema_path: String,
    pub message: String,
}

/// A schema document that cannot be used: an unsupported keyword, a malformed value, or an unknown definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaError(pub String);

impl fmt::Display for SchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unusable schema: {}", self.0)
    }
}

impl std::error::Error for SchemaError {}

const ANNOTATIONS: &[&str] = &[
    "$schema",
    "$id",
    "$comment",
    "title",
    "description",
    "examples",
    "default",
    "deprecated",
];
const KEYWORDS: &[&str] = &[
    "$defs",
    "$ref",
    "type",
    "enum",
    "const",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "allOf",
    "anyOf",
    "oneOf",
    "if",
    "then",
    "else",
    "pattern",
    "minLength",
    "maxLength",
    "minimum",
    "maximum",
    "minItems",
    "maxItems",
    "minProperties",
    "uniqueItems",
    "format",
];
const TYPES: &[&str] = &[
    "null", "boolean", "object", "array", "number", "integer", "string",
];

/// Arrays and objects allowed on any path of a value: the most serde_json itself parses from text. Deeper values (only
/// constructible in memory) are refused before anything recurses into them (M1T01R-11).
pub const MAX_DEPTH: usize = 127;

/// Whether a number is exactly what was written: serde_json keeps integers within i64/u64 exactly and turns larger
/// ones into f64, rounding them; every value of that magnitude is therefore refused instead of compared (M1T01R-14).
/// An f64 of exactly -2^63 can only come from a rounded literal (-2^63 itself parses as i64), so the low end is open.
fn exact(n: &Number) -> bool {
    n.is_i64()
        || n.is_u64()
        || n.as_f64()
            .is_some_and(|f| f > -9_223_372_036_854_775_808.0 && f < 18_446_744_073_709_551_616.0)
}

/// One iterative pass over the whole value, before any recursive step: nesting depth and number exactness.
fn precheck(value: &Value) -> Option<Violation> {
    let mut stack: Vec<(&Value, usize, String)> = vec![(value, 0, String::new())];
    while let Some((v, depth, path)) = stack.pop() {
        let violation = |message: String| Violation {
            instance_path: path.clone(),
            schema_path: String::new(),
            message,
        };
        match v {
            Value::Array(_) | Value::Object(_) if depth + 1 > MAX_DEPTH => {
                return Some(violation(format!("nested deeper than {MAX_DEPTH} levels")));
            }
            Value::Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    stack.push((item, depth + 1, format!("{path}/{i}")));
                }
            }
            Value::Object(members) => {
                for (key, item) in members {
                    stack.push((item, depth + 1, format!("{path}/{}", escape(key))));
                }
            }
            Value::Number(n) if !exact(n) => {
                return Some(violation(format!(
                    "{n} is outside the range HoH compares exactly (integers within i64/u64)"
                )));
            }
            _ => {}
        }
    }
    None
}

/// A short rendering of a value for messages.
fn brief(v: &Value) -> String {
    let text = v.to_string();
    match text.char_indices().nth(120) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text,
    }
}

/// A loaded schema document.
#[derive(Debug)]
pub struct Schema {
    document: Value,
    patterns: HashMap<String, Matcher>,
}

impl Schema {
    /// Load a schema document, refusing keywords and values this validator does not implement.
    pub fn new(document: Value) -> Result<Self, SchemaError> {
        if let Some(v) = precheck(&document) {
            dismantle(document); // too deep to drop recursively (M1T01R-20)
            return Err(SchemaError(format!("{}: {}", v.instance_path, v.message)));
        }
        let mut patterns = HashMap::new();
        check_schema(&document, &document, "", &mut patterns)?;
        check_reference_cycles(&document)?;
        Ok(Self { document, patterns })
    }

    /// The schema document itself.
    pub fn document(&self) -> &Value {
        &self.document
    }

    /// Validate an instance against the document's root schema.
    pub fn validate(&self, instance: &Value) -> Result<(), Vec<Violation>> {
        if let Some(v) = precheck(instance) {
            return Err(vec![v]);
        }
        let out = self.evaluate(&self.document, instance, "");
        if out.is_empty() { Ok(()) } else { Err(out) }
    }

    /// Evaluate from the top: a run out of evaluation depth anywhere, even inside a branch that oneOf, anyOf or if
    /// discarded, fails the whole validation.
    fn evaluate(&self, schema: &Value, instance: &Value, spath: &str) -> Vec<Violation> {
        EXHAUSTED.with(|e| e.set(false));
        let mut out = Vec::new();
        self.node(schema, instance, "", spath, &mut out);
        if EXHAUSTED.with(|e| e.replace(false)) {
            return vec![Violation {
                instance_path: String::new(),
                schema_path: spath.to_owned(),
                message: format!(
                    "schema evaluation nested too deep (more than {MAX_EVAL_DEPTH} levels)"
                ),
            }];
        }
        out
    }

    /// Validate an instance against `#/$defs/<def>`.
    pub fn validate_def(
        &self,
        def: &str,
        instance: &Value,
    ) -> Result<Result<(), Vec<Violation>>, SchemaError> {
        let pointer = format!("/$defs/{}", escape(def));
        let schema = self
            .document
            .pointer(&pointer)
            .ok_or_else(|| SchemaError(format!("no definition {def}")))?;
        if let Some(v) = precheck(instance) {
            return Ok(Err(vec![v]));
        }
        let out = self.evaluate(schema, instance, &pointer);
        Ok(if out.is_empty() { Ok(()) } else { Err(out) })
    }

    fn passes(&self, schema: &Value, instance: &Value, ipath: &str, spath: &str) -> bool {
        let mut scratch = Vec::new();
        self.node(schema, instance, ipath, spath, &mut scratch);
        scratch.is_empty()
    }

    fn node(
        &self,
        schema: &Value,
        inst: &Value,
        ipath: &str,
        spath: &str,
        out: &mut Vec<Violation>,
    ) {
        // Bounded however the schema is built, e.g. a long acyclic chain of references (M1T01R-21).
        let Some(_depth) = EvalDepth::enter() else {
            EXHAUSTED.with(|e| e.set(true));
            return fail(out, ipath, spath, "schema evaluation nested too deep");
        };
        let obj = match schema {
            Value::Bool(true) => return,
            Value::Bool(false) => return fail(out, ipath, spath, "no value is allowed here"),
            Value::Object(obj) => obj,
            _ => return fail(out, ipath, spath, "schema is not an object"),
        };
        let at = |key: &str| format!("{spath}/{key}");
        if let Some(Value::String(target)) = obj.get("$ref") {
            match target
                .strip_prefix('#')
                .and_then(|p| self.document.pointer(p))
            {
                Some(resolved) => {
                    self.node(resolved, inst, ipath, target.trim_start_matches('#'), out)
                }
                None => fail(
                    out,
                    ipath,
                    &at("$ref"),
                    &format!("unresolvable reference {target}"),
                ),
            }
        }
        if let Some(types) = obj.get("type") {
            let allowed: Vec<&str> = match types {
                Value::String(t) => vec![t.as_str()],
                Value::Array(ts) => ts.iter().filter_map(Value::as_str).collect(),
                _ => Vec::new(),
            };
            if !allowed.iter().any(|t| has_type(inst, t)) {
                fail(
                    out,
                    ipath,
                    &at("type"),
                    &format!("expected {}, got {}", allowed.join(" or "), type_name(inst)),
                );
            }
        }
        if let Some(Value::Array(values)) = obj.get("enum")
            && !values.iter().any(|v| json_eq(v, inst))
        {
            fail(
                out,
                ipath,
                &at("enum"),
                &format!("{} is not one of the allowed values", brief(inst)),
            );
        }
        if let Some(expected) = obj.get("const")
            && !json_eq(expected, inst)
        {
            fail(
                out,
                ipath,
                &at("const"),
                &format!("expected {}", brief(expected)),
            );
        }
        match inst {
            Value::String(s) => self.string(obj, s, ipath, spath, out),
            Value::Number(n) => number(obj, n, ipath, spath, out),
            Value::Array(items) => self.array(obj, items, ipath, spath, out),
            Value::Object(members) => self.object(obj, members, ipath, spath, out),
            _ => {}
        }
        if let Some(Value::Array(all)) = obj.get("allOf") {
            for (i, sub) in all.iter().enumerate() {
                self.node(sub, inst, ipath, &format!("{spath}/allOf/{i}"), out);
            }
        }
        if let Some(Value::Array(any)) = obj.get("anyOf")
            && !any
                .iter()
                .enumerate()
                .any(|(i, sub)| self.passes(sub, inst, ipath, &format!("{spath}/anyOf/{i}")))
        {
            fail(out, ipath, &at("anyOf"), "matches none of the alternatives");
        }
        if let Some(Value::Array(one)) = obj.get("oneOf") {
            let matching = one
                .iter()
                .enumerate()
                .filter(|(i, sub)| self.passes(sub, inst, ipath, &format!("{spath}/oneOf/{i}")))
                .count();
            if matching != 1 {
                fail(
                    out,
                    ipath,
                    &at("oneOf"),
                    &format!("matches {matching} alternatives, expected exactly one"),
                );
            }
        }
        if let Some(cond) = obj.get("if") {
            let branch = if self.passes(cond, inst, ipath, &at("if")) {
                "then"
            } else {
                "else"
            };
            if let Some(sub) = obj.get(branch) {
                self.node(sub, inst, ipath, &at(branch), out);
            }
        }
    }

    fn string(
        &self,
        obj: &Map<String, Value>,
        s: &str,
        ipath: &str,
        spath: &str,
        out: &mut Vec<Violation>,
    ) {
        let length = s.chars().count() as u64;
        if let Some(min) = obj.get("minLength").and_then(count)
            && length < min
        {
            fail(
                out,
                ipath,
                &format!("{spath}/minLength"),
                &format!("shorter than {min} characters"),
            );
        }
        if let Some(max) = obj.get("maxLength").and_then(count)
            && length > max
        {
            fail(
                out,
                ipath,
                &format!("{spath}/maxLength"),
                &format!("longer than {max} characters"),
            );
        }
        if let Some(Value::String(pattern)) = obj.get("pattern")
            && !self.patterns.get(pattern).is_some_and(|m| m.is_match(s))
        {
            fail(
                out,
                ipath,
                &format!("{spath}/pattern"),
                &format!("does not match {pattern}"),
            );
        }
        if obj.get("format").and_then(Value::as_str) == Some("date-time") && !is_date_time(s) {
            fail(
                out,
                ipath,
                &format!("{spath}/format"),
                "not an RFC 3339 date-time",
            );
        }
    }

    fn array(
        &self,
        obj: &Map<String, Value>,
        items: &[Value],
        ipath: &str,
        spath: &str,
        out: &mut Vec<Violation>,
    ) {
        if let Some(sub) = obj.get("items") {
            for (i, item) in items.iter().enumerate() {
                self.node(
                    sub,
                    item,
                    &format!("{ipath}/{i}"),
                    &format!("{spath}/items"),
                    out,
                );
            }
        }
        let len = items.len() as u64;
        if obj
            .get("minItems")
            .and_then(count)
            .is_some_and(|min| len < min)
        {
            fail(out, ipath, &format!("{spath}/minItems"), "too few items");
        }
        if obj
            .get("maxItems")
            .and_then(count)
            .is_some_and(|max| len > max)
        {
            fail(out, ipath, &format!("{spath}/maxItems"), "too many items");
        }
        if obj.get("uniqueItems") == Some(&Value::Bool(true)) {
            let duplicate = items
                .iter()
                .enumerate()
                .any(|(i, a)| items[..i].iter().any(|b| json_eq(a, b)));
            if duplicate {
                fail(
                    out,
                    ipath,
                    &format!("{spath}/uniqueItems"),
                    "items are not unique",
                );
            }
        }
    }

    fn object(
        &self,
        obj: &Map<String, Value>,
        members: &Map<String, Value>,
        ipath: &str,
        spath: &str,
        out: &mut Vec<Violation>,
    ) {
        if let Some(Value::Array(required)) = obj.get("required") {
            for name in required.iter().filter_map(Value::as_str) {
                if !members.contains_key(name) {
                    fail(
                        out,
                        ipath,
                        &format!("{spath}/required"),
                        &format!("missing required property {name}"),
                    );
                }
            }
        }
        let properties = obj.get("properties").and_then(Value::as_object);
        for (name, value) in members {
            let child = format!("{ipath}/{}", escape(name));
            match properties.and_then(|p| p.get(name)) {
                Some(sub) => self.node(
                    sub,
                    value,
                    &child,
                    &format!("{spath}/properties/{}", escape(name)),
                    out,
                ),
                None => {
                    if let Some(extra) = obj.get("additionalProperties") {
                        if extra == &Value::Bool(false) {
                            fail(
                                out,
                                &child,
                                &format!("{spath}/additionalProperties"),
                                &format!("unexpected property {name}"),
                            );
                        } else {
                            self.node(
                                extra,
                                value,
                                &child,
                                &format!("{spath}/additionalProperties"),
                                out,
                            );
                        }
                    }
                }
            }
        }
        if obj
            .get("minProperties")
            .and_then(count)
            .is_some_and(|min| (members.len() as u64) < min)
        {
            fail(
                out,
                ipath,
                &format!("{spath}/minProperties"),
                "too few properties",
            );
        }
    }
}

fn number(
    obj: &Map<String, Value>,
    n: &Number,
    ipath: &str,
    spath: &str,
    out: &mut Vec<Violation>,
) {
    use std::cmp::Ordering;
    if let Some(Value::Number(min)) = obj.get("minimum")
        && cmp_numbers(n, min).is_none_or(|o| o == Ordering::Less)
    {
        fail(
            out,
            ipath,
            &format!("{spath}/minimum"),
            &format!("less than {min}"),
        );
    }
    if let Some(Value::Number(max)) = obj.get("maximum")
        && cmp_numbers(n, max).is_none_or(|o| o == Ordering::Greater)
    {
        fail(
            out,
            ipath,
            &format!("{spath}/maximum"),
            &format!("greater than {max}"),
        );
    }
}

fn fail(out: &mut Vec<Violation>, ipath: &str, spath: &str, message: &str) {
    out.push(Violation {
        instance_path: ipath.to_owned(),
        schema_path: spath.to_owned(),
        message: message.to_owned(),
    });
}

fn escape(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn is_integer(n: &Number) -> bool {
    n.is_i64()
        || n.is_u64()
        || n.as_f64()
            .is_some_and(|f| f.is_finite() && f.fract() == 0.0)
}

fn has_type(v: &Value, t: &str) -> bool {
    match (t, v) {
        ("integer", Value::Number(n)) => is_integer(n),
        ("number", Value::Number(_)) => true,
        _ => type_name(v) == t,
    }
}

fn as_int(n: &Number) -> Option<i128> {
    n.as_i64()
        .map(i128::from)
        .or_else(|| n.as_u64().map(i128::from))
}

/// Exact order of an integer and a float (no rounding of the integer through f64).
fn cmp_int_float(i: i128, f: f64) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    if f.is_nan() {
        return None;
    }
    if f.is_infinite() {
        return Some(if f > 0.0 {
            Ordering::Less
        } else {
            Ordering::Greater
        });
    }
    let floor = f.floor();
    if floor.abs() >= 1.0e38 {
        // Far beyond any integer serde_json keeps exactly.
        return Some(if f > 0.0 {
            Ordering::Less
        } else {
            Ordering::Greater
        });
    }
    let whole = floor as i128;
    Some(match i.cmp(&whole) {
        Ordering::Equal if f > floor => Ordering::Less,
        other => other,
    })
}

/// Exact numeric order (integers beyond 2^53 included); `None` only for NaN.
pub fn cmp_numbers(a: &Number, b: &Number) -> Option<std::cmp::Ordering> {
    match (as_int(a), as_int(b)) {
        (Some(x), Some(y)) => Some(x.cmp(&y)),
        (Some(x), None) => cmp_int_float(x, b.as_f64()?),
        (None, Some(y)) => cmp_int_float(y, a.as_f64()?).map(std::cmp::Ordering::reverse),
        (None, None) => a.as_f64()?.partial_cmp(&b.as_f64()?),
    }
}

/// A count keyword's value (`minLength` …): a non-negative integer, also written as an integral float.
fn count(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) => n.as_u64().or_else(|| {
            n.as_f64()
                .filter(|f| f.fract() == 0.0 && *f >= 0.0 && *f < 18_446_744_073_709_551_616.0)
                .map(|f| f as u64)
        }),
        _ => None,
    }
}

/// JSON equality as JSON Schema defines it: numbers compare by exact value (1 == 1.0), objects regardless of key order.
pub fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            cmp_numbers(x, y) == Some(std::cmp::Ordering::Equal)
        }
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| json_eq(a, b))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| json_eq(v, w)))
        }
        _ => a == b,
    }
}

/// RFC 3339 `date-time` (section 5.6): full-date "T" full-time, with "Z" or a numeric offset; T and Z in any case;
/// ASCII digits only; seconds 00-59 (Python's checker, which the corpora pin, refuses leap seconds).
pub fn is_date_time(s: &str) -> bool {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"(?i)^([0-9]{4})-([0-9]{2})-([0-9]{2})t([0-9]{2}):([0-9]{2}):([0-9]{2})(\.[0-9]+)?(z|[+-]([0-9]{2}):([0-9]{2}))$",
        )
        .ok()
    });
    let Some(caps) = re.as_ref().and_then(|re| re.captures(s)) else {
        return false;
    };
    let num = |i: usize| caps.get(i).and_then(|m| m.as_str().parse::<u32>().ok());
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) =
        (num(1), num(2), num(3), num(4), num(5), num(6))
    else {
        return false;
    };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    let offset_ok = caps.get(9).is_none()
        || (num(9).is_some_and(|h| h < 24) && num(10).is_some_and(|m| m < 60));
    (1..=days).contains(&day) && hour < 24 && minute < 60 && second < 60 && offset_ok
}

fn check_schema(
    doc: &Value,
    schema: &Value,
    path: &str,
    patterns: &mut HashMap<String, Matcher>,
) -> Result<(), SchemaError> {
    let obj = match schema {
        Value::Bool(_) => return Ok(()),
        Value::Object(obj) => obj,
        _ => {
            return Err(SchemaError(format!(
                "{path}: a schema must be an object or a boolean"
            )));
        }
    };
    let bad = |what: &str| SchemaError(format!("{path}: {what}"));
    for (key, value) in obj {
        if ANNOTATIONS.contains(&key.as_str()) {
            continue;
        }
        if !KEYWORDS.contains(&key.as_str()) {
            return Err(bad(&format!("unsupported keyword {key}")));
        }
        let here = format!("{path}/{}", escape(key));
        match key.as_str() {
            "$defs" | "properties" => {
                for (name, sub) in value
                    .as_object()
                    .ok_or_else(|| bad(&format!("{key} must be an object")))?
                {
                    check_schema(doc, sub, &format!("{here}/{}", escape(name)), patterns)?;
                }
            }
            "allOf" | "anyOf" | "oneOf" => {
                let subs = value
                    .as_array()
                    .filter(|a| !a.is_empty())
                    .ok_or_else(|| bad(&format!("{key} must be a non-empty array")))?;
                for (i, sub) in subs.iter().enumerate() {
                    check_schema(doc, sub, &format!("{here}/{i}"), patterns)?;
                }
            }
            "items" | "additionalProperties" | "if" | "then" | "else" => {
                check_schema(doc, value, &here, patterns)?
            }
            "$ref" => {
                // Only whole definitions: a pointer deeper into a definition could reach an unchecked subtree.
                let target = value.as_str().unwrap_or_default();
                if definition_name(target).is_none() {
                    return Err(bad(&format!(
                        "only references to a definition (#/$defs/<name>) are supported, not {value}"
                    )));
                }
                if doc.pointer(target.trim_start_matches('#')).is_none() {
                    return Err(bad(&format!("unresolvable reference {target}")));
                }
            }
            "type" => {
                let names: Vec<&Value> = match value {
                    Value::Array(ts) => ts.iter().collect(),
                    other => vec![other],
                };
                if names.is_empty()
                    || !names
                        .iter()
                        .all(|t| t.as_str().is_some_and(|t| TYPES.contains(&t)))
                {
                    return Err(bad("type must name JSON types"));
                }
            }
            "enum" if !value.is_array() => return Err(bad("enum must be an array")),
            "required"
                if !value
                    .as_array()
                    .is_some_and(|a| a.iter().all(Value::is_string)) =>
            {
                return Err(bad("required must list names"));
            }
            "pattern" => {
                let source = value
                    .as_str()
                    .ok_or_else(|| bad("pattern must be a string"))?;
                let matcher = match not_starting_with(source) {
                    Some(prefix) => Matcher::NotStartingWith(prefix),
                    None => {
                        portable_pattern(source)
                            .map_err(|e| bad(&format!("pattern {source}: {e}")))?;
                        Matcher::Regex(
                            Regex::new(source)
                                .map_err(|e| bad(&format!("pattern {source}: {e}")))?,
                        )
                    }
                };
                patterns.insert(source.to_owned(), matcher);
            }
            "minLength" | "maxLength" | "minItems" | "maxItems" | "minProperties"
                if count(value).is_none() =>
            {
                return Err(bad(&format!("{key} must be a non-negative integer")));
            }
            "minimum" | "maximum" if !value.is_number() => {
                return Err(bad(&format!("{key} must be a number")));
            }
            "uniqueItems" if !value.is_boolean() => {
                return Err(bad("uniqueItems must be a boolean"));
            }
            "format" if value.as_str() != Some("date-time") => {
                return Err(bad(&format!("unsupported format {value}")));
            }
            _ => {}
        }
    }
    Ok(())
}

/// A compiled `pattern`.
#[derive(Debug, Clone)]
enum Matcher {
    Regex(Regex),
    /// `^(?!<literal>)`: the string does not start with the literal (the regex crate has no lookaround).
    NotStartingWith(String),
}

impl Matcher {
    fn is_match(&self, s: &str) -> bool {
        match self {
            Matcher::Regex(re) => re.is_match(s),
            Matcher::NotStartingWith(prefix) => !s.starts_with(prefix.as_str()),
        }
    }
}

/// The literal of a pattern that is exactly `^(?!<literal>)`, its literal made of characters that mean themselves in
/// both dialects (no metacharacter, no escape): the one lookahead read, as ECMA-262 means it
/// (contracts/independent-report.schema.json's relative paths).
fn not_starting_with(source: &str) -> Option<String> {
    let literal = source.strip_prefix("^(?!")?.strip_suffix(')')?;
    let plain = !literal.is_empty()
        && literal
            .chars()
            .all(|c| !"\\^$.|?*+()[]{}".contains(c) && !c.is_control());
    plain.then(|| literal.to_owned())
}

/// Refuse pattern constructs whose meaning differs between ECMA-262 (what JSON Schema specifies) and the regex
/// crate: class escapes such as `\d`, `\w`, `\s`, `\b`, `\p` (Unicode-aware here, ASCII there), `.` outside a class
/// (line terminators differ) and `(?` groups. What remains (ASCII classes and ranges, literals, escaped punctuation,
/// alternation, groups, quantifiers, `^` and `$`) means the same in both (M1T01R-16).
fn portable_pattern(source: &str) -> Result<(), String> {
    let mut chars = source.chars().peekable();
    let mut in_class = false;
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(e) if e.is_ascii_punctuation() => {}
                Some(e) => {
                    return Err(format!(
                        "escape \\{e} differs between ECMA-262 and this validator"
                    ));
                }
                None => return Err("trailing backslash".to_owned()),
            },
            '[' if !in_class => {
                // ECMA-262 reads `[]` as an empty class and `[^]` as any character; the regex crate reads a leading
                // `]` as a literal (M1T01R-24).
                let mut ahead = chars.clone();
                if ahead.peek() == Some(&'^') {
                    ahead.next();
                }
                if ahead.peek() == Some(&']') {
                    return Err("a class starting with ] differs between dialects".to_owned());
                }
                in_class = true;
            }
            ']' if in_class => in_class = false,
            // Nested brackets (POSIX classes, unions) and the set operators && ~~ -- exist in the regex crate only.
            '[' => return Err("brackets inside a class differ between dialects".to_owned()),
            '&' | '~' | '-' if in_class && chars.peek() == Some(&c) => {
                return Err(format!("{c}{c} inside a class differs between dialects"));
            }
            '.' if !in_class => {
                return Err("`.` outside a class differs between dialects; use a class".to_owned());
            }
            '(' if !in_class && chars.peek() == Some(&'?') => {
                return Err("(? groups are not supported".to_owned());
            }
            _ => {}
        }
    }
    Ok(())
}

/// The definition name a reference `#/$defs/<segment>` names, with the JSON Pointer escapes (`~1`, `~0`) decoded;
/// `None` unless the target is exactly one well-formed segment.
fn definition_name(target: &str) -> Option<String> {
    let segment = target.strip_prefix("#/$defs/")?;
    if segment.is_empty() || segment.contains('/') {
        return None;
    }
    let mut name = String::new();
    let mut chars = segment.chars();
    while let Some(c) = chars.next() {
        match (c, c == '~') {
            (_, true) => match chars.next() {
                Some('0') => name.push('~'),
                Some('1') => name.push('/'),
                _ => return None,
            },
            (c, false) => name.push(c),
        }
    }
    Some(name)
}

/// The definitions a schema reaches without descending into the instance (`$ref` directly or inside
/// allOf/anyOf/oneOf/if/then/else); `properties`, `items` and `additionalProperties` descend and end the walk.
fn same_instance_refs(schema: &Value, found: &mut Vec<String>) {
    let Value::Object(obj) = schema else { return };
    if let Some(name) = obj
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(definition_name)
    {
        found.push(name);
    }
    for key in ["allOf", "anyOf", "oneOf"] {
        for sub in obj.get(key).and_then(Value::as_array).into_iter().flatten() {
            same_instance_refs(sub, found);
        }
    }
    for key in ["if", "then", "else"] {
        if let Some(sub) = obj.get(key) {
            same_instance_refs(sub, found);
        }
    }
}

/// Refuse reference cycles that never descend into the instance: they would recurse forever on any value (M1T01R-07).
fn check_reference_cycles(doc: &Value) -> Result<(), SchemaError> {
    let Some(defs) = doc.get("$defs").and_then(Value::as_object) else {
        return Ok(());
    };
    let edges = |name: &str| {
        let mut found = Vec::new();
        if let Some(def) = defs.get(name) {
            same_instance_refs(def, &mut found);
        }
        found
    };
    for start in defs.keys() {
        let mut stack: Vec<String> = edges(start);
        let mut seen: Vec<String> = Vec::new();
        while let Some(next) = stack.pop() {
            if &next == start {
                return Err(SchemaError(format!(
                    "#/$defs/{start} refers back to itself without descending into the instance"
                )));
            }
            if !seen.contains(&next) {
                stack.extend(edges(&next));
                seen.push(next);
            }
        }
    }
    Ok(())
}

fn load(name: &str, text: &str) -> Schema {
    // The embedded contracts are fixed at build time and exercised by the test suite; failing to load one is a build
    // defect, not a runtime condition.
    match serde_json::from_str(text)
        .map_err(|e| SchemaError(e.to_string()))
        .and_then(Schema::new)
    {
        Ok(schema) => schema,
        Err(e) => panic!("embedded {name} is unusable: {e}"),
    }
}

/// `contracts/protocol.schema.json` (HCP wire types).
pub fn protocol() -> &'static Schema {
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        load(
            "protocol.schema.json",
            include_str!("../../../contracts/protocol.schema.json"),
        )
    })
}

/// `contracts/compat-manifest.schema.json` (the product compat manifest written by probe()).
pub fn compat_manifest() -> &'static Schema {
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        load(
            "compat-manifest.schema.json",
            include_str!("../../../contracts/compat-manifest.schema.json"),
        )
    })
}

/// `contracts/test-result.schema.json` (one line of a check's test-results.jsonl, CR-003).
pub fn test_result() -> &'static Schema {
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        load(
            "test-result.schema.json",
            include_str!("../../../contracts/test-result.schema.json"),
        )
    })
}

/// `contracts/live-session.schema.json` (the native sessions a live check used).
pub fn live_session() -> &'static Schema {
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        load(
            "live-session.schema.json",
            include_str!("../../../contracts/live-session.schema.json"),
        )
    })
}

/// `contracts/independent-report.schema.json` (a verifier's report, docs/adr/ADR-028.md decision 15).
pub fn independent_report() -> &'static Schema {
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        load(
            "independent-report.schema.json",
            include_str!("../../../contracts/independent-report.schema.json"),
        )
    })
}

/// `contracts/native-attempt.schema.json` (the runtime's export of a native agent attempt).
pub fn native_attempt() -> &'static Schema {
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        load(
            "native-attempt.schema.json",
            include_str!("../../../contracts/native-attempt.schema.json"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unsupported_keywords_are_refused_when_loading() {
        for schema in [
            json!({"not": {}}),
            json!({"patternProperties": {}}),
            json!({"properties": {"a": {"prefixItems": []}}}),
            json!({"format": "email"}),
            json!({"$ref": "https://example.com/x"}),
            json!({"$ref": "#/$defs/missing"}),
            json!({"type": "date"}),
            json!({"pattern": "("}),
        ] {
            assert!(Schema::new(schema.clone()).is_err(), "{schema}");
        }
    }

    #[test]
    fn references_must_name_a_definition() {
        // M1T01R-06: a pointer into an annotation would reach an unchecked subtree.
        let into_annotation =
            json!({"$defs": {"x": {"default": {"not": {}}}}, "$ref": "#/$defs/x/default"});
        assert!(Schema::new(into_annotation).is_err());
        let nested = json!({"$defs": {"x": {"properties": {"a": {"type": "string"}}}}, "$ref": "#/$defs/x/properties/a"});
        assert!(Schema::new(nested).is_err());
    }

    #[test]
    fn reference_cycles_without_progress_are_refused() {
        // M1T01R-07: these would recurse forever on any instance.
        for schema in [
            json!({"$defs": {"a": {"$ref": "#/$defs/a"}}, "$ref": "#/$defs/a"}),
            json!({"$defs": {"a": {"$ref": "#/$defs/b"}, "b": {"allOf": [{"$ref": "#/$defs/a"}]}}, "$ref": "#/$defs/a"}),
            json!({"$defs": {"a": {"if": {"$ref": "#/$defs/a"}}}, "$ref": "#/$defs/a"}),
        ] {
            assert!(Schema::new(schema.clone()).is_err(), "{schema}");
        }
    }

    #[test]
    fn deep_instances_fail_instead_of_overflowing_the_stack() -> Result<(), SchemaError> {
        let schema = Schema::new(
            json!({"$defs": {"n": {"type": "array", "items": {"$ref": "#/$defs/n"}}}, "$ref": "#/$defs/n"}),
        )?;
        let mut deep = json!([]);
        for _ in 0..100_000 {
            deep = Value::Array(vec![deep]);
        }
        let errors = schema.validate(&deep).err().unwrap_or_default();
        assert!(
            errors.iter().any(|v| v.message.contains("nested")),
            "{:?}",
            errors.first()
        );
        std::mem::forget(deep); // dropping a 100k-deep value recursively would itself overflow the test stack
        Ok(())
    }

    #[test]
    fn escaped_definition_names_are_followed_for_cycles() -> Result<(), SchemaError> {
        // M1T01R-12: `a~1b` names the definition "a/b"; the cycle check must see it.
        for schema in [
            json!({"$defs": {"a/b": {"$ref": "#/$defs/a~1b"}}, "$ref": "#/$defs/a~1b"}),
            json!({"$defs": {"a~b": {"$ref": "#/$defs/a~0b"}}, "$ref": "#/$defs/a~0b"}),
        ] {
            assert!(Schema::new(schema.clone()).is_err(), "{schema}");
        }
        let fine =
            Schema::new(json!({"$defs": {"a/b": {"type": "string"}}, "$ref": "#/$defs/a~1b"}))?;
        assert!(fine.validate(&json!("x")).is_ok());
        assert!(fine.validate(&json!(1)).is_err());
        Ok(())
    }

    #[test]
    fn a_leading_negative_lookahead_of_a_literal_means_the_string_does_not_start_with_it() {
        // contracts/independent-report.schema.json's `^(?!/)` (a relative path): the one `(?` group read, exactly as
        // ECMA-262 means it; any other `(?` group stays refused.
        let schema = Schema::new(json!({"pattern": "^(?!/)"})).map_err(|e| e.0);
        let Ok(schema) = schema else {
            panic!("{schema:?}");
        };
        assert!(schema.validate(&json!("a/b")).is_ok());
        assert!(schema.validate(&json!("")).is_ok());
        assert!(schema.validate(&json!("/etc")).is_err());
        for refused in ["^(?!a)b", "(?!/)", "^(?=/)", "^(?!a|b)", r"^(?!\d)"] {
            assert!(
                Schema::new(json!({"pattern": refused})).is_err(),
                "{refused}"
            );
        }
        assert!(super::independent_report().validate(&json!({})).is_err());
    }

    #[test]
    fn dialect_dependent_pattern_constructs_are_refused() {
        // M1T01R-16: \d, \w, \s, \b and `.` mean different things in ECMA-262 and the regex crate.
        for pattern in [r"^\d+$", r"\w", r"\s", r"\bx", r"^.$", r"(?i)a", r"\p{L}"] {
            assert!(
                Schema::new(json!({"pattern": pattern})).is_err(),
                "{pattern}"
            );
        }
        for pattern in [
            "^[a-f0-9]{64}$",
            "^([a-f0-9]{40}|[a-f0-9]{64})$",
            r"^a\.b[.]c$",
            "x+y*z?",
        ] {
            assert!(
                Schema::new(json!({"pattern": pattern})).is_ok(),
                "{pattern}"
            );
        }
    }

    #[test]
    fn deep_values_are_refused_before_any_recursion() {
        // M1T01R-11: free-form members are not walked by the schema, so the depth check covers the whole value.
        let schema = match Schema::new(json!({"type": "object"})) {
            Ok(s) => s,
            Err(e) => panic!("{e}"),
        };
        let mut deep = json!([]);
        for _ in 0..100_000 {
            deep = Value::Array(vec![deep]);
        }
        let wrapped = json!({"payload": null});
        let mut wrapped = wrapped;
        wrapped["payload"] = deep;
        let errors = schema.validate(&wrapped).err().unwrap_or_default();
        assert!(
            errors.iter().any(|v| v.message.contains("nested")),
            "{:?}",
            errors.first()
        );
        std::mem::forget(wrapped);
        // The outer object is one level: MAX_DEPTH - 1 arrays inside it fit, MAX_DEPTH do not.
        let nest = |levels: usize| (0..levels).fold(json!(1), |v, _| Value::Array(vec![v]));
        assert!(schema.validate(&json!({"p": nest(MAX_DEPTH)})).is_err());
        assert!(schema.validate(&json!({"p": nest(MAX_DEPTH - 1)})).is_ok());
    }

    #[test]
    fn inexact_schema_numbers_are_refused() {
        assert!(Schema::new(json!({"maximum": 18_446_744_073_709_551_616.0_f64})).is_err());
        assert!(Schema::new(json!({"const": [1e300]})).is_err());
        assert!(Schema::new(json!({"maximum": 18_446_744_073_709_551_615_u64})).is_ok());
    }

    #[test]
    fn deep_owned_schema_documents_are_refused_without_overflow() {
        // M1T01R-20: Schema::new owns its document; refusing a deep one must not drop it recursively.
        let mut deep = json!([]);
        for _ in 0..100_000 {
            deep = Value::Array(vec![deep]);
        }
        // Built by hand: json!() would serialize the interpolated value recursively.
        let mut document = serde_json::Map::new();
        document.insert("const".to_owned(), deep);
        assert!(Schema::new(Value::Object(document)).is_err());
    }

    #[test]
    fn long_reference_chains_are_bounded() -> Result<(), SchemaError> {
        // M1T01R-21: an acyclic chain of references is legal but must not exhaust the stack when evaluated.
        let mut defs = serde_json::Map::new();
        for i in 0..1000 {
            defs.insert(
                format!("d{i}"),
                json!({"$ref": format!("#/$defs/d{}", i + 1)}),
            );
        }
        defs.insert("d1000".to_owned(), json!({"type": "null"}));
        let chain = Schema::new(json!({"$defs": defs, "$ref": "#/$defs/d0"}))?;
        let errors = chain.validate(&json!(null)).err().unwrap_or_default();
        assert!(
            errors.iter().any(|v| v.message.contains("too deep")),
            "{:?}",
            errors.first()
        );
        // A recursive schema over an instance at the nesting limit still evaluates.
        let tree = Schema::new(
            json!({"$defs": {"n": {"type": "array", "items": {"$ref": "#/$defs/n"}}}, "$ref": "#/$defs/n"}),
        )?;
        let nested = (0..(MAX_DEPTH - 1)).fold(json!([]), |v, _| Value::Array(vec![v]));
        assert!(tree.validate(&nested).is_ok());
        Ok(())
    }

    #[test]
    fn exhausted_evaluation_is_never_a_match() -> Result<(), SchemaError> {
        // M1T01R-22: running out of evaluation depth inside oneOf or if must fail the whole validation instead of
        // counting as "does not match" (which would let oneOf accept or if take the else branch).
        let mut defs = serde_json::Map::new();
        for i in 0..520 {
            defs.insert(
                format!("d{i}"),
                json!({"$ref": format!("#/$defs/d{}", i + 1)}),
            );
        }
        defs.insert("d520".to_owned(), json!({"type": "null"}));
        let one_of =
            Schema::new(json!({"$defs": defs.clone(), "oneOf": [true, {"$ref": "#/$defs/d0"}]}))?;
        let errors = one_of.validate(&json!(null)).err().unwrap_or_default();
        assert!(
            errors.iter().any(|v| v.message.contains("too deep")),
            "{errors:?}"
        );
        let cond = Schema::new(
            json!({"$defs": defs, "if": {"$ref": "#/$defs/d0"}, "then": false, "else": true}),
        )?;
        assert!(cond.validate(&json!(null)).is_err());
        // The next, unrelated validation on this thread is unaffected.
        assert!(
            Schema::new(json!({"type": "null"}))?
                .validate(&json!(null))
                .is_ok()
        );
        Ok(())
    }

    #[test]
    fn character_classes_are_limited_to_portable_forms() {
        // M1T01R-19: set operations, POSIX classes and nested brackets differ between ECMA-262 and the regex crate.
        for pattern in [
            "^[a&&b]$",
            "^[a~~b]$",
            "^[a-z--b]$",
            "^[[:alpha:]]$",
            "^[a[b]]$",
            // M1T01R-24: a class starting with ] (ECMA-262: empty class; regex crate: literal ]).
            "^[]a]$",
            "^[]a~~b]$",
            "^[^]a]$",
            "^[]$",
        ] {
            assert!(
                Schema::new(json!({"pattern": pattern})).is_err(),
                "{pattern}"
            );
        }
        for pattern in ["^[^a-z]$", r"^[a\-z]$", "^[A-Z0-9_]+$"] {
            assert!(
                Schema::new(json!({"pattern": pattern})).is_ok(),
                "{pattern}"
            );
        }
    }

    #[test]
    fn json_equality_follows_json_schema() {
        assert!(json_eq(&json!(1), &json!(1.0)));
        assert!(json_eq(
            &json!({"a": [1, {"b": 2}]}),
            &json!({"a": [1.0, {"b": 2}]})
        ));
        assert!(!json_eq(&json!("1"), &json!(1)));
        assert!(!json_eq(&json!({"a": 1}), &json!({"a": 1, "b": 2})));
    }

    #[test]
    fn date_time_follows_rfc_3339() {
        for ok in [
            "2026-11-04T01:02:03Z",
            "2024-02-29t23:59:59.5z",
            "2026-11-04T01:02:03-08:00",
        ] {
            assert!(is_date_time(ok), "{ok}");
        }
        for bad in [
            "2026-11-04",
            "2026-11-04 01:02:03Z",
            "2025-02-29T00:00:00Z",
            "2026-13-01T00:00:00Z",
            "2026-11-04T24:00:00Z",
            "2026-11-04T01:02:03",
            "2026-11-04T01:02:03+24:00",
            "2016-12-31T23:59:60Z",
        ] {
            assert!(!is_date_time(bad), "{bad}");
        }
    }

    #[test]
    fn embedded_contracts_load() {
        assert!(protocol().document().get("$defs").is_some());
        assert!(compat_manifest().document().get("$defs").is_some());
    }
}
