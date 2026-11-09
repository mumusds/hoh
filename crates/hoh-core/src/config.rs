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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(e) => return Err(failed(e.to_string())),
    };
    if !meta.is_file() {
        return Err(failed("not a regular file".to_owned()));
    }
    let text = std::fs::read_to_string(&file).map_err(|e| failed(e.to_string()))?;
    parse(&text).map_err(|mut error| {
        error.file = Some(file.clone());
        error
    })
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
    let reader = Reader { text };
    let mut config = Config::default();
    reader.read(table.get_ref(), &mut config)?;
    Ok(config)
}

/// The 1-based line and column (in characters) of byte `at` of `text`.
fn place(text: &str, at: usize) -> (usize, usize) {
    let before = &text[..at.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let start = before.rfind('\n').map_or(0, |i| i + 1);
    (line, before[start..].chars().count() + 1)
}

type Value<'i> = Spanned<DeValue<'i>>;

struct Reader<'t> {
    text: &'t str,
}

impl Reader<'_> {
    fn error(&self, key: &str, at: usize, message: impl Into<String>) -> ConfigError {
        let (line, column) = place(self.text, at);
        ConfigError {
            file: None,
            key: Some(key.to_owned()),
            line: Some(line),
            column: Some(column),
            message: message.into(),
        }
    }

    fn unknown(&self, key: &str, at: usize) -> ConfigError {
        self.error(key, at, "unknown key (REQ-018: unknown keys are refused)")
    }

    fn read(&self, top: &DeTable<'_>, config: &mut Config) -> Result<(), ConfigError> {
        match top.get("config_version") {
            None => {
                return Err(ConfigError {
                    file: None,
                    key: Some("config_version".to_owned()),
                    line: Some(1),
                    column: Some(1),
                    message: format!("missing; this release reads config_version = \"{VERSION}\""),
                });
            }
            Some(value) => {
                let version = self.string("config_version", value)?;
                if version != VERSION {
                    return Err(self.error(
                        "config_version",
                        value.span().start,
                        format!(
                            "\"{version}\" is not read by this release (it reads \"{VERSION}\")"
                        ),
                    ));
                }
            }
        }
        for (key, value) in top {
            let name = &**key.get_ref();
            match name {
                "config_version" => {}
                "project" => self.project(self.table(name, value)?, &mut config.project)?,
                "execution" => self.execution(self.table(name, value)?, &mut config.execution)?,
                "delivery" => {
                    for (key, value) in self.table(name, value)? {
                        match &**key.get_ref() {
                            "max_delivery_attempts" => {
                                config.delivery.max_delivery_attempts = self.count(
                                    "delivery.max_delivery_attempts",
                                    value,
                                    1,
                                    u32::MAX,
                                )?;
                            }
                            other => {
                                return Err(
                                    self.unknown(&format!("delivery.{other}"), key.span().start)
                                );
                            }
                        }
                    }
                }
                "persistence" => {
                    for (key, value) in self.table(name, value)? {
                        match &**key.get_ref() {
                            "writer_queue_capacity" => {
                                config.persistence.writer_queue_capacity = self.count(
                                    "persistence.writer_queue_capacity",
                                    value,
                                    1,
                                    u32::MAX,
                                )?;
                            }
                            other => {
                                return Err(
                                    self.unknown(&format!("persistence.{other}"), key.span().start)
                                );
                            }
                        }
                    }
                }
                "scheduler" => self.scheduler(self.table(name, value)?, &mut config.scheduler)?,
                "team" => {
                    for (key, value) in self.table(name, value)? {
                        let role = &**key.get_ref();
                        let path = format!("team.{role}");
                        let table = self.table(&path, value)?;
                        self.role(role, &path, key.span().start, table, &mut config.team)?;
                    }
                }
                "supervision" => {
                    for (key, value) in self.table(name, value)? {
                        match &**key.get_ref() {
                            "policy" => {
                                config.supervision.policy = self.one_of(
                                    "supervision.policy",
                                    value,
                                    &["efficient", "balanced", "intensive"],
                                )?;
                            }
                            other => {
                                return Err(
                                    self.unknown(&format!("supervision.{other}"), key.span().start)
                                );
                            }
                        }
                    }
                }
                "budget" => self.budget(self.table(name, value)?, &mut config.budget)?,
                "ui" => self.ui(self.table(name, value)?, &mut config.ui)?,
                "web" => self.web(self.table(name, value)?, &mut config.web)?,
                other => return Err(self.unknown(other, key.span().start)),
            }
        }
        Ok(())
    }

    fn project(&self, table: &DeTable<'_>, project: &mut Project) -> Result<(), ConfigError> {
        for (key, value) in table {
            match &**key.get_ref() {
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
                other => return Err(self.unknown(&format!("project.{other}"), key.span().start)),
            }
        }
        Ok(())
    }

    fn execution(&self, table: &DeTable<'_>, execution: &mut Execution) -> Result<(), ConfigError> {
        for (key, value) in table {
            let name = &**key.get_ref();
            let path = format!("execution.{name}");
            match name {
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
                _ => return Err(self.unknown(&path, key.span().start)),
            }
        }
        Ok(())
    }

    fn scheduler(&self, table: &DeTable<'_>, params: &mut Params) -> Result<(), ConfigError> {
        for (key, value) in table {
            match &**key.get_ref() {
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
                    for (key, value) in self.table("scheduler.weights", value)? {
                        let name = &**key.get_ref();
                        let path = format!("scheduler.weights.{name}");
                        let slot = match name {
                            "user_value" => &mut *user_value,
                            "unblock_value" => &mut *unblock_value,
                            "risk_reduction" => &mut *risk_reduction,
                            "information_gain" => &mut *information_gain,
                            _ => return Err(self.unknown(&path, key.span().start)),
                        };
                        *slot = self.count(&path, value, 0, 10)?;
                    }
                }
                other => return Err(self.unknown(&format!("scheduler.{other}"), key.span().start)),
            }
        }
        Ok(())
    }

    fn role(
        &self,
        name: &str,
        path: &str,
        at: usize,
        table: &DeTable<'_>,
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
        let mut agent = team.get(name).map(|role| role.agent);
        let mut role = team
            .get(name)
            .cloned()
            .unwrap_or_else(|| Role::of(Agent::Codex));
        for (key, value) in table {
            let key_name = &**key.get_ref();
            let key_path = format!("{path}.{key_name}");
            match key_name {
                "agent" => agent = Some(self.agent(&key_path, value)?),
                "model" => {
                    let model = self.string(&key_path, value)?;
                    role.model = (!model.is_empty()).then_some(model);
                }
                "fallback" => {
                    let DeValue::Array(items) = value.get_ref() else {
                        return Err(self.error(
                            &key_path,
                            value.span().start,
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
                                item.span().start,
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
                        return Err(self.error(&key_path, value.span().start, "an empty path"));
                    }
                    if let Some(contract) = prompt.strip_prefix(BUILTIN_PROMPT)
                        && !BUILTIN_ROLES.contains(&contract)
                    {
                        return Err(self.error(
                            &key_path,
                            value.span().start,
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
                            value.span().start,
                            "the reviewer's session is always independent (CONTRACT 7)",
                        ));
                    }
                    role.independent_session = independent;
                }
                _ => return Err(self.unknown(&key_path, key.span().start)),
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
        role.agent = agent;
        team.insert(name.to_owned(), role);
        Ok(())
    }

    fn budget(&self, table: &DeTable<'_>, budget: &mut Budget) -> Result<(), ConfigError> {
        for (key, value) in table {
            let name = &**key.get_ref();
            let path = format!("budget.{name}");
            match name {
                "max_duration_hours" => budget.max_duration_hours = self.positive(&path, value)?,
                "max_iterations" => {
                    budget.max_iterations = self.count(&path, value, 1, u32::MAX)?
                }
                "max_local_repairs" => {
                    budget.max_local_repairs = self.count(&path, value, 0, u32::MAX)?
                }
                "max_protocol_repairs" => {
                    budget.max_protocol_repairs = self.count(&path, value, 0, u32::MAX)?;
                }
                "max_usd" => budget.max_usd = Some(self.positive(&path, value)?),
                "deadline_at" => {
                    let text = match value.get_ref() {
                        DeValue::String(s) => s.to_string(),
                        DeValue::Datetime(d) => d.to_string(),
                        _ => {
                            return Err(self.error(
                                &path,
                                value.span().start,
                                "expected an RFC 3339 date-time",
                            ));
                        }
                    };
                    let Some(at) = crate::time::parse_rfc3339(&text) else {
                        return Err(self.error(
                            &path,
                            value.span().start,
                            format!("\"{text}\" is not an RFC 3339 date-time"),
                        ));
                    };
                    budget.deadline_at = Some(crate::time::rfc3339_utc(at));
                }
                _ => return Err(self.unknown(&path, key.span().start)),
            }
        }
        Ok(())
    }

    fn ui(&self, table: &DeTable<'_>, ui: &mut Ui) -> Result<(), ConfigError> {
        for (key, value) in table {
            let name = &**key.get_ref();
            let path = format!("ui.{name}");
            match name {
                // CONTRACT 8: Workspace by default, Team and Focus optional.
                "view" => ui.view = self.one_of(&path, value, &["workspace", "team", "focus"])?,
                "statusline" => ui.statusline = self.one_of(&path, value, &["standard"])?,
                "mouse" => ui.mouse = self.boolean(&path, value)?,
                _ => return Err(self.unknown(&path, key.span().start)),
            }
        }
        Ok(())
    }

    fn web(&self, table: &DeTable<'_>, web: &mut Web) -> Result<(), ConfigError> {
        for (key, value) in table {
            let name = &**key.get_ref();
            let path = format!("web.{name}");
            match name {
                "host" => {
                    let host = self.string(&path, value)?;
                    let loopback = host == "localhost"
                        || host
                            .parse::<std::net::IpAddr>()
                            .is_ok_and(|ip| ip.is_loopback());
                    if !loopback {
                        return Err(self.error(
                            &path,
                            value.span().start,
                            format!("\"{host}\" is not a loopback address (remote use goes through ssh -L)"),
                        ));
                    }
                    web.host = host;
                }
                "port" => {
                    web.port = u16::try_from(self.count(&path, value, 1, u32::from(u16::MAX))?)
                        .unwrap_or(u16::MAX);
                }
                _ => return Err(self.unknown(&path, key.span().start)),
            }
        }
        Ok(())
    }

    fn table<'a, 'i>(
        &self,
        path: &str,
        value: &'a Value<'i>,
    ) -> Result<&'a DeTable<'i>, ConfigError> {
        match value.get_ref() {
            DeValue::Table(table) => Ok(table),
            _ => Err(self.error(path, value.span().start, "expected a table")),
        }
    }

    fn string(&self, path: &str, value: &Value<'_>) -> Result<String, ConfigError> {
        match value.get_ref() {
            DeValue::String(s) => Ok(s.to_string()),
            _ => Err(self.error(path, value.span().start, "expected a string")),
        }
    }

    fn one_of(
        &self,
        path: &str,
        value: &Value<'_>,
        allowed: &[&str],
    ) -> Result<String, ConfigError> {
        let text = self.string(path, value)?;
        if allowed.contains(&text.as_str()) {
            Ok(text)
        } else {
            Err(self.error(
                path,
                value.span().start,
                format!("\"{text}\" is not one of {}", allowed.join(", ")),
            ))
        }
    }

    fn agent(&self, path: &str, value: &Value<'_>) -> Result<Agent, ConfigError> {
        let name = self.string(path, value)?;
        Agent::named(&name).ok_or_else(|| {
            self.error(
                path,
                value.span().start,
                format!("\"{name}\" is not a registered agent (codex, claude, pi)"),
            )
        })
    }

    fn boolean(&self, path: &str, value: &Value<'_>) -> Result<bool, ConfigError> {
        match value.get_ref() {
            DeValue::Boolean(b) => Ok(*b),
            _ => Err(self.error(path, value.span().start, "expected true or false")),
        }
    }

    /// An integer within `min..=max`.
    fn count(&self, path: &str, value: &Value<'_>, min: u32, max: u32) -> Result<u32, ConfigError> {
        let DeValue::Integer(integer) = value.get_ref() else {
            return Err(self.error(path, value.span().start, "expected an integer"));
        };
        i64::from_str_radix(integer.as_str(), integer.radix())
            .ok()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| (min..=max).contains(n))
            .ok_or_else(|| {
                self.error(
                    path,
                    value.span().start,
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
    fn positive(&self, path: &str, value: &Value<'_>) -> Result<f64, ConfigError> {
        let number = match value.get_ref() {
            DeValue::Integer(integer) => i64::from_str_radix(integer.as_str(), integer.radix())
                .ok()
                .map(|n| n as f64),
            DeValue::Float(float) => float.as_str().parse::<f64>().ok(),
            _ => None,
        };
        number
            .filter(|n| n.is_finite() && *n > 0.0)
            .ok_or_else(|| self.error(path, value.span().start, "expected a number above 0"))
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
