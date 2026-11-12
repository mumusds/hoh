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
    /// A work turn's identity header (docs/adr/ADR-028.md decision 2): the executor sets it, the store does not.
    pub identity: Option<AttemptIdentity>,
    pub params: Option<Params>,
    pub entries: Vec<Entry>,
    /// Registered objects whose text the executor reads into entries ([`Facts::resolve_loads`], docs/adr/ADR-028.md
    /// decision 14): the store names them, it never reads a file.
    pub loads: Vec<Load>,
    /// A candidate's changed files, read by the executor ([`Facts::add_code`], decision 14's code source).
    pub code: Option<CodeSource>,
}

/// A registered object to read into an evidence entry (docs/adr/ADR-028.md decision 14): its digest and the name it
/// was registered under, the entry's path, its label and its rank.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Load {
    pub path: String,
    pub sha256: String,
    pub name: String,
    pub label: String,
    pub relevance: u64,
}

/// At most this much of a loaded object's text is an entry's (decision 14).
pub const LOAD_BYTES: usize = 8 * 1024;

/// The files a candidate changed from `base` to `commit` (decision 14's code source).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeSource {
    pub base: String,
    pub commit: String,
}

/// One changed file: its text at the candidate's commit, or why only its path is listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeFile {
    pub path: String,
    pub text: Result<String, String>,
}

/// At most this many changed files are listed, each read only up to this size (decision 14).
pub const CODE_FILES: usize = 50;
pub const CODE_BYTES: u64 = 64 * 1024;

/// The identity a work attempt's every output must repeat, and where its outputs go (docs/adr/ADR-028.md decision 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptIdentity {
    pub run_id: String,
    pub task_id: String,
    pub attempt_id: String,
    pub role: String,
    pub role_epoch: i64,
    /// Its bridge file: one JSON object per line, its TaskResult and the messages it sends.
    pub bridge: String,
    /// A writer's directory for its self-test records, relative to the project (docs/adr/ADR-028.md decision 14).
    pub self_tests: Option<String>,
    /// A verifier's: what it verifies and its helpers.
    pub verified: Option<Verified>,
}

/// What a verification attempt verifies (docs/adr/ADR-028.md decisions 2 and 7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    /// The development tasks of its round: the tasks it verifies, not its own.
    pub source_task_ids: Vec<String>,
    /// The candidate's identity, field by field: candidate_id and its five digests.
    pub candidate: Vec<(String, String)>,
    /// Its request helpers: `run-check <check_id>` and `ask <message.json>`.
    pub run_check: String,
    pub ask: String,
}

impl Facts {
    /// Its packing parameters: the run's, else [`CONTEXT_V1`].
    pub fn params(&self) -> Params {
        self.params.unwrap_or(CONTEXT_V1)
    }

    /// Its loads read into evidence entries, each object by its digest through `read` (decision 14): UTF-8 text up to
    /// [`LOAD_BYTES`] under its label, the cut marked; text that is not UTF-8, or an object that cannot be read, only
    /// its reference.
    pub fn resolve_loads(&mut self, read: impl Fn(&str) -> Result<Vec<u8>, String>) {
        for load in std::mem::take(&mut self.loads) {
            let head = format!("{}：{} sha256:{}", load.label, load.name, load.sha256);
            let text = match read(&load.sha256).map(String::from_utf8) {
                Ok(Ok(text)) if text.len() <= LOAD_BYTES => format!("{head}\n{text}"),
                Ok(Ok(text)) => {
                    let mut end = LOAD_BYTES;
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    format!(
                        "{head}\n{}\n…（截断：只取前 {LOAD_BYTES} 字节，原文 {} 字节，全文见引用）",
                        &text[..end],
                        text.len()
                    )
                }
                Ok(Err(_)) => format!("{head}（不是 UTF-8 文本，只列引用）"),
                Err(why) => format!("{head}（对象不可读，只列引用：{why}）"),
            };
            self.entries.push(Entry {
                class: super::Class::Evidence,
                kind: super::Kind::Plain,
                path: load.path,
                text,
                summary: None,
                relevance: load.relevance,
                recency: 0,
                causal: 0,
            });
        }
    }

    /// Its code source's files as Code entries (decision 14): `files` as the executor read them (in path order, at
    /// most [`CODE_FILES`]) with how many changed in all, or why none could be read.
    pub fn add_code(&mut self, files: Result<(Vec<CodeFile>, usize), String>) {
        let Some(source) = self.code.take() else {
            return;
        };
        let entry = |path: String, text: String| Entry {
            class: super::Class::Code,
            kind: super::Kind::Plain,
            path,
            text,
            summary: None,
            relevance: 1,
            recency: 0,
            causal: 0,
        };
        let (files, total) = match files {
            Ok(files) => files,
            Err(why) => {
                self.entries.push(entry(
                    "code/unavailable".to_owned(),
                    format!("候选相对 {} 改动的文件不可读：{why}", source.base),
                ));
                return;
            }
        };
        let listed = files.len();
        for file in files {
            let text = match file.text {
                Ok(text) => format!(
                    "改动的文件 {}（候选 commit {} 中的内容）：\n{text}",
                    file.path, source.commit
                ),
                Err(why) => format!("改动的文件 {}（只列路径：{why}）", file.path),
            };
            self.entries
                .push(entry(format!("code/{}", file.path), text));
        }
        if total > listed {
            self.entries.push(entry(
                "code/more".to_owned(),
                format!(
                    "另有 {} 个改动的文件未列出（至多列 {CODE_FILES} 个；基准 {}，候选 commit {}）",
                    total - listed,
                    source.base,
                    source.commit
                ),
            ));
        }
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
    let mut task = String::new();
    if let Some(id) = &facts.identity {
        task.push_str("#### 本次 Attempt（身份头：每个输出的这些字段须与此逐项一致）\n");
        for (key, value) in [
            ("run_id", id.run_id.as_str()),
            ("task_id", id.task_id.as_str()),
            ("attempt_id", id.attempt_id.as_str()),
            ("role", id.role.as_str()),
        ] {
            task.push_str(&format!("- {key}：{value}\n"));
        }
        task.push_str(&format!("- role_epoch：{}\n", id.role_epoch));
        task.push_str(&format!(
            "- 输出文件（TaskResult 与发出的消息，每行一个 JSON 对象）：{}\n",
            id.bridge
        ));
        if let Some(dir) = &id.self_tests {
            task.push_str(&format!(
                "- 自测记录目录（项目相对路径；TaskResult.evidence 只接受指向这里的文件）：{dir}\n"
            ));
        }
        if let Some(v) = &id.verified {
            task.push_str(&format!(
                "- 被验证的任务（不是本 Task）：{}\n",
                v.source_task_ids.join(", ")
            ));
            task.push_str("- 被测候选：\n");
            for (key, value) in &v.candidate {
                task.push_str(&format!("  - {key}：{value}\n"));
            }
            task.push_str(&format!(
                "- 运行检查计划条目：{} <check_id>（以检查的退出码结束，被拒为 2）\n",
                v.run_check
            ));
            task.push_str(&format!(
                "- 发送消息：{} <message.json>（等待回应：rejected 或 deferred）\n",
                v.ask
            ));
        }
        task.push('\n');
    }
    task.push_str("#### 作用域\n");
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
