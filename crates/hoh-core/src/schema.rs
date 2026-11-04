//! JSON Schema validation for the HoH contracts (`contracts/*.schema.json`), which stay the single source of truth for
//! the wire types (docs/04-architecture.md: Rust types and schema must not drift apart).
//!
//! Only the draft 2020-12 keywords these contracts use are implemented; a schema using any other keyword is refused
//! when it is loaded, so a contract change cannot silently stop being enforced. `$ref` names a local definition
//! (`#/$defs/<name>`) only, and a reference cycle that does not descend into the instance is refused; instances nested
//! deeper than [`MAX_DEPTH`] are a violation instead of a stack overflow. Numbers compare exactly (integers beyond 2^53
//! included, 1 == 1.0) and count keywords such as `minLength` accept integral floats. `pattern` follows ECMA-262 as
//! JSON Schema requires (`$` is the end of input; Python's `re` also matches before a final newline, the one known
//! divergence). `format` supports `date-time` (RFC 3339 with ASCII digits and, like Python's checker, no second 60).
//! The corpora in `tests/fixtures/schema/` (contract documents and keyword semantics) pin the verdicts to those of
//! Python jsonschema (the bundle test checks Python, `tests/schema_corpus.rs` this code).

use regex::Regex;
use serde_json::{Map, Number, Value};
use std::collections::HashMap;
use std::fmt;
use std::sync::OnceLock;

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

/// Instances nested deeper than this are reported instead of validated further.
pub const MAX_DEPTH: usize = 256;

/// A loaded schema document.
#[derive(Debug)]
pub struct Schema {
    document: Value,
    patterns: HashMap<String, Regex>,
}

impl Schema {
    /// Load a schema document, refusing keywords and values this validator does not implement.
    pub fn new(document: Value) -> Result<Self, SchemaError> {
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
        let mut out = Vec::new();
        self.node(&self.document, instance, "", "", &mut out);
        if out.is_empty() { Ok(()) } else { Err(out) }
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
        let mut out = Vec::new();
        self.node(schema, instance, "", &pointer, &mut out);
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
        if ipath.matches('/').count() > MAX_DEPTH {
            return fail(
                out,
                ipath,
                spath,
                &format!("nested deeper than {MAX_DEPTH} levels"),
            );
        }
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
                &format!("{inst} is not one of the allowed values"),
            );
        }
        if let Some(expected) = obj.get("const")
            && !json_eq(expected, inst)
        {
            fail(out, ipath, &at("const"), &format!("expected {expected}"));
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
            && !self.patterns.get(pattern).is_some_and(|re| re.is_match(s))
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
    patterns: &mut HashMap<String, Regex>,
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
                let re = Regex::new(source).map_err(|e| bad(&format!("pattern {source}: {e}")))?;
                patterns.insert(source.to_owned(), re);
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

/// `name` for a reference to `#/$defs/<name>` (a single pointer segment), else `None`.
fn definition_name(target: &str) -> Option<&str> {
    target
        .strip_prefix("#/$defs/")
        .filter(|name| !name.is_empty() && !name.contains('/'))
}

/// The definitions a schema reaches without descending into the instance (`$ref` directly or inside
/// allOf/anyOf/oneOf/if/then/else); `properties`, `items` and `additionalProperties` descend and end the walk.
fn same_instance_refs<'a>(schema: &'a Value, found: &mut Vec<&'a str>) {
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
        let mut stack: Vec<&str> = edges(start);
        let mut seen: Vec<&str> = Vec::new();
        while let Some(next) = stack.pop() {
            if next == start {
                return Err(SchemaError(format!(
                    "#/$defs/{start} refers back to itself without descending into the instance"
                )));
            }
            if !seen.contains(&next) {
                seen.push(next);
                stack.extend(edges(next));
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
