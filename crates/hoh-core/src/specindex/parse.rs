//! ADR-033's line grammar; source positions survive blank-line skipping.

use super::{Decision, IndexError, Question, Source};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub(super) struct Parsed {
    pub requirements: Vec<Value>,
    pub scenarios: Vec<Value>,
    pub questions: Vec<Question>,
    pub decisions: Vec<Decision>,
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
    fn line(&self) -> usize {
        self.lines.get(self.at).map_or(self.end, |(line, _)| *line)
    }
    fn decisions(&mut self, parsed: &mut Parsed) -> Result<(), IndexError> {
        if self.peek() == "无" {
            self.at += 1;
            return Ok(());
        }
        while !self.peek().is_empty() && !self.peek().starts_with('#') {
            let line = self.line();
            let text = self.field("- ")?;
            let at = self.at;
            let source = self.field("source: ")?;
            let Some((kind, id)) = source.split_once('=').filter(|(kind, id)| {
                ["question_id", "input_id", "event_id"].contains(kind) && identifier(id)
            }) else {
                self.at = at;
                return Err(self.error("expected source: question_id|input_id|event_id=<id>"));
            };
            parsed.decisions.push(Decision {
                text,
                source_kind: kind.to_owned(),
                source_id: id.to_owned(),
                path: self.path.to_owned(),
                line,
            });
        }
        Ok(())
    }
    /// `- Q-<id> <text>` then `阻塞：是|否` (docs/adr/ADR-035.md decision 1); the id is in the REQ/AC id set.
    fn questions(
        &mut self,
        parsed: &mut Parsed,
        ids: &mut BTreeSet<String>,
    ) -> Result<(), IndexError> {
        if self.peek() == "无" {
            self.at += 1;
            return Ok(());
        }
        while !self.peek().is_empty() && !self.peek().starts_with('#') {
            let line = self.line();
            let Some((qid, text)) = self
                .peek()
                .strip_prefix("- Q-")
                .and_then(|s| s.split_once(' '))
                .filter(|(qid, text)| word(qid) && !text.trim().is_empty())
            else {
                return Err(self.error("expected - Q-<id> <question>"));
            };
            let id = format!("Q-{qid}");
            self.id(ids, &id)?;
            let text = text.to_owned();
            self.at += 1;
            let blocking = match self.peek() {
                "阻塞：是" => true,
                "阻塞：否" => false,
                _ => return Err(self.error("expected 阻塞：是 or 阻塞：否")),
            };
            self.at += 1;
            parsed.questions.push(Question {
                id,
                text,
                blocking,
                path: self.path.to_owned(),
                line,
            });
        }
        Ok(())
    }
}

/// `alnum (alnum | "_")*`: a REQ's or a question's own id.
fn word(s: &str) -> bool {
    !s.is_empty()
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.:/".contains(&b))
}

/// Split on LF. Strip one trailing CR only when it is the CR of a CRLF: every
/// segment but the last, and the last only when the text itself ends in LF.
/// A leftover CR (a bare CR, or a second CR before LF) is not a line ending.
/// The empty segment `split` yields after a final LF is dropped so line numbers
/// match `str::lines` for ordinary files (M3B-08).
fn split_lines(text: &str) -> Result<Vec<(usize, &str)>, usize> {
    let ends_with_lf = text.ends_with('\n');
    let parts: Vec<&str> = text.split('\n').collect();
    let mut out = Vec::new();
    for (index, line) in parts.iter().enumerate() {
        let last = index + 1 == parts.len();
        let body = if !last || ends_with_lf {
            line.strip_suffix('\r').unwrap_or(line)
        } else {
            *line
        };
        if body.contains('\r') {
            return Err(index + 1);
        }
        if last && ends_with_lf && body.is_empty() {
            continue;
        }
        out.push((index + 1, body));
    }
    Ok(out)
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
    let split = split_lines(text)
        .map_err(|line| IndexError::at(&source.path, line, "a value must be a single line"))?;
    let end = split.len().max(1);
    let mut lines = Lines {
        path: &source.path,
        lines: split
            .into_iter()
            .filter(|(_, l)| !l.trim().is_empty())
            .collect(),
        at: 0,
        end,
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
    let mut parsed = Parsed {
        requirements: Vec::new(),
        scenarios: Vec::new(),
        questions: Vec::new(),
        decisions: Vec::new(),
    };
    lines.take("# 决策")?;
    lines.decisions(&mut parsed)?;
    lines.take("# 开放问题")?;
    lines.questions(&mut parsed, ids)?;
    lines.take("# 需求")?;
    while !lines.peek().is_empty() {
        let Some((req, title)) = lines
            .peek()
            .strip_prefix("## REQ-")
            .and_then(|s| s.split_once(' '))
        else {
            return Err(lines.error("expected ## REQ-<id> <title>"));
        };
        if !word(req) || title.trim().is_empty() {
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
