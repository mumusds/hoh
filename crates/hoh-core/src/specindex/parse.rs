//! ADR-033's line grammar; source positions survive blank-line skipping.

use super::{IndexError, Source};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub(super) struct Parsed {
    pub requirements: Vec<Value>,
    pub scenarios: Vec<Value>,
}

struct Lines<'a> {
    path: &'a str,
    lines: Vec<(usize, &'a str)>,
    at: usize,
    end: usize,
}

impl<'a> Lines<'a> {
    fn peek(&self) -> &'a str {
        self.lines.get(self.at).map_or("", |(_, text)| text)
    }
    fn error(&self, error: impl Into<String>) -> IndexError {
        IndexError::at(
            self.path,
            self.lines.get(self.at).map_or(self.end, |(line, _)| *line),
            error,
        )
    }
    fn take(&mut self, want: &str) -> Result<(), IndexError> {
        if self.peek() != want {
            return Err(self.error(format!("expected {want}")));
        }
        self.at += 1;
        Ok(())
    }
    fn field(&mut self, name: &str) -> Result<String, IndexError> {
        let Some(value) = self
            .peek()
            .strip_prefix(name)
            .filter(|v| !v.trim().is_empty())
        else {
            return Err(self.error(format!("expected nonempty {name}line")));
        };
        self.at += 1;
        Ok(value.to_owned())
    }
    fn id(&self, ids: &mut BTreeSet<String>, id: &str) -> Result<(), IndexError> {
        if !ids.insert(id.to_owned()) {
            return Err(self.error(format!("duplicate id {id}")));
        }
        Ok(())
    }
    fn entries(&mut self, decisions: bool) -> Result<(), IndexError> {
        if self.peek() == "无" {
            self.at += 1;
            return Ok(());
        }
        while !self.peek().is_empty() && !self.peek().starts_with('#') {
            self.field("- ")?;
            if decisions {
                let at = self.at;
                let source = self.field("source: ")?;
                if !source.split_once('=').is_some_and(|(kind, id)| {
                    ["question_id", "input_id", "event_id"].contains(&kind) && identifier(id)
                }) {
                    self.at = at;
                    return Err(self.error("expected source: question_id|input_id|event_id=<id>"));
                }
            } else {
                if !["阻塞：是", "阻塞：否"].contains(&self.peek()) {
                    return Err(self.error("expected 阻塞：是 or 阻塞：否"));
                }
                self.at += 1;
            }
        }
        Ok(())
    }
}

fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.:/".contains(&b))
}

pub(super) fn document(source: &Source, ids: &mut BTreeSet<String>) -> Result<Parsed, IndexError> {
    let text = std::str::from_utf8(&source.bytes).map_err(|e| {
        let line = source.bytes[..e.valid_up_to()]
            .iter()
            .filter(|b| **b == b'\n')
            .count()
            + 1;
        IndexError::at(&source.path, line, "specification must be UTF-8")
    })?;
    let mut lines = Lines {
        path: &source.path,
        lines: text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
            .map(|(i, l)| (i + 1, l))
            .collect(),
        at: 0,
        end: text.lines().count().max(1),
    };
    for heading in ["# 用户", "# 范围", "# 非目标"] {
        lines.take(heading)?;
        while !lines.peek().is_empty() && !lines.peek().starts_with('#') {
            if lines.peek().trim_start().starts_with('#') {
                return Err(lines.error("headings must not be indented"));
            }
            lines.at += 1;
        }
    }
    lines.take("# 决策")?;
    lines.entries(true)?;
    lines.take("# 开放问题")?;
    lines.entries(false)?;
    lines.take("# 需求")?;
    let mut parsed = Parsed {
        requirements: Vec::new(),
        scenarios: Vec::new(),
    };
    while !lines.peek().is_empty() {
        let Some((req, title)) = lines
            .peek()
            .strip_prefix("## REQ-")
            .and_then(|s| s.split_once(' '))
        else {
            return Err(lines.error("expected ## REQ-<id> <title>"));
        };
        if req.is_empty()
            || !req.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || !req.as_bytes()[0].is_ascii_alphanumeric()
            || title.trim().is_empty()
        {
            return Err(lines.error("invalid requirement id or title"));
        }
        let id = format!("REQ-{req}");
        lines.id(ids, &id)?;
        lines.at += 1;
        let class = if lines.peek().starts_with("class:") {
            let class = lines.field("class: ")?;
            if !["required", "degradable"].contains(&class.as_str()) {
                return Err(lines.error("class must be required or degradable"));
            }
            class
        } else {
            "required".to_owned()
        };
        let mut acceptance = Vec::new();
        while lines.peek().starts_with("### ") {
            let Some(ac) = lines.peek().strip_prefix("### AC-") else {
                return Err(lines.error("expected ### AC-<req>-<nn>"));
            };
            if !ac.rsplit_once('-').is_some_and(|(owner, n)| {
                owner == req && n.len() == 2 && n.bytes().all(|b| b.is_ascii_digit())
            }) {
                return Err(lines.error("AC must belong to its REQ and have a two-digit ordinal"));
            }
            let ac_id = format!("AC-{ac}");
            let test_id = format!("TEST-{ac}");
            lines.id(ids, &ac_id)?;
            lines.at += 1;
            let given = lines.field("given: ")?;
            let when = lines.field("when: ")?;
            let then = lines.field("then: ")?;
            let check_at = lines.at;
            let check = lines.field("check: ")?;
            if !identifier(&check) {
                lines.at = check_at;
                return Err(lines.error("invalid check id"));
            }
            acceptance.push(json!({"id": ac_id, "given": given, "when": when, "then": then, "test_id": test_id}));
            parsed.scenarios.push(json!({"id": test_id, "requirement_id": id, "acceptance_id": ac_id, "check_id": check}));
        }
        acceptance.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        parsed.requirements.push(json!({"id": id, "title": title, "release_class": class, "release_targets": ["project"], "acceptance": acceptance}));
    }
    Ok(parsed)
}
