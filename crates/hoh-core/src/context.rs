//! Context packing (REQ-039, ADR-026): a role contract's four layers filled within a token budget.
//!
//! Pure: no I/O, no clock, no hash-map iteration, no floating point, so the same inputs pack to the same bytes
//! (behaviour 8). Mandatory items are never cut: a budget below them is `TooSmall` and no prompt is made
//! (behaviour 2, failure mode 1). What they leave is shared 45/35/20 among code, evidence and knowledge as hard caps
//! (behaviour 3, decision 1); within a class entries go in by rank as a strict prefix, each in full, else as its own
//! summary, else as a reference only (failure mode 2). Every entry not in full has an index item with its path and
//! the sha256 of its full text, in an index object the prompt names by one fixed-length pointer line (behaviour 4).
//! Every count is the estimate below and marked estimated (behaviour 6, decision 2).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// The estimator's id, recorded in the truncation policy.
pub const ESTIMATOR: &str = "utf8-bytes/3";
/// The packing format: the fixed text and layout this code renders. Part of the resource bundle version
/// (docs/adr/ADR-026.md decision 7), so a build that renders differently never repacks an old prompt as the same
/// version; the golden test fails when the rendering changes and this does not.
pub const FORMAT: &str = "hoh-context/1";
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
    shares_of(remaining, SHARES)
}

fn shares_of(remaining: u64, percent: [u64; 3]) -> [u64; 3] {
    // In u128 so no remaining overflows; each share is at most `remaining`, so it fits back in u64.
    percent.map(|p| (u128::from(remaining) * u128::from(p.min(100)) / 100) as u64)
}

/// The truncation policy every packed prompt records.
pub fn policy() -> Value {
    json!({
        "format": FORMAT,
        "estimator": ESTIMATOR,
        "shares": SHARES,
        "order": ["relevance desc", "recency desc", "causal desc", "path asc", "sha256 asc"],
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    Code,
    Evidence,
    Knowledge,
}

/// What an entry is: plain, or recalled from project memory (labelled as experience).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
/// blank; the evidence layer always has the index pointer. `lead` (a new holder's first input, when it is owed) goes
/// into the value of the template's first placeholder, after that layer's own items and before any entry, so it is
/// read before every other context (docs/adr/ADR-026.md decision 3).
#[derive(Debug, Clone, Default)]
pub struct Mandatory {
    pub goal: String,
    pub project: String,
    pub task: String,
    pub evidence: String,
    pub lead: String,
    /// The schemas the role hands back (`output_schemas`): placed at the end of the task layer's items, counted with
    /// the role contract (docs/15; docs/adr/ADR-026.md decision 3).
    pub schemas: String,
}

/// A candidate for the remaining budget. The caller gives the scores; the packer computes the sha256 of `text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub class: Class,
    pub kind: Kind,
    pub path: String,
    pub text: String,
    /// The entry's own summary, if it has one; the packer never summarises.
    pub summary: Option<String>,
    pub relevance: u64,
    pub recency: u64,
    pub causal: u64,
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
    /// The four layers of REQ-039 behaviour 1 and docs/15.
    pub layers: Layers,
    pub total: u64,
    pub estimated: bool,
}

/// The estimate of each layer as REQ-039 behaviour 1 names them: the role contract (the template's own text), and
/// the project, task (the goal's value with the task's) and evidence values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Layers {
    pub role_contract: u64,
    pub project: u64,
    pub task: u64,
    pub evidence: u64,
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
    let (layers, _) = base_layers(mandatory, parts.first(), &ZERO_SHA)?;
    Ok(estimate(&parts.fill(&layers)))
}

/// Packs `entries` into `template` with `mandatory` within `budget` estimated tokens.
pub fn pack(
    template: &str,
    mandatory: &Mandatory,
    entries: &[Entry],
    budget: u64,
) -> Result<Packed, PackError> {
    pack_with(template, mandatory, entries, budget, SHARES)
}

/// [`pack`] with the run's own shares, in percent of what the mandatory items leave (its configuration snapshot,
/// docs/adr/ADR-026.md decision 7); together at most 100.
pub fn pack_with(
    template: &str,
    mandatory: &Mandatory,
    entries: &[Entry],
    budget: u64,
    percent: [u64; 3],
) -> Result<Packed, PackError> {
    if percent.iter().sum::<u64>() > 100 {
        return Err(PackError::Template(format!(
            "shares {percent:?} exceed 100"
        )));
    }
    let parts = Template::parse(template)?;
    let (base, _) = base_layers(mandatory, parts.first(), &ZERO_SHA)?;
    let required = estimate(&parts.fill(&base));
    if budget < required {
        return Err(PackError::TooSmall {
            required,
            available: budget,
        });
    }
    let caps = shares_of(budget - required, percent);
    // Rank each class's entries by one total order; an entry given twice (same path and text) counts once.
    let mut ranked: Vec<(&Entry, String)> = entries
        .iter()
        .map(|e| (e, sha256_hex(e.text.as_bytes())))
        .collect();
    ranked.sort_by(|(a, a_sha), (b, b_sha)| {
        a.class
            .cmp(&b.class)
            .then(b.relevance.cmp(&a.relevance))
            .then(b.recency.cmp(&a.recency))
            .then(b.causal.cmp(&a.causal))
            .then(a.path.cmp(&b.path))
            .then(a_sha.cmp(b_sha))
            .then(a.summary.cmp(&b.summary))
            .then((a.kind == Kind::Memory).cmp(&(b.kind == Kind::Memory)))
    });
    // One per (class, path, sha256), the first in that order (M2T09D-157): not only adjacent ones.
    let mut seen = std::collections::BTreeSet::new();
    ranked.retain(|(e, sha)| seen.insert((e.class, e.path.clone(), sha.clone())));
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
    let (mut layers, at) = base_layers(mandatory, parts.first(), &index_sha256)?;
    for (class, layer) in [
        (Class::Code, Layer::Task),
        (Class::Knowledge, Layer::Project),
        (Class::Evidence, Layer::Evidence),
    ] {
        layers[layer as usize].insert_str(at[layer as usize], &blocks[class as usize]);
    }
    let prompt = parts.fill(&layers);
    let empty: [String; 4] = Default::default();
    let stats = Stats {
        budget,
        mandatory: required,
        shares: caps,
        code: used[Class::Code as usize],
        evidence: used[Class::Evidence as usize],
        knowledge: used[Class::Knowledge as usize],
        layers: Layers {
            role_contract: estimate(&format!("{}{}", parts.fill(&empty), mandatory.schemas)),
            project: estimate(&layers[Layer::Project as usize]),
            task: estimate(&format!(
                "{}{}",
                layers[Layer::Goal as usize],
                if mandatory.schemas.is_empty() {
                    layers[Layer::Task as usize].clone()
                } else {
                    layers[Layer::Task as usize].replacen(
                        &format!("\n\n{}", mandatory.schemas),
                        "",
                        1,
                    )
                }
            )),
            evidence: estimate(&layers[Layer::Evidence as usize]),
        },
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

/// The schemas `role` hands back, as its contract's output section names them (docs/adr/ADR-026.md decision 3): each
/// a heading line and one line of compact JSON that defines every local `$ref` it uses. A role without a built-in
/// contract hands back a TaskResult.
pub fn output_schemas(role: &str) -> Result<String, String> {
    let names: &[&str] = match role {
        "commander" => &["Message", "DecisionPayload"],
        "qa" => &["TaskResult", "AcceptanceObservation", "IndependentReport"],
        "reviewer" => &["TaskResult", "IndependentReport"],
        _ => &["TaskResult"],
    };
    let source = |path: &str| -> Result<Value, String> {
        let text = crate::objects::RESOURCES
            .iter()
            .find(|(p, _)| *p == path)
            .map(|(_, text)| *text)
            .ok_or_else(|| format!("{path} is not built in"))?;
        serde_json::from_str(text).map_err(|e| format!("{path}: {e}"))
    };
    let protocol = source("contracts/protocol.schema.json")?;
    let mut out = String::new();
    for name in names {
        let (path, schema) = match *name {
            // Only what the agent writes itself, its ```hoh-report block: the bridge fills in the rest, and the contract
            // says a report naming those fields itself is refused (docs/adr/ADR-026.md decision 3, M2T09D-159).
            "IndependentReport" => {
                let full = source("contracts/independent-report.schema.json")?;
                let mine = ["role", "status", "scope", "findings"];
                let properties: serde_json::Map<String, Value> = mine
                    .iter()
                    .filter_map(|k| Some(((*k).to_owned(), full["properties"].get(*k)?.clone())))
                    .collect();
                if properties.len() != mine.len() {
                    return Err(
                        "independent-report.schema.json lacks a hoh-report field".to_owned()
                    );
                }
                (
                    "contracts/independent-report.schema.json 的 hoh-report 块子集".to_owned(),
                    json!({"type": "object", "additionalProperties": false, "required": mine,
                        "properties": properties}),
                )
            }
            "AcceptanceObservation" => (
                "contracts/acceptance-observation.schema.json".to_owned(),
                source("contracts/acceptance-observation.schema.json")?,
            ),
            _ => {
                let defs = protocol
                    .get("$defs")
                    .and_then(Value::as_object)
                    .ok_or("no $defs")?;
                let mut schema = defs
                    .get(*name)
                    .cloned()
                    .ok_or_else(|| format!("no {name}"))?;
                // Every definition it reaches, by name, so the block stands alone.
                let mut reached = std::collections::BTreeMap::new();
                let mut todo = vec![schema.clone()];
                while let Some(node) = todo.pop() {
                    let text = node.to_string();
                    for reference in text.split("\"#/$defs/").skip(1) {
                        let target = reference.split('"').next().unwrap_or_default().to_owned();
                        if let std::collections::btree_map::Entry::Vacant(slot) =
                            reached.entry(target)
                        {
                            let def = defs
                                .get(slot.key())
                                .cloned()
                                .ok_or_else(|| format!("no {}", slot.key()))?;
                            todo.push(def.clone());
                            slot.insert(def);
                        }
                    }
                }
                if !reached.is_empty()
                    && let Some(object) = schema.as_object_mut()
                {
                    object.insert(
                        "$defs".to_owned(),
                        Value::Object(reached.into_iter().collect()),
                    );
                }
                (
                    format!("contracts/protocol.schema.json#/$defs/{name}"),
                    schema,
                )
            }
        };
        out.push_str(&format!("#### 输出 Schema：{name}（{path}）\n{schema}\n"));
    }
    Ok(out)
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

/// The four layers' mandatory text, in Layer order, and where each layer's entries go in it: after its own items
/// (and the lead, in the first layer), before the evidence layer's index pointer. The pointer's length does not
/// depend on the index (a sha256 is always 64 hex digits), so the mandatory estimate is known up front.
fn base_layers(
    mandatory: &Mandatory,
    first: Layer,
    index_sha256: &(impl AsRef<[u8]> + ?Sized),
) -> Result<([String; 4], [usize; 4]), PackError> {
    for (layer, text) in [
        (Layer::Goal, &mandatory.goal),
        (Layer::Project, &mandatory.project),
        (Layer::Task, &mandatory.task),
    ] {
        if text.trim().is_empty() {
            return Err(PackError::EmptyLayer(layer));
        }
    }
    let join = |a: &str, b: &str| match (a.is_empty(), b.is_empty()) {
        (_, true) => a.to_owned(),
        (true, false) => b.to_owned(),
        (false, false) => format!("{a}\n\n{b}"),
    };
    let mut layers = [
        mandatory.goal.clone(),
        mandatory.project.clone(),
        mandatory.task.clone(),
        mandatory.evidence.clone(),
    ];
    let task = Layer::Task as usize;
    layers[task] = join(&layers[task], &mandatory.schemas);
    layers[first as usize] = join(&layers[first as usize], &mandatory.lead);
    let at = [0, 1, 2, 3].map(|l| layers[l].len());
    let sha = String::from_utf8_lossy(index_sha256.as_ref());
    let pointer = format!(
        "引用索引：未装入全文的条目（path 与 sha256，可取回原文）见 .hoh/objects/sha256/{}/{sha}",
        &sha[..2.min(sha.len())]
    );
    // Always on a line of its own after a blank line, whatever precedes it (the layer's items, its last entry).
    let evidence = Layer::Evidence as usize;
    layers[evidence] = format!("{}\n\n{pointer}", layers[evidence]);
    Ok((layers, at))
}

/// A role contract split at its placeholders: either all four, each once, filled in place in the template's own
/// order, or none, the layers then following the contract in a fixed order.
struct Template<'a> {
    /// The text before each placeholder, the placeholder's layer, and the text after the last one.
    pieces: Vec<(&'a str, Layer)>,
    tail: &'a str,
}

impl<'a> Template<'a> {
    /// The layer of the template's first placeholder; the goal's when the layers follow the contract.
    fn first(&self) -> Layer {
        self.pieces.first().map_or(Layer::Goal, |(_, layer)| *layer)
    }

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

mod record;
pub use record::{Contract, Object, Recorded, record, repack};
mod turn;
pub use turn::{CONTEXT_V1, Facts, Params, lead, mandatory};

#[cfg(test)]
mod tests;
