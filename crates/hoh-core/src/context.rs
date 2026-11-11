//! Context packing (REQ-039, ADR-026): a role contract's four layers filled within a token budget.
//!
//! Pure: no I/O, no clock, no hash-map iteration, no floating point, so the same inputs pack to the same bytes
//! (behaviour 8). Mandatory items are never cut: a budget below them is `TooSmall` and no prompt is made
//! (behaviour 2, failure mode 1). What they leave is shared 45/35/20 among code, evidence and knowledge as hard caps
//! (behaviour 3, decision 1); within a class entries go in by rank as a strict prefix, each in full, else as its own
//! summary, else as a reference only (failure mode 2). Every entry not in full has an index item with its path and
//! the sha256 of its full text, in an index object the prompt names by one fixed-length pointer line (behaviour 4).
//! Every count is the estimate below and marked estimated (behaviour 6, decision 2).

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// The estimator's id, recorded in the truncation policy.
pub const ESTIMATOR: &str = "utf8-bytes/3";
/// Code, evidence and knowledge, in percent of what the mandatory items leave (REQ-039 behaviour 3).
pub const SHARES: [u64; 3] = [45, 35, 20];
/// The label of an entry recalled from project memory: experience, not acceptance fact (behaviour 7).
pub const MEMORY_LABEL: &str = "经验，不作验收事实";

const PLACEHOLDERS: [(&str, Layer); 4] = [
    ("GOAL", Layer::Goal),
    ("PROJECT_CONTEXT", Layer::Project),
    ("TASK_CONTEXT", Layer::Task),
    ("EVIDENCE_CONTEXT", Layer::Evidence),
];

/// The estimated tokens of `text`: its UTF-8 bytes divided by three, rounded up. Monotone and subadditive (the
/// estimates of two pieces add up to at least the estimate of both), so counting piece by piece bounds the whole.
pub fn estimate(text: &str) -> u64 {
    (text.len() as u64).div_ceil(3)
}

/// The three classes' shares of `remaining` tokens: floors of 45, 35 and 20 percent.
pub fn shares(remaining: u64) -> [u64; 3] {
    // In u128 so no remaining overflows; each share is at most `remaining`, so it fits back in u64.
    SHARES.map(|percent| (u128::from(remaining) * u128::from(percent) / 100) as u64)
}

/// The truncation policy every packed prompt records.
pub fn policy() -> Value {
    json!({
        "estimator": ESTIMATOR,
        "shares": SHARES,
        "order": ["relevance desc", "causal desc", "recency desc", "path asc", "sha256 asc"],
        "rule": "prefix",
    })
}

/// One of the four layers, each a placeholder of the role contract (docs/15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    Goal,
    Project,
    Task,
    Evidence,
}

/// A candidate entry's class; each has its share and its layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    Code,
    Evidence,
    Knowledge,
}

/// What an entry is: plain, or recalled from project memory (labelled as experience).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Plain,
    Memory,
}

/// How an entry went in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Full,
    Summary,
    Reference,
}

/// The mandatory items of each layer, already rendered by the caller; never cut. Goal, project and task must not be
/// blank; the evidence layer always has the index pointer.
#[derive(Debug, Clone, Default)]
pub struct Mandatory {
    pub goal: String,
    pub project: String,
    pub task: String,
    pub evidence: String,
}

/// A candidate for the remaining budget. Scores are 0..=100; the packer computes the sha256 of `text` itself.
#[derive(Debug, Clone)]
pub struct Entry {
    pub class: Class,
    pub kind: Kind,
    pub path: String,
    pub text: String,
    /// The entry's own summary, if it has one; the packer never summarises.
    pub summary: Option<String>,
    pub relevance: u8,
    pub causal: u8,
    pub recency: u8,
}

/// Every entry as it went in, in class order then rank.
#[derive(Debug, Clone, Serialize)]
pub struct Input {
    pub path: String,
    pub sha256: String,
    pub class: Class,
    pub kind: Kind,
    pub disposition: Disposition,
    /// The estimate of the entry's full text.
    pub tokens: u64,
    /// What it cost in the prompt (0 for a reference).
    pub placed: u64,
}

/// An entry not in full: where its full text is.
#[derive(Debug, Clone, Serialize)]
pub struct IndexItem {
    pub path: String,
    pub sha256: String,
    pub class: Class,
    pub disposition: Disposition,
    pub tokens: u64,
}

/// The counts of a packed prompt, all estimated.
#[derive(Debug, Clone, Serialize)]
pub struct Stats {
    pub budget: u64,
    pub mandatory: u64,
    pub shares: [u64; 3],
    pub code: u64,
    pub evidence: u64,
    pub knowledge: u64,
    /// Each layer's filled text, in Layer order.
    pub layers: [u64; 4],
    pub total: u64,
    pub estimated: bool,
}

/// A packed prompt.
#[derive(Debug, Clone)]
pub struct Packed {
    pub prompt: String,
    pub sha256: String,
    pub stats: Stats,
    pub inputs: Vec<Input>,
    pub index: Vec<IndexItem>,
    /// The index object's bytes (canonical JSON) and their sha256, which the prompt's pointer names.
    pub index_object: Vec<u8>,
    pub index_sha256: String,
}

/// Why nothing was packed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackError {
    /// CONTEXT_BUDGET_TOO_SMALL: the mandatory items need `required` estimated tokens, the budget is `available`.
    TooSmall { required: u64, available: u64 },
    /// The template has some but not all four placeholders, an unknown one, or one twice.
    Template(String),
    /// A layer's mandatory items are blank.
    EmptyLayer(Layer),
}

impl std::fmt::Display for PackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackError::TooSmall {
                required,
                available,
            } => write!(
                f,
                "CONTEXT_BUDGET_TOO_SMALL: the mandatory items need {required} estimated tokens, the budget is {available}"
            ),
            PackError::Template(why) => write!(f, "the role contract's placeholders: {why}"),
            PackError::EmptyLayer(layer) => {
                write!(f, "the {layer:?} layer's mandatory items are blank")
            }
        }
    }
}

impl std::error::Error for PackError {}

/// The estimated tokens the mandatory items take in `template`: the budget below which nothing is packed.
pub fn required(template: &str, mandatory: &Mandatory) -> Result<u64, PackError> {
    let parts = Template::parse(template)?;
    let layers = base_layers(mandatory, &ZERO_SHA)?;
    Ok(estimate(&parts.fill(&layers)))
}

/// Packs `entries` into `template` with `mandatory` within `budget` estimated tokens.
pub fn pack(
    template: &str,
    mandatory: &Mandatory,
    entries: &[Entry],
    budget: u64,
) -> Result<Packed, PackError> {
    let parts = Template::parse(template)?;
    let base = base_layers(mandatory, &ZERO_SHA)?;
    let required = estimate(&parts.fill(&base));
    if budget < required {
        return Err(PackError::TooSmall {
            required,
            available: budget,
        });
    }
    let caps = shares(budget - required);
    // Rank each class's entries by one total order; an entry given twice (same path and text) counts once.
    let mut ranked: Vec<(&Entry, String)> = entries
        .iter()
        .map(|e| (e, sha256_hex(e.text.as_bytes())))
        .collect();
    ranked.sort_by(|(a, a_sha), (b, b_sha)| {
        a.class
            .cmp(&b.class)
            .then(b.relevance.cmp(&a.relevance))
            .then(b.causal.cmp(&a.causal))
            .then(b.recency.cmp(&a.recency))
            .then(a.path.cmp(&b.path))
            .then(a_sha.cmp(b_sha))
            .then(a.summary.cmp(&b.summary))
            .then((a.kind == Kind::Memory).cmp(&(b.kind == Kind::Memory)))
    });
    ranked.dedup_by(|(b, b_sha), (a, a_sha)| {
        a.class == b.class && a.path == b.path && a_sha == b_sha
    });
    let mut blocks: [String; 3] = Default::default();
    let mut used = [0u64; 3];
    let mut broken = [false; 3];
    let mut inputs = Vec::with_capacity(ranked.len());
    for (entry, sha) in &ranked {
        let c = entry.class as usize;
        let left = caps[c] - used[c];
        let mut disposition = Disposition::Reference;
        let mut text = String::new();
        if !broken[c] {
            let full = block(entry, Disposition::Full);
            if estimate(&full) <= left {
                (disposition, text) = (Disposition::Full, full);
            } else if entry.summary.is_some() {
                let summary = block(entry, Disposition::Summary);
                if estimate(&summary) <= left {
                    (disposition, text) = (Disposition::Summary, summary);
                }
            }
            // Strict prefix (failure mode 2): once one fits neither way, it and every later one are references.
            broken[c] = disposition == Disposition::Reference;
        }
        let placed = estimate(&text);
        used[c] += placed;
        blocks[c].push_str(&text);
        inputs.push(Input {
            path: entry.path.clone(),
            sha256: sha.clone(),
            class: entry.class,
            kind: entry.kind,
            disposition,
            tokens: estimate(&entry.text),
            placed,
        });
    }
    let index: Vec<IndexItem> = inputs
        .iter()
        .filter(|i| i.disposition != Disposition::Full)
        .map(|i| IndexItem {
            path: i.path.clone(),
            sha256: i.sha256.clone(),
            class: i.class,
            disposition: i.disposition,
            tokens: i.tokens,
        })
        .collect();
    let index_object = serde_json::to_vec(&json!({"estimator": ESTIMATOR, "items": index}))
        .map_err(|e| PackError::Template(format!("the index object: {e}")))?;
    let index_sha256 = sha256_hex(&index_object);
    // Each class's blocks after its layer's mandatory items: code in the task layer, knowledge in the project
    // layer, evidence in the evidence layer before the index pointer. Every block adds its own bytes only, so the
    // whole estimate is at most the mandatory estimate plus each block's (subadditivity): within the budget.
    let mut layers = base_layers(mandatory, &index_sha256)?;
    layers[Layer::Task as usize].insert_str(mandatory.task.len(), &blocks[Class::Code as usize]);
    layers[Layer::Project as usize]
        .insert_str(mandatory.project.len(), &blocks[Class::Knowledge as usize]);
    layers[Layer::Evidence as usize]
        .insert_str(mandatory.evidence.len(), &blocks[Class::Evidence as usize]);
    let prompt = parts.fill(&layers);
    let stats = Stats {
        budget,
        mandatory: required,
        shares: caps,
        code: used[Class::Code as usize],
        evidence: used[Class::Evidence as usize],
        knowledge: used[Class::Knowledge as usize],
        layers: [0, 1, 2, 3].map(|l| estimate(&layers[l])),
        total: estimate(&prompt),
        estimated: true,
    };
    Ok(Packed {
        sha256: sha256_hex(prompt.as_bytes()),
        prompt,
        stats,
        inputs,
        index,
        index_object,
        index_sha256,
    })
}

/// An entry's block as it goes into its layer: a heading line with its class, path and sha256, then its full text
/// or its own summary.
pub(crate) fn block(entry: &Entry, disposition: Disposition) -> String {
    let class = match entry.class {
        Class::Code => "代码",
        Class::Evidence => "证据",
        Class::Knowledge => "知识",
    };
    let memory = match entry.kind {
        Kind::Memory => format!("·{MEMORY_LABEL}"),
        Kind::Plain => String::new(),
    };
    let (what, body) = match (disposition, &entry.summary) {
        (Disposition::Summary, Some(summary)) => ("·摘要", summary.as_str()),
        _ => ("", entry.text.as_str()),
    };
    // One heading line whatever the path holds.
    let path: String = entry
        .path
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let sha = sha256_hex(entry.text.as_bytes());
    format!("\n\n#### 〔{class}{memory}{what}〕{path} sha256:{sha}\n{body}")
}

const ZERO_SHA: [u8; 64] = [b'0'; 64];

/// The four layers' mandatory text, in Layer order; the evidence layer ends with the index pointer, whose length
/// does not depend on the index (a sha256 is always 64 hex digits), so the mandatory estimate is known up front.
fn base_layers(
    mandatory: &Mandatory,
    index_sha256: &(impl AsRef<[u8]> + ?Sized),
) -> Result<[String; 4], PackError> {
    for (layer, text) in [
        (Layer::Goal, &mandatory.goal),
        (Layer::Project, &mandatory.project),
        (Layer::Task, &mandatory.task),
    ] {
        if text.trim().is_empty() {
            return Err(PackError::EmptyLayer(layer));
        }
    }
    let sha = String::from_utf8_lossy(index_sha256.as_ref());
    let pointer =
        format!("引用索引：未装入全文的条目（path 与 sha256，可取回原文）见对象 sha256:{sha}");
    let evidence = if mandatory.evidence.is_empty() {
        pointer
    } else {
        format!("{}\n\n{pointer}", mandatory.evidence)
    };
    Ok([
        mandatory.goal.clone(),
        mandatory.project.clone(),
        mandatory.task.clone(),
        evidence,
    ])
}

/// A role contract split at its placeholders: either all four, each once, filled in place in the template's own
/// order, or none, the layers then following the contract in a fixed order.
struct Template<'a> {
    /// The text before each placeholder, the placeholder's layer, and the text after the last one.
    pieces: Vec<(&'a str, Layer)>,
    tail: &'a str,
}

impl<'a> Template<'a> {
    fn parse(template: &'a str) -> Result<Template<'a>, PackError> {
        // Each `{{NAME}}` with NAME of capitals, digits and underscores, as tools/render_prompt.py reads them.
        let mut slots: Vec<(usize, usize, Layer)> = Vec::new();
        let mut from = 0;
        while let Some(offset) = template[from..].find("{{") {
            let start = from + offset;
            let Some(len) = template[start + 2..].find("}}") else {
                break;
            };
            let name = &template[start + 2..start + 2 + len];
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            {
                from = start + 1;
                continue;
            }
            let layer = PLACEHOLDERS
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, l)| *l)
                .ok_or_else(|| {
                    PackError::Template(format!("unknown placeholder {{{{{name}}}}}"))
                })?;
            if slots.iter().any(|(_, _, l)| *l == layer) {
                return Err(PackError::Template(format!("{{{{{name}}}}} appears twice")));
            }
            let end = start + 2 + len + 2;
            slots.push((start, end, layer));
            from = end;
        }
        if slots.is_empty() {
            // No placeholder: the contract verbatim (its trailing blank lines dropped), then the four layers.
            return Ok(Template {
                pieces: Vec::new(),
                tail: template.trim_end(),
            });
        }
        if slots.len() != PLACEHOLDERS.len() {
            return Err(PackError::Template(format!(
                "{} of the four placeholders",
                slots.len()
            )));
        }
        let mut pieces = Vec::with_capacity(slots.len());
        let mut at = 0;
        for (start, end, layer) in slots {
            pieces.push((&template[at..start], layer));
            at = end;
        }
        Ok(Template {
            pieces,
            tail: &template[at..],
        })
    }

    fn fill(&self, layers: &[String; 4]) -> String {
        let mut out = String::new();
        if self.pieces.is_empty() {
            out.push_str(self.tail);
            for (heading, layer) in [
                ("当前目标", Layer::Goal),
                ("项目上下文", Layer::Project),
                ("任务上下文", Layer::Task),
                ("证据上下文", Layer::Evidence),
            ] {
                out.push_str(&format!("\n\n### {heading}\n"));
                out.push_str(&layers[layer as usize]);
            }
            out.push('\n');
            return out;
        }
        for (text, layer) in &self.pieces {
            out.push_str(text);
            out.push_str(&layers[*layer as usize]);
        }
        out.push_str(self.tail);
        out
    }
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests;
