//! The product configuration (REQ-018, ADR-022): `<project>/.hoh/config.toml`, read over the built-in defaults, which
//! equal `config/hoh.example.toml` key for key. Every key of the table in REQ-018 decision 6 is known; any other key,
//! and any value outside its domain, is refused with its key path and its place in the file, never ignored and never
//! replaced by a default. A missing file is the defaults.

use crate::schedule::{Params, Weights};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use toml::Spanned;
use toml::de::{DeTable, DeValue};

/// Where a project's configuration lives, relative to its root.
pub const FILE: &str = ".hoh/config.toml";

/// The configuration version this release reads.
pub const VERSION: &str = "1.0";

/// The built-in roles, each with a built-in role contract (resources/prompts/).
pub const BUILTIN_ROLES: [&str; 6] = [
    "commander",
    "cto",
    "developer",
    "qa",
    "researcher",
    "reviewer",
];

/// The built-in role contract of `role` (resources/prompts/<role>.md, built into the binary), if it is a built-in role.
pub fn builtin_contract(role: &str) -> Option<&'static str> {
    Some(match role {
        "commander" => include_str!("../../../resources/prompts/commander.md"),
        "cto" => include_str!("../../../resources/prompts/cto.md"),
        "developer" => include_str!("../../../resources/prompts/developer.md"),
        "qa" => include_str!("../../../resources/prompts/qa.md"),
        "researcher" => include_str!("../../../resources/prompts/researcher.md"),
        "reviewer" => include_str!("../../../resources/prompts/reviewer.md"),
        _ => return None,
    })
}

/// A role contract as a turn is sent it (docs/adr/ADR-022.md decision 11): its layer (`builtin` or `project`), its path
/// and its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contract {
    pub layer: &'static str,
    pub path: String,
    pub text: String,
}

/// The contract a turn of `role` follows in the project at `root`, the role's `prompt` as configured: none is the
/// role's own built-in contract, `builtin:<role>` another built-in one, else the override file, resolved as the
/// configuration checked it ([`prompt_file`]) and read now; or why it is not available (never another contract in its
/// place).
pub fn contract(root: &Path, role: &str, prompt: Option<&str>) -> Result<Contract, String> {
    let builtin = |name: &str| {
        builtin_contract(name)
            .map(|text| Contract {
                layer: "builtin",
                path: format!("resources/prompts/{name}.md"),
                text: text.to_owned(),
            })
            .ok_or_else(|| format!("{name} has no built-in role contract"))
    };
    match prompt {
        None => builtin(role),
        Some(prompt) => match prompt.strip_prefix(BUILTIN_PROMPT) {
            Some(name) => builtin(name),
            None => {
                let file = prompt_file(root, prompt)?;
                let text = std::fs::read_to_string(&file)
                    .map_err(|e| format!("\"{prompt}\" cannot be read: {e}"))?;
                Ok(Contract {
                    layer: "project",
                    path: prompt.to_owned(),
                    text,
                })
            }
        },
    }
}

/// The roles a team may not lose (REQ-018 failure mode 3).
pub const REQUIRED_ROLES: [&str; 4] = ["commander", "developer", "qa", "reviewer"];

/// The prefix of a custom role's `prompt` that names a built-in role contract instead of a file (REQ-018 decision 5).
pub const BUILTIN_PROMPT: &str = "builtin:";

/// The native agents HoH drives (the registered adapters).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    Codex,
    Claude,
    Pi,
}

impl Agent {
    /// The agent `name` names, if it is a registered adapter.
    pub fn named(name: &str) -> Option<Agent> {
        match name {
            "codex" => Some(Agent::Codex),
            "claude" => Some(Agent::Claude),
            "pi" => Some(Agent::Pi),
            _ => None,
        }
    }

    /// Its name, as configuration, the store and the executable on PATH name it.
    pub fn as_str(self) -> &'static str {
        match self {
            Agent::Codex => "codex",
            Agent::Claude => "claude",
            Agent::Pi => "pi",
        }
    }

    /// The executable name looked up on PATH.
    pub fn command(self) -> &'static str {
        self.as_str()
    }
}

/// One role's binding: data, not a process (REQ-018).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Role {
    pub agent: Agent,
    /// `None`: the agent's own default model (the attempt records the resolved one).
    pub model: Option<String>,
    /// Agents to fall back to, in order (none: the role is BLOCKED when its agent is unavailable).
    pub fallback: Vec<Agent>,
    /// A role prompt override file, relative to the project root, or `builtin:<role>`.
    pub prompt: Option<String>,
    /// Whether its attempts must not share a session with any other role's (always true for the reviewer).
    pub independent_session: bool,
}

impl Role {
    fn of(agent: Agent) -> Role {
        Role {
            agent,
            model: None,
            fallback: Vec::new(),
            prompt: None,
            independent_session: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Project {
    pub default_mode: String,
    pub document_profile: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Execution {
    pub profile: String,
    pub writer_slots: u32,
    pub reader_slots: u32,
    pub reserve_supervisor_slot: bool,
    pub cancel_native_grace_seconds: u32,
    pub cancel_term_grace_seconds: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Delivery {
    pub max_delivery_attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Persistence {
    pub writer_queue_capacity: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Supervision {
    pub policy: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Budget {
    pub max_duration_hours: f64,
    pub max_iterations: u32,
    pub max_local_repairs: u32,
    pub max_protocol_repairs: u32,
    /// No default: an unset cost cap is no cap (CR-002).
    pub max_usd: Option<f64>,
    /// RFC 3339, in UTC.
    pub deadline_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Ui {
    pub view: String,
    pub statusline: String,
    pub mouse: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Web {
    pub host: String,
    pub port: u16,
}

/// The effective configuration.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Config {
    pub project: Project,
    pub execution: Execution,
    pub delivery: Delivery,
    pub persistence: Persistence,
    pub scheduler: Params,
    /// By role name; the built-in roles always present.
    pub team: BTreeMap<String, Role>,
    pub supervision: Supervision,
    pub budget: Budget,
    pub ui: Ui,
    pub web: Web,
}

impl Default for Config {
    /// The built-in defaults: config/hoh.example.toml, key for key (AC-018-01).
    fn default() -> Config {
        let mut team = BTreeMap::new();
        for (name, agent) in [
            ("commander", Agent::Codex),
            ("cto", Agent::Claude),
            ("developer", Agent::Claude),
            ("qa", Agent::Pi),
            ("researcher", Agent::Pi),
            ("reviewer", Agent::Codex),
        ] {
            team.insert(name.to_owned(), Role::of(agent));
        }
        if let Some(reviewer) = team.get_mut("reviewer") {
            reviewer.independent_session = true;
        }
        Config {
            project: Project {
                default_mode: "AUTO".to_owned(),
                document_profile: "full".to_owned(),
            },
            execution: Execution {
                profile: "yolo-vm".to_owned(),
                writer_slots: 1,
                reader_slots: 2,
                reserve_supervisor_slot: true,
                cancel_native_grace_seconds: 10,
                cancel_term_grace_seconds: 5,
            },
            delivery: Delivery {
                max_delivery_attempts: 5,
            },
            persistence: Persistence {
                writer_queue_capacity: 256,
            },
            scheduler: Params::default(),
            team,
            supervision: Supervision {
                policy: "balanced".to_owned(),
            },
            budget: Budget {
                max_duration_hours: 120.0,
                max_iterations: 100,
                max_local_repairs: 3,
                max_protocol_repairs: 2,
                max_usd: None,
                deadline_at: None,
            },
            ui: Ui {
                view: "workspace".to_owned(),
                statusline: "standard".to_owned(),
                mouse: true,
            },
            web: Web {
                host: "127.0.0.1".to_owned(),
                port: 8787,
            },
        }
    }
}

/// Why a configuration was refused: the file, the key path and the place, when known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub file: Option<PathBuf>,
    /// The key path, as `team.qa.agent` or `team.developer.fallback[1]`.
    pub key: Option<String>,
    /// 1-based.
    pub line: Option<usize>,
    /// 1-based, in characters.
    pub column: Option<usize>,
    pub message: String,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(file) = &self.file {
            write!(f, "{}:", file.display())?;
        }
        if let Some(line) = self.line {
            write!(f, "{line}:")?;
            if let Some(column) = self.column {
                write!(f, "{column}:")?;
            }
        }
        if self.file.is_some() || self.line.is_some() {
            f.write_str(" ")?;
        }
        if let Some(key) = &self.key {
            write!(f, "{key}: ")?;
        }
        f.write_str(&self.message)
    }
}

impl std::error::Error for ConfigError {}

/// The effective configuration of the project at `root`: its `.hoh/config.toml` over the defaults, or the defaults
/// when there is none.
pub fn load(root: &Path) -> Result<Config, ConfigError> {
    load_file(root).map(|(config, _)| config)
}

/// [`load`], with the file's text as it was read (none without a file): what the configuration came from.
pub fn load_file(root: &Path) -> Result<(Config, Option<String>), ConfigError> {
    let file = root.join(FILE);
    let failed = |message: String| ConfigError {
        file: Some(file.clone()),
        key: None,
        line: None,
        column: None,
        message,
    };
    let meta = match std::fs::metadata(&file) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Config::default(), None)),
        Err(e) => return Err(failed(e.to_string())),
    };
    if !meta.is_file() {
        return Err(failed("not a regular file".to_owned()));
    }
    let text = std::fs::read_to_string(&file).map_err(|e| failed(e.to_string()))?;
    let config = parse(&text).map_err(|mut error| {
        error.file = Some(file.clone());
        error
    })?;
    for (name, role) in &config.team {
        if let Some(prompt) = &role.prompt {
            check_prompt(root, &format!("team.{name}.prompt"), prompt).map_err(|mut error| {
                error.file = Some(file.clone());
                error
            })?;
        }
    }
    Ok((config, Some(text)))
}

/// The effective configuration `text` gives over the defaults.
pub fn parse(text: &str) -> Result<Config, ConfigError> {
    let table = DeTable::parse(text).map_err(|e| {
        let (line, column) = e
            .span()
            .map(|span| place(text, span.start))
            .map_or((None, None), |(l, c)| (Some(l), Some(c)));
        ConfigError {
            file: None,
            key: None,
            line,
            column,
            message: e.message().trim().to_owned(),
        }
    })?;
    let top = from_toml_table(table.get_ref());
    let reader = Reader { text: Some(text) };
    let mut config = Config::default();
    reader.read(&top, &mut config)?;
    Ok(config)
}

/// The team `base` becomes with ConfigureTeam's `bindings`: an RFC 7396 merge patch over the team as JSON (`role:
/// null` removes the role, `key: null` gives the key its default), checked whole by the file's rules, with key paths
/// under `bindings` (docs/adr/ADR-022.md decisions 2 and 4).
pub fn patch_team(
    base: &BTreeMap<String, Role>,
    bindings: &serde_json::Value,
) -> Result<BTreeMap<String, Role>, ConfigError> {
    let reader = Reader { text: None };
    let serde_json::Value::Object(patch) = bindings else {
        return Err(reader.error("bindings", None, "expected an object of roles"));
    };
    let mut merged: BTreeMap<String, serde_json::Map<String, serde_json::Value>> = base
        .iter()
        .map(|(name, role)| (name.clone(), role_json(role)))
        .collect();
    for (name, change) in patch {
        match change {
            serde_json::Value::Null => {
                merged.remove(name);
            }
            serde_json::Value::Object(keys) => {
                let role = merged.entry(name.clone()).or_default();
                for (key, value) in keys {
                    if value.is_null() {
                        role.remove(key);
                    } else {
                        role.insert(key.clone(), value.clone());
                    }
                }
            }
            _ => {
                return Err(reader.error(
                    &format!("bindings.{name}"),
                    None,
                    "expected an object of the role's keys, or null to remove the role",
                ));
            }
        }
    }
    let mut team = Config::default().team;
    for (name, keys) in &merged {
        let table: Vec<Entry> = keys
            .iter()
            .map(|(key, value)| (key.clone(), None, from_json(value)))
            .collect();
        reader.role(name, &format!("bindings.{name}"), None, &table, &mut team)?;
    }
    team.retain(|name, _| merged.contains_key(name));
    if let Some(missing) = REQUIRED_ROLES
        .iter()
        .find(|role| !team.contains_key(**role))
    {
        return Err(reader.error(
            &format!("bindings.{missing}"),
            None,
            "a required role cannot be removed (commander, developer, qa and reviewer)",
        ));
    }
    Ok(team)
}

/// A role as the merge patch sees it: its keys, an unset model or prompt and an empty fallback left out.
fn role_json(role: &Role) -> serde_json::Map<String, serde_json::Value> {
    let mut keys = serde_json::Map::new();
    keys.insert("agent".to_owned(), role.agent.as_str().into());
    if let Some(model) = &role.model {
        keys.insert("model".to_owned(), model.clone().into());
    }
    if !role.fallback.is_empty() {
        keys.insert(
            "fallback".to_owned(),
            role.fallback.iter().map(|a| a.as_str()).collect(),
        );
    }
    if let Some(prompt) = &role.prompt {
        keys.insert("prompt".to_owned(), prompt.clone().into());
    }
    keys.insert(
        "independent_session".to_owned(),
        role.independent_session.into(),
    );
    keys
}

/// The 1-based line and column (in characters) of byte `at` of `text`.
fn place(text: &str, at: usize) -> (usize, usize) {
    let before = &text[..at.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let start = before.rfind('\n').map_or(0, |i| i + 1);
    (line, before[start..].chars().count() + 1)
}

/// A configuration value, and where it starts in the file when it came from one (a ConfigureTeam's has no place).
#[derive(Debug, Clone)]
struct Node {
    at: Option<usize>,
    item: Item,
}

#[derive(Debug, Clone)]
enum Item {
    Str(String),
    /// `None`: an integer out of the 64-bit range.
    Int(Option<i64>),
    Float(f64),
    Bool(bool),
    Datetime(String),
    Array(Vec<Node>),
    Table(Vec<Entry>),
}

/// A table's key, where the key starts, and its value.
type Entry = (String, Option<usize>, Node);

fn from_toml(value: &Spanned<DeValue<'_>>) -> Node {
    let item = match value.get_ref() {
        DeValue::String(s) => Item::Str(s.to_string()),
        DeValue::Integer(i) => Item::Int(i64::from_str_radix(i.as_str(), i.radix()).ok()),
        DeValue::Float(f) => Item::Float(f.as_str().parse().unwrap_or(f64::NAN)),
        DeValue::Boolean(b) => Item::Bool(*b),
        DeValue::Datetime(d) => Item::Datetime(d.to_string()),
        DeValue::Array(items) => Item::Array(items.iter().map(from_toml).collect()),
        DeValue::Table(table) => Item::Table(from_toml_table(table)),
    };
    Node {
        at: Some(value.span().start),
        item,
    }
}

fn from_toml_table(table: &DeTable<'_>) -> Vec<Entry> {
    table
        .iter()
        .map(|(key, value)| {
            (
                key.get_ref().to_string(),
                Some(key.span().start),
                from_toml(value),
            )
        })
        .collect()
}

/// A JSON value as a configuration value (a null is a key left out: the merge patch removed them).
fn from_json(value: &serde_json::Value) -> Node {
    let item = match value {
        serde_json::Value::String(s) => Item::Str(s.clone()),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(i) => Item::Int(Some(i)),
            None if n.is_u64() => Item::Int(None),
            None => Item::Float(n.as_f64().unwrap_or(f64::NAN)),
        },
        serde_json::Value::Bool(b) => Item::Bool(*b),
        serde_json::Value::Array(items) => Item::Array(items.iter().map(from_json).collect()),
        serde_json::Value::Object(map) => Item::Table(
            map.iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k.clone(), None, from_json(v)))
                .collect(),
        ),
        serde_json::Value::Null => Item::Table(Vec::new()),
    };
    Node { at: None, item }
}

struct Reader<'t> {
    /// The file's text, for lines and columns.
    text: Option<&'t str>,
}

impl Reader<'_> {
    fn error(&self, key: &str, at: Option<usize>, message: impl Into<String>) -> ConfigError {
        let (line, column) = match (self.text, at) {
            (Some(text), Some(at)) => {
                let (line, column) = place(text, at);
                (Some(line), Some(column))
            }
            _ => (None, None),
        };
        ConfigError {
            file: None,
            key: Some(key.to_owned()),
            line,
            column,
            message: message.into(),
        }
    }

    fn unknown(&self, key: &str, at: Option<usize>) -> ConfigError {
        self.error(key, at, "unknown key (REQ-018: unknown keys are refused)")
    }

    fn read(&self, top: &[Entry], config: &mut Config) -> Result<(), ConfigError> {
        match top.iter().find(|(key, _, _)| key == "config_version") {
            None => {
                return Err(ConfigError {
                    file: None,
                    key: Some("config_version".to_owned()),
                    line: Some(1),
                    column: Some(1),
                    message: format!("missing; this release reads config_version = \"{VERSION}\""),
                });
            }
            Some((_, _, value)) => {
                let version = self.string("config_version", value)?;
                if version != VERSION {
                    return Err(self.error(
                        "config_version",
                        value.at,
                        format!(
                            "\"{version}\" is not read by this release (it reads \"{VERSION}\")"
                        ),
                    ));
                }
            }
        }
        for (name, at, value) in top {
            match name.as_str() {
                "config_version" => {}
                "project" => self.project(self.table(name, value)?, &mut config.project)?,
                "execution" => self.execution(self.table(name, value)?, &mut config.execution)?,
                "delivery" => {
                    for (key, at, value) in self.table(name, value)? {
                        match key.as_str() {
                            "max_delivery_attempts" => {
                                config.delivery.max_delivery_attempts = self.count(
                                    "delivery.max_delivery_attempts",
                                    value,
                                    1,
                                    u32::MAX,
                                )?;
                            }
                            other => return Err(self.unknown(&format!("delivery.{other}"), *at)),
                        }
                    }
                }
                "persistence" => {
                    for (key, at, value) in self.table(name, value)? {
                        match key.as_str() {
                            "writer_queue_capacity" => {
                                config.persistence.writer_queue_capacity = self.count(
                                    "persistence.writer_queue_capacity",
                                    value,
                                    1,
                                    u32::MAX,
                                )?;
                            }
                            other => {
                                return Err(self.unknown(&format!("persistence.{other}"), *at));
                            }
                        }
                    }
                }
                "scheduler" => self.scheduler(self.table(name, value)?, &mut config.scheduler)?,
                "team" => {
                    for (role, at, value) in self.table(name, value)? {
                        let path = format!("team.{role}");
                        let table = self.table(&path, value)?;
                        self.role(role, &path, *at, table, &mut config.team)?;
                    }
                }
                "supervision" => {
                    for (key, at, value) in self.table(name, value)? {
                        match key.as_str() {
                            "policy" => {
                                config.supervision.policy = self.one_of(
                                    "supervision.policy",
                                    value,
                                    &["efficient", "balanced", "intensive"],
                                )?;
                            }
                            other => {
                                return Err(self.unknown(&format!("supervision.{other}"), *at));
                            }
                        }
                    }
                }
                "budget" => self.budget(self.table(name, value)?, &mut config.budget)?,
                "ui" => self.ui(self.table(name, value)?, &mut config.ui)?,
                "web" => self.web(self.table(name, value)?, &mut config.web)?,
                other => return Err(self.unknown(other, *at)),
            }
        }
        Ok(())
    }

    fn project(&self, table: &[Entry], project: &mut Project) -> Result<(), ConfigError> {
        for (key, at, value) in table {
            match key.as_str() {
                "default_mode" => {
                    project.default_mode =
                        self.one_of("project.default_mode", value, &["PLAN", "BUILD", "AUTO"])?;
                }
                // REQ-029: the document profiles by project size.
                "document_profile" => {
                    project.document_profile = self.one_of(
                        "project.document_profile",
                        value,
                        &["lite", "standard", "full"],
                    )?;
                }
                other => return Err(self.unknown(&format!("project.{other}"), *at)),
            }
        }
        Ok(())
    }

    fn execution(&self, table: &[Entry], execution: &mut Execution) -> Result<(), ConfigError> {
        for (name, at, value) in table {
            let path = format!("execution.{name}");
            match name.as_str() {
                // The one execution profile HoH has (CONTRACT: YOLO inside the VM).
                "profile" => execution.profile = self.one_of(&path, value, &["yolo-vm"])?,
                // At most one code writer per repository (REQ-037).
                "writer_slots" => execution.writer_slots = self.count(&path, value, 1, 1)?,
                "reader_slots" => execution.reader_slots = self.count(&path, value, 1, u32::MAX)?,
                "reserve_supervisor_slot" => {
                    execution.reserve_supervisor_slot = self.boolean(&path, value)?;
                }
                "cancel_native_grace_seconds" => {
                    execution.cancel_native_grace_seconds =
                        self.count(&path, value, 1, u32::MAX)?;
                }
                "cancel_term_grace_seconds" => {
                    execution.cancel_term_grace_seconds = self.count(&path, value, 1, u32::MAX)?;
                }
                _ => return Err(self.unknown(&path, *at)),
            }
        }
        Ok(())
    }

    fn scheduler(&self, table: &[Entry], params: &mut Params) -> Result<(), ConfigError> {
        for (key, at, value) in table {
            match key.as_str() {
                // contracts/protocol.schema.json SchedulerSettings: 1..=1000, each weight 0..=10.
                "max_wait_rounds" => {
                    params.max_wait_rounds =
                        self.count("scheduler.max_wait_rounds", value, 1, 1000)?;
                }
                "weights" => {
                    let Weights {
                        user_value,
                        unblock_value,
                        risk_reduction,
                        information_gain,
                    } = &mut params.weights;
                    for (name, at, value) in self.table("scheduler.weights", value)? {
                        let path = format!("scheduler.weights.{name}");
                        let slot = match name.as_str() {
                            "user_value" => &mut *user_value,
                            "unblock_value" => &mut *unblock_value,
                            "risk_reduction" => &mut *risk_reduction,
                            "information_gain" => &mut *information_gain,
                            _ => return Err(self.unknown(&path, *at)),
                        };
                        *slot = self.count(&path, value, 0, 10)?;
                    }
                }
                other => return Err(self.unknown(&format!("scheduler.{other}"), *at)),
            }
        }
        Ok(())
    }

    fn role(
        &self,
        name: &str,
        path: &str,
        at: Option<usize>,
        table: &[Entry],
        team: &mut BTreeMap<String, Role>,
    ) -> Result<(), ConfigError> {
        let builtin = BUILTIN_ROLES.contains(&name);
        if !builtin && !role_name(name) {
            return Err(self.error(
                path,
                at,
                "a custom role's name is a lower-case letter, then up to 63 lower-case letters, digits or '_'",
            ));
        }
        // A built-in role's keys left out keep its default; a custom role starts from nothing.
        let defaults = Config::default().team;
        let mut agent = defaults.get(name).map(|role| role.agent);
        let mut role = defaults
            .get(name)
            .cloned()
            .unwrap_or_else(|| Role::of(Agent::Codex));
        if let Some(current) = team.get(name) {
            agent = Some(current.agent);
            role = current.clone();
        }
        for (key_name, key_at, value) in table {
            let key_path = format!("{path}.{key_name}");
            match key_name.as_str() {
                "agent" => agent = Some(self.agent(&key_path, value)?),
                "model" => {
                    let model = self.string(&key_path, value)?;
                    role.model = (!model.is_empty()).then_some(model);
                }
                "fallback" => {
                    let Item::Array(items) = &value.item else {
                        return Err(self.error(
                            &key_path,
                            value.at,
                            "expected an array of agent names",
                        ));
                    };
                    let mut fallback = Vec::new();
                    for (i, item) in items.iter().enumerate() {
                        let item_path = format!("{key_path}[{i}]");
                        let agent = self.agent(&item_path, item)?;
                        if fallback.contains(&agent) {
                            return Err(self.error(
                                &item_path,
                                item.at,
                                format!("\"{}\" is already in the list", agent.as_str()),
                            ));
                        }
                        fallback.push(agent);
                    }
                    role.fallback = fallback;
                }
                "prompt" => {
                    let prompt = self.string(&key_path, value)?;
                    if prompt.is_empty() {
                        return Err(self.error(&key_path, value.at, "an empty path"));
                    }
                    if let Some(contract) = prompt.strip_prefix(BUILTIN_PROMPT)
                        && !BUILTIN_ROLES.contains(&contract)
                    {
                        return Err(self.error(
                            &key_path,
                            value.at,
                            format!("no built-in role contract named \"{contract}\""),
                        ));
                    }
                    role.prompt = Some(prompt);
                }
                "independent_session" => {
                    let independent = self.boolean(&key_path, value)?;
                    // CONTRACT 7: an independent review is a hard rule, not an option (REQ-018 decision 4).
                    if name == "reviewer" && !independent {
                        return Err(self.error(
                            &key_path,
                            value.at,
                            "the reviewer's session is always independent (CONTRACT 7)",
                        ));
                    }
                    role.independent_session = independent;
                }
                _ => return Err(self.unknown(&key_path, *key_at)),
            }
        }
        let Some(agent) = agent else {
            return Err(self.error(&format!("{path}.agent"), at, "a custom role needs an agent"));
        };
        if !builtin && role.prompt.is_none() {
            return Err(self.error(
                &format!("{path}.prompt"),
                at,
                "a custom role needs a prompt override file or a built-in role contract (\"builtin:<role>\")",
            ));
        }
        // A fallback to the role's own agent is no fallback (M2T07R-60).
        if let Some(at) = role.fallback.iter().position(|a| *a == agent) {
            return Err(self.error(
                &format!("{path}.fallback[{at}]"),
                at_of(table, "fallback"),
                format!("\"{}\" is the role's own agent", agent.as_str()),
            ));
        }
        role.agent = agent;
        team.insert(name.to_owned(), role);
        Ok(())
    }

    fn budget(&self, table: &[Entry], budget: &mut Budget) -> Result<(), ConfigError> {
        for (name, at, value) in table {
            let path = format!("budget.{name}");
            match name.as_str() {
                "max_duration_hours" => budget.max_duration_hours = self.positive(&path, value)?,
                "max_iterations" => {
                    budget.max_iterations = self.count(&path, value, 1, u32::MAX)?
                }
                "max_local_repairs" => {
                    budget.max_local_repairs = self.count(&path, value, 0, u32::MAX)?;
                }
                "max_protocol_repairs" => {
                    budget.max_protocol_repairs = self.count(&path, value, 0, u32::MAX)?;
                }
                "max_usd" => budget.max_usd = Some(self.positive(&path, value)?),
                "deadline_at" => {
                    let text = match &value.item {
                        Item::Str(s) | Item::Datetime(s) => s.clone(),
                        _ => {
                            return Err(self.error(
                                &path,
                                value.at,
                                "expected an RFC 3339 date-time",
                            ));
                        }
                    };
                    let Some(instant) = crate::time::parse_rfc3339(&text) else {
                        return Err(self.error(
                            &path,
                            value.at,
                            format!("\"{text}\" is not an RFC 3339 date-time"),
                        ));
                    };
                    budget.deadline_at = Some(crate::time::rfc3339_utc(instant));
                }
                _ => return Err(self.unknown(&path, *at)),
            }
        }
        Ok(())
    }

    fn ui(&self, table: &[Entry], ui: &mut Ui) -> Result<(), ConfigError> {
        for (name, at, value) in table {
            let path = format!("ui.{name}");
            match name.as_str() {
                // CONTRACT 8: Workspace by default, Team and Focus optional.
                "view" => ui.view = self.one_of(&path, value, &["workspace", "team", "focus"])?,
                "statusline" => ui.statusline = self.one_of(&path, value, &["standard"])?,
                "mouse" => ui.mouse = self.boolean(&path, value)?,
                _ => return Err(self.unknown(&path, *at)),
            }
        }
        Ok(())
    }

    fn web(&self, table: &[Entry], web: &mut Web) -> Result<(), ConfigError> {
        for (name, at, value) in table {
            let path = format!("web.{name}");
            match name.as_str() {
                "host" => {
                    let host = self.string(&path, value)?;
                    let loopback = host == "localhost"
                        || host
                            .parse::<std::net::IpAddr>()
                            .is_ok_and(|ip| ip.is_loopback());
                    if !loopback {
                        return Err(self.error(
                            &path,
                            value.at,
                            format!(
                                "\"{host}\" is not a loopback address (remote use goes through ssh -L)"
                            ),
                        ));
                    }
                    web.host = host;
                }
                "port" => {
                    web.port = u16::try_from(self.count(&path, value, 1, u32::from(u16::MAX))?)
                        .unwrap_or(u16::MAX);
                }
                _ => return Err(self.unknown(&path, *at)),
            }
        }
        Ok(())
    }

    fn table<'n>(&self, path: &str, value: &'n Node) -> Result<&'n [Entry], ConfigError> {
        match &value.item {
            Item::Table(table) => Ok(table),
            _ => Err(self.error(path, value.at, "expected a table")),
        }
    }

    fn string(&self, path: &str, value: &Node) -> Result<String, ConfigError> {
        match &value.item {
            Item::Str(s) => Ok(s.clone()),
            _ => Err(self.error(path, value.at, "expected a string")),
        }
    }

    fn one_of(&self, path: &str, value: &Node, allowed: &[&str]) -> Result<String, ConfigError> {
        let text = self.string(path, value)?;
        if allowed.contains(&text.as_str()) {
            Ok(text)
        } else {
            Err(self.error(
                path,
                value.at,
                format!("\"{text}\" is not one of {}", allowed.join(", ")),
            ))
        }
    }

    fn agent(&self, path: &str, value: &Node) -> Result<Agent, ConfigError> {
        let name = self.string(path, value)?;
        Agent::named(&name).ok_or_else(|| {
            self.error(
                path,
                value.at,
                format!("\"{name}\" is not a registered agent (codex, claude, pi)"),
            )
        })
    }

    fn boolean(&self, path: &str, value: &Node) -> Result<bool, ConfigError> {
        match &value.item {
            Item::Bool(b) => Ok(*b),
            _ => Err(self.error(path, value.at, "expected true or false")),
        }
    }

    /// An integer within `min..=max`.
    fn count(&self, path: &str, value: &Node, min: u32, max: u32) -> Result<u32, ConfigError> {
        let Item::Int(integer) = &value.item else {
            return Err(self.error(path, value.at, "expected an integer"));
        };
        integer
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| (min..=max).contains(n))
            .ok_or_else(|| {
                self.error(
                    path,
                    value.at,
                    if max == u32::MAX {
                        format!("expected an integer of at least {min}")
                    } else if min == max {
                        format!("must be {min}")
                    } else {
                        format!("expected an integer from {min} to {max}")
                    },
                )
            })
    }

    /// A number above zero, integer or float.
    fn positive(&self, path: &str, value: &Node) -> Result<f64, ConfigError> {
        let number = match &value.item {
            Item::Int(Some(n)) => Some(*n as f64),
            Item::Float(f) => Some(*f),
            _ => None,
        };
        number
            .filter(|n| n.is_finite() && *n > 0.0)
            .ok_or_else(|| self.error(path, value.at, "expected a number above 0"))
    }
}

/// Where key `key` of `table` starts, if it is there and from a file.
fn at_of(table: &[Entry], key: &str) -> Option<usize> {
    table
        .iter()
        .find(|(name, _, _)| name == key)
        .and_then(|(_, _, value)| value.at)
}

/// Check a role's `prompt` against the project at `root` (docs/adr/ADR-022.md decision 11, M2T07R-53): a built-in role
/// contract, or a relative path inside the project to a readable regular file. `key` is its key path for the error.
pub fn check_prompt(root: &Path, key: &str, prompt: &str) -> Result<(), ConfigError> {
    if prompt.starts_with(BUILTIN_PROMPT) {
        return Ok(());
    }
    prompt_file(root, prompt)
        .and_then(|path| {
            std::fs::File::open(&path)
                .map(drop)
                .map_err(|e| format!("\"{prompt}\" cannot be read: {e}"))
        })
        .map_err(|message| ConfigError {
            file: None,
            key: Some(key.to_owned()),
            line: None,
            column: None,
            message,
        })
}

/// The regular file an override `prompt` names in the project at `root`, every symbolic link on its way followed: a
/// relative path without `..` whose file is inside the project once both are resolved (M2T07R-101), or why not.
pub fn prompt_file(root: &Path, prompt: &str) -> Result<std::path::PathBuf, String> {
    let path = Path::new(prompt);
    if path.is_absolute()
        || path.components().any(|c| {
            !matches!(
                c,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
    {
        return Err(format!(
            "\"{prompt}\" is not a path inside the project (relative, without ..)"
        ));
    }
    let root = std::fs::canonicalize(root)
        .map_err(|e| format!("the project {} cannot be resolved: {e}", root.display()))?;
    let full = std::fs::canonicalize(root.join(path)).map_err(|e| format!("\"{prompt}\": {e}"))?;
    if !full.starts_with(&root) {
        return Err(format!(
            "\"{prompt}\" leads outside the project (to {})",
            full.display()
        ));
    }
    match std::fs::metadata(&full) {
        Ok(meta) if meta.is_file() => Ok(full),
        Ok(_) => Err(format!("\"{prompt}\" is not a regular file")),
        Err(e) => Err(format!("\"{prompt}\": {e}")),
    }
}

/// Whether `name` may name a custom role.
fn role_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && name.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests;
