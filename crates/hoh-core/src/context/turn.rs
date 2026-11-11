//! A turn's mandatory items rendered from what the store gathered (docs/adr/ADR-026.md decision 3): pure, so the
//! executor and the tests render the same text from the same facts.

use super::{Entry, Mandatory, output_schemas};

/// The packing parameters a run keeps in its configuration snapshot (docs/adr/ADR-026.md decision 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    pub budget_tokens: u64,
    pub shares: [u64; 3],
}

/// The parameters of a run whose snapshot names none (one started before context packing): frozen, never changed;
/// other defaults would be another constant, for new runs only.
pub const CONTEXT_V1: Params = Params {
    budget_tokens: 32_000,
    shares: super::SHARES,
};

/// What the store gathers for one turn: the values its mandatory items are rendered from, its packing parameters and
/// its candidate entries. The executor adds what only it knows: a first input owed and a Commander's bridge file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    /// The run's goal (its StartRun command); none or blank: not given.
    pub goal: Option<String>,
    /// A Commander turn's question: the wake it answers.
    pub question: Option<String>,
    /// The run's specification: its sha256 and revision.
    pub spec: Option<(String, i64)>,
    /// The scope lines (the task, or the wake's snapshot tasks).
    pub scope: Vec<String>,
    /// The in-scope acceptance criteria, by id, with their text (a Commander turn's: all its snapshot tasks').
    pub acceptance: Vec<(String, String)>,
    /// A scope that has no task (a wake about the run as a whole).
    pub no_task: bool,
    /// The run's current candidate: id, commit, tree.
    pub candidate: Option<(String, String, String)>,
    /// A Commander turn's header: the values its decision names, in order.
    pub header: Vec<(String, String)>,
    pub params: Option<Params>,
    pub entries: Vec<Entry>,
}

impl Facts {
    /// Its packing parameters: the run's, else [`CONTEXT_V1`].
    pub fn params(&self) -> Params {
        self.params.unwrap_or(CONTEXT_V1)
    }
}

/// The mandatory items of `role`'s turn from `facts`, with `lead` (a first input owed, already rendered) and, for a
/// Commander turn, the file its answers go to and the project's root.
pub fn mandatory(
    facts: &Facts,
    role: &str,
    lead: &str,
    bridge: Option<&str>,
    project: Option<&str>,
) -> Result<Mandatory, String> {
    let goal = facts
        .goal
        .as_deref()
        .filter(|g| !g.trim().is_empty())
        .unwrap_or("（未给出）");
    let mut goal = format!("Run 目标：{goal}");
    if let Some(question) = &facts.question {
        goal.push_str(&format!("\n本次唤醒要回答的问题：{question}"));
    }
    let project_text = match &facts.spec {
        Some((sha256, revision)) => format!("spec_sha256：{sha256}\nspec revision：{revision}"),
        None => "spec：尚无 spec".to_owned(),
    };
    let mut task = String::from("#### 作用域\n");
    if facts.no_task {
        task.push_str("本唤醒不涉及具体任务\n");
    }
    for line in &facts.scope {
        task.push_str(&format!("- {line}\n"));
    }
    task.push_str("\n#### 验收标准（in-scope AC，原文）\n");
    if facts.acceptance.is_empty() {
        task.push_str("（无）\n");
    }
    for (id, body) in &facts.acceptance {
        task.push_str(&format!("- {id}：{body}\n"));
    }
    task.push_str("\n#### 候选\n");
    match &facts.candidate {
        Some((id, commit, tree)) => task.push_str(&format!(
            "candidate_id：{id}\ngit_commit：{commit}\ngit_tree：{tree}\n"
        )),
        None => task.push_str("尚无候选\n"),
    }
    if !facts.header.is_empty() || bridge.is_some() {
        task.push_str("\n#### 本次唤醒\n");
        for (key, value) in &facts.header {
            task.push_str(&format!("- {key}：{value}\n"));
        }
        if let Some(bridge) = bridge {
            task.push_str(&format!("- 输出文件：{bridge}\n"));
        }
        if let Some(project) = project {
            task.push_str(&format!("- 项目根：{project}\n"));
        }
    }
    Ok(Mandatory {
        goal,
        project: project_text,
        task: task.trim_end().to_owned(),
        evidence: String::new(),
        lead: lead.to_owned(),
        schemas: output_schemas(role)?.trim_end().to_owned(),
    })
}

/// A first input as its prompt shows it: the whole handover package, never cut (REQ-019 behaviour 4: a new holder's
/// first input is the whole checkpoint package; docs/adr/ADR-026.md decision 3).
pub fn lead(package: &serde_json::Value) -> String {
    let json = serde_json::to_string_pretty(package).unwrap_or_else(|_| package.to_string());
    format!("#### 交接包（本会话的首个输入）\n\n```json\n{json}\n```")
}
