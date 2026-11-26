//! Native public-field normalization. Sources are pinned in docs/m4-native-public-streams.md.
use hoh_core::public_output::{PublicKind, PublicPart, PublicUsage};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Claude,
    CodexAppServer,
    CodexExec,
    Pi,
}
impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::CodexAppServer => "codex-app-server",
            Self::CodexExec => "codex-exec",
            Self::Pi => "pi",
        }
    }
    fn supports(self, version: &str) -> bool {
        match self {
            Self::Claude => matches!(
                version,
                "2.1.317" | "2.1.318" | "2.1.317 (Claude Code)" | "2.1.318 (Claude Code)"
            ),
            Self::CodexAppServer | Self::CodexExec => {
                matches!(version, "0.166.0" | "codex-cli 0.166.0")
            }
            Self::Pi => matches!(version, "1.5.0" | "1.6.0"),
        }
    }
}
#[derive(Debug)]
pub struct Normalizer {
    mode: Mode,
    version: String,
    offsets: BTreeMap<String, u64>,
    pi_message: Option<String>,
    received: Option<u64>,
    native_seen: BTreeSet<String>,
}
impl Normalizer {
    pub fn new(mode: Mode, version: &str) -> Self {
        Self {
            mode,
            version: version.into(),
            offsets: BTreeMap::new(),
            pi_message: None,
            received: None,
            native_seen: BTreeSet::new(),
        }
    }
    fn part(
        &self,
        id: String,
        block: String,
        offset: u64,
        kind: PublicKind,
        text: String,
    ) -> PublicPart {
        let (text, redacted) = redact(&text);
        PublicPart {
            native_stream: self.mode.name().into(),
            native_item_id: id,
            part: 0,
            received_offset: offset,
            block_key: block,
            stream: "text".into(),
            first_offset: 0,
            kind,
            speaker: (kind == PublicKind::Message).then(|| "assistant".into()),
            name: None,
            text,
            activity: None,
            model: None,
            usage: None,
            redacted,
            complete: false,
        }
    }
    fn text(
        &mut self,
        id: String,
        block: String,
        offset: u64,
        text: &str,
        final_snapshot: bool,
    ) -> PublicPart {
        let (clean, redacted) = redact(text);
        let text = clean.as_str();
        let previous = *self.offsets.get(&block).unwrap_or(&0);
        let body = if final_snapshot {
            text.get(usize::try_from(previous).unwrap_or(usize::MAX)..)
                .unwrap_or("")
        } else {
            text
        };
        let mut p = self.part(id, block.clone(), offset, PublicKind::Message, body.into());
        p.redacted |= redacted;
        p.first_offset = previous;
        p.complete = final_snapshot;
        self.offsets.insert(block, previous + p.text.len() as u64);
        p
    }
    pub fn feed(&mut self, frame: &Value, offset: u64) -> Vec<PublicPart> {
        if self.received.is_some_and(|last| offset <= last) {
            return Vec::new();
        }
        self.received = Some(offset);
        let native_key = match self.mode {
            Mode::Claude => frame["uuid"].as_str().map(str::to_owned),
            Mode::CodexAppServer => frame["params"]["item"]["id"]
                .as_str()
                .map(|id| format!("{}/{id}", frame["method"].as_str().unwrap_or(""))),
            Mode::CodexExec => frame["item"]["id"]
                .as_str()
                .map(|id| format!("{}/{id}", frame["type"].as_str().unwrap_or(""))),
            Mode::Pi => {
                if frame["type"] == "message_update" {
                    None
                } else {
                    frame["message"]["responseId"]
                        .as_str()
                        .or(frame["toolCallId"].as_str())
                        .map(|id| format!("{}/{id}", frame["type"].as_str().unwrap_or("")))
                }
            }
        };
        if native_key.is_some_and(|key| !self.native_seen.insert(key)) {
            return Vec::new();
        }
        if !self.mode.supports(&self.version) {
            let mut p = self.part(
                format!("unsupported/{offset}"),
                format!("unsupported/{offset}"),
                offset,
                PublicKind::Diagnostic,
                String::new(),
            );
            p.activity = Some("unsupported_native_version".into());
            return vec![p];
        }
        let mut parts = match self.mode {
            Mode::Claude => self.claude(frame, offset),
            Mode::CodexAppServer => self.codex(frame, offset, true),
            Mode::CodexExec => self.codex(frame, offset, false),
            Mode::Pi => self.pi(frame, offset),
        };
        let error = match self.mode {
            Mode::Claude if frame["type"] == "result" && frame["is_error"] == true => {
                frame["errors"].as_array().map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("\n")
                })
            }
            Mode::CodexExec if frame["type"] == "turn.failed" => {
                frame["error"]["message"].as_str().map(str::to_owned)
            }
            Mode::CodexExec if frame["type"] == "error" => {
                frame["message"].as_str().map(str::to_owned)
            }
            Mode::Pi if frame["type"] == "response" && frame["success"] == false => {
                frame["error"].as_str().map(str::to_owned)
            }
            Mode::Pi if frame["type"] == "message_end" => {
                frame["message"]["errorMessage"].as_str().map(str::to_owned)
            }
            _ => None,
        };
        if let Some(error) = error.filter(|text| !text.is_empty()) {
            let mut part = self.part(
                format!("error/{offset}"),
                format!("error/{offset}"),
                offset,
                PublicKind::Diagnostic,
                error,
            );
            part.activity = Some("error".into());
            part.complete = true;
            parts.push(part);
        }
        if parts.is_empty() {
            let mut p = self.part(
                format!("diagnostic/{offset}"),
                format!("diagnostic/{offset}"),
                offset,
                PublicKind::Diagnostic,
                String::new(),
            );
            p.activity = Some("private_or_unmapped_native_frame".into());
            vec![p]
        } else {
            parts
        }
    }
    fn claude(&mut self, f: &Value, offset: u64) -> Vec<PublicPart> {
        let kind = f["type"].as_str().unwrap_or("");
        let id = f["uuid"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("receive/{offset}"));
        let mut out = Vec::new();
        if matches!(kind, "assistant" | "user") {
            let m = &f["message"];
            if let Some(parts) = m["content"].as_array() {
                for (i, c) in parts.iter().enumerate() {
                    let mut p = match (kind, c["type"].as_str().unwrap_or("")) {
                        ("assistant", "text") => self.text(
                            id.clone(),
                            format!("message/{}/text/{i}", m["id"].as_str().unwrap_or(&id)),
                            offset,
                            c["text"].as_str().unwrap_or(""),
                            true,
                        ),
                        ("assistant", "tool_use") => {
                            let Some(call) = c["id"].as_str() else {
                                continue;
                            };
                            let mut p = self.part(
                                id.clone(),
                                format!("tool/{call}"),
                                offset,
                                PublicKind::ToolStart,
                                public_arguments(&c["input"]),
                            );
                            p.name = public_name(&c["name"]);
                            p.stream = "input".into();
                            p
                        }
                        ("user", "tool_result") => {
                            let Some(call) = c["tool_use_id"].as_str() else {
                                continue;
                            };
                            let mut p = self.part(
                                id.clone(),
                                format!("tool/{call}"),
                                offset,
                                PublicKind::ToolEnd,
                                public_text(&c["content"]),
                            );
                            p.stream = "output".into();
                            p.complete = true;
                            if c["is_error"] == true {
                                p.activity = Some("tool_error".into());
                            }
                            p
                        }
                        _ => continue,
                    };
                    p.part = i as u64;
                    p.model = public_name(&m["model"]);
                    out.push(p);
                }
            }
        } else if kind == "result" {
            let mut p = self.part(id.clone(), id, offset, PublicKind::Activity, String::new());
            p.activity = Some("usage".into());
            p.usage = Some(usage(&f["usage"], false));
            if let Some(u) = p.usage.as_mut() {
                u.cost_usd = f["total_cost_usd"]
                    .as_f64()
                    .filter(|n| n.is_finite() && *n >= 0.0);
            }
            if let Some(models) = f["modelUsage"].as_object()
                && models.len() == 1
                && let Some((model, values)) = models.iter().next()
            {
                p.model = public_name(&values["canonicalModel"])
                    .or_else(|| public_name(&Value::String(model.clone())));
                if let Some(u) = p.usage.as_mut() {
                    u.context_limit = values["contextWindow"].as_u64();
                }
            }
            p.complete = true;
            out.push(p);
        } else if kind == "system" && f["subtype"] == "compact_boundary" {
            let mut p = self.part(id.clone(), id, offset, PublicKind::Activity, String::new());
            p.activity = Some("compaction_completed".into());
            out.push(p);
        }
        out
    }
    fn codex(&mut self, f: &Value, offset: u64, app: bool) -> Vec<PublicPart> {
        let method = f[if app { "method" } else { "type" }]
            .as_str()
            .unwrap_or("");
        let params = if app { &f["params"] } else { f };
        let item = &params["item"];
        let mut out = Vec::new();
        if matches!(
            method,
            "item/started" | "item/completed" | "item.started" | "item.completed"
        ) {
            let Some(id) = item["id"].as_str() else {
                return out;
            };
            let done = matches!(method, "item/completed" | "item.completed");
            let key = format!("{method}/{id}");
            match item["type"].as_str().unwrap_or("") {
                "agentMessage" | "agent_message" if done => out.push(self.text(
                    key,
                    format!("message/{id}"),
                    offset,
                    item["text"].as_str().unwrap_or(""),
                    true,
                )),
                "commandExecution" | "command_execution" => {
                    let text = if done {
                        item[if app {
                            "aggregatedOutput"
                        } else {
                            "aggregated_output"
                        }]
                        .as_str()
                        .unwrap_or("")
                    } else {
                        item["command"].as_str().unwrap_or("")
                    };
                    let mut p = self.part(
                        key,
                        format!("tool/{id}"),
                        offset,
                        if done {
                            PublicKind::ToolEnd
                        } else {
                            PublicKind::ToolStart
                        },
                        text.into(),
                    );
                    p.name = Some("command".into());
                    p.stream = if done { "output" } else { "input" }.into();
                    p.complete = done;
                    if done
                        && (item["status"] == "failed"
                            || item[if app { "exitCode" } else { "exit_code" }]
                                .as_i64()
                                .is_some_and(|code| code != 0))
                    {
                        p.activity = Some("tool_error".into());
                    }
                    out.push(p);
                }
                "collabAgentToolCall" | "mcpToolCall" | "dynamicToolCall" => {
                    let body = if !done {
                        public_arguments(&item["arguments"])
                    } else if item["type"] == "mcpToolCall" {
                        public_text(&item["result"]["content"])
                    } else if item["type"] == "dynamicToolCall" {
                        item["contentItems"]
                            .as_array()
                            .map(|items| {
                                items
                                    .iter()
                                    .filter(|v| v["type"] == "inputText")
                                    .filter_map(|v| v["text"].as_str())
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            })
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    let mut p = self.part(
                        key,
                        format!("tool/{id}"),
                        offset,
                        if done {
                            PublicKind::ToolEnd
                        } else {
                            PublicKind::ToolStart
                        },
                        body,
                    );
                    p.name = public_name(&item["tool"]);
                    p.stream = if done { "output" } else { "input" }.into();
                    p.complete = done;
                    if item["type"] == "collabAgentToolCall" {
                        p.activity = Some("handoff".into());
                    }
                    out.push(p);
                }
                "contextCompaction" => {
                    let mut p = self.part(
                        key,
                        format!("activity/{id}"),
                        offset,
                        PublicKind::Activity,
                        String::new(),
                    );
                    p.activity = Some(
                        if done {
                            "compaction_completed"
                        } else {
                            "compacting"
                        }
                        .into(),
                    );
                    out.push(p);
                }
                _ => {}
            }
        } else if app && method == "error" {
            let mut p = self.part(
                format!("error/{offset}"),
                format!("error/{offset}"),
                offset,
                PublicKind::Diagnostic,
                params["error"]["message"].as_str().unwrap_or("").into(),
            );
            p.activity = Some("error".into());
            out.push(p);
        } else if app && method == "item/agentMessage/delta" {
            if let (Some(id), Some(delta)) = (params["itemId"].as_str(), params["delta"].as_str()) {
                out.push(self.text(
                    format!("delta/{id}/{offset}"),
                    format!("message/{id}"),
                    offset,
                    delta,
                    false,
                ));
            }
        } else if method == "thread/tokenUsage/updated" || method == "turn.completed" {
            let v = if app {
                &params["tokenUsage"]["total"]
            } else {
                &f["usage"]
            };
            let mut u = usage(v, app);
            if app {
                u.context_limit = params["tokenUsage"]["modelContextWindow"].as_u64();
            }
            let id = format!("usage/{offset}");
            let mut p = self.part(id.clone(), id, offset, PublicKind::Activity, String::new());
            p.activity = Some("usage".into());
            p.usage = Some(u);
            p.complete = !app;
            out.push(p);
        }
        out
    }
    fn pi(&mut self, f: &Value, offset: u64) -> Vec<PublicPart> {
        let kind = f["type"].as_str().unwrap_or("");
        let id = if kind == "message_update" {
            format!("receive/{offset}")
        } else {
            f["message"]["responseId"]
                .as_str()
                .map(|id| format!("{kind}/{id}"))
                .unwrap_or_else(|| format!("receive/{offset}"))
        };
        let mut out = Vec::new();
        let m = &f["message"];
        if kind == "message_start" && m["role"] == "assistant" {
            self.pi_message = Some(format!("message/{offset}"));
        }
        if kind == "message_update" && f["assistantMessageEvent"]["type"] == "text_delta" {
            let e = &f["assistantMessageEvent"];
            let block = format!(
                "{}/text/{}",
                self.pi_message.as_deref().unwrap_or(&id),
                e["contentIndex"].as_u64().unwrap_or(0)
            );
            out.push(self.text(id, block, offset, e["delta"].as_str().unwrap_or(""), false));
        } else if kind == "message_end" && m["role"] == "assistant" {
            if let Some(cs) = m["content"].as_array() {
                for (i, c) in cs.iter().enumerate() {
                    if c["type"] == "text" {
                        let block =
                            format!("{}/text/{i}", self.pi_message.as_deref().unwrap_or(&id));
                        let mut p = self.text(
                            id.clone(),
                            block,
                            offset,
                            c["text"].as_str().unwrap_or(""),
                            true,
                        );
                        p.part = i as u64;
                        p.model = public_name(&m["model"]);
                        out.push(p);
                    }
                }
            }
            let mut p = self.part(
                format!("usage/{id}"),
                format!("usage/{id}"),
                offset,
                PublicKind::Activity,
                String::new(),
            );
            p.part = out.len() as u64;
            p.model = public_name(&m["model"]);
            p.activity = Some("usage".into());
            p.usage = Some(PublicUsage {
                input_tokens: m["usage"]["input"].as_u64(),
                output_tokens: m["usage"]["output"].as_u64(),
                total_tokens: m["usage"]["totalTokens"].as_u64(),
                cost_usd: m["usage"]["cost"]["total"]
                    .as_f64()
                    .filter(|v| v.is_finite() && *v >= 0.0),
                context_limit: None,
            });
            out.push(p);
        } else if matches!(kind, "tool_execution_start" | "tool_execution_end") {
            if let Some(call) = f["toolCallId"].as_str() {
                let done = kind == "tool_execution_end";
                let mut p = self.part(
                    format!("{kind}/{call}"),
                    format!("tool/{call}"),
                    offset,
                    if done {
                        PublicKind::ToolEnd
                    } else {
                        PublicKind::ToolStart
                    },
                    if done {
                        public_text(&f["result"]["content"])
                    } else {
                        public_arguments(&f["args"])
                    },
                );
                p.name = public_name(&f["toolName"]);
                p.stream = if done { "output" } else { "input" }.into();
                p.complete = done;
                if done && f["isError"] == true {
                    p.activity = Some("tool_error".into());
                }
                out.push(p);
            }
        } else if matches!(
            kind,
            "compaction_start" | "compaction_end" | "auto_retry_start" | "auto_retry_end"
        ) {
            let mut p = self.part(id.clone(), id, offset, PublicKind::Activity, String::new());
            p.activity = Some(kind.into());
            out.push(p);
        }
        out
    }
}
fn public_name(v: &Value) -> Option<String> {
    v.as_str()
        .filter(|s| s.len() <= 256 && !s.chars().any(char::is_control))
        .map(|s| redact(s).0)
}
fn public_arguments(v: &Value) -> String {
    // Only the recorded public command/path fields; nested env, headers and arbitrary prompt objects are excluded.
    ["command", "file_path", "path"]
        .into_iter()
        .filter_map(|k| v[k].as_str().map(|s| format!("{k}: {s}")))
        .collect::<Vec<_>>()
        .join("\n")
}
fn public_text(v: &Value) -> String {
    if let Some(s) = v.as_str() {
        s.into()
    } else {
        v.as_array()
            .map(|xs| {
                xs.iter()
                    .filter(|v| v["type"] == "text")
                    .filter_map(|v| v["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }
}
fn usage(v: &Value, camel: bool) -> PublicUsage {
    PublicUsage {
        input_tokens: v[if camel { "inputTokens" } else { "input_tokens" }].as_u64(),
        output_tokens: v[if camel {
            "outputTokens"
        } else {
            "output_tokens"
        }]
        .as_u64(),
        total_tokens: v[if camel { "totalTokens" } else { "total_tokens" }].as_u64(),
        ..Default::default()
    }
}
fn redact(text: &str) -> (String, bool) {
    let mut changed = false;
    let clean = text
        .split_inclusive('\n')
        .map(|line| {
            let low = line.to_ascii_lowercase();
            if [
                "authorization:",
                "bearer ",
                "api_key",
                "api-key",
                "apikey",
                "password",
                "access_token",
                "secret",
                "private_key",
                "token=",
            ]
            .iter()
            .any(|needle| low.contains(needle))
            {
                changed = true;
                if line.ends_with('\n') {
                    "[redacted]\n"
                } else {
                    "[redacted]"
                }
            } else {
                line
            }
        })
        .collect();
    (clean, changed)
}
#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
#[derive(Debug, Clone, Default)]
pub struct Backpressure(Arc<std::sync::Mutex<Delay>>);
#[derive(Debug, Default)]
struct Delay {
    total: Duration,
    since: Option<Instant>,
    holders: usize,
}
impl Backpressure {
    pub fn snapshot(&self) -> (Duration, bool) {
        let d = self.0.lock().unwrap_or_else(|e| e.into_inner());
        (
            d.total + d.since.map_or(Duration::ZERO, |t| t.elapsed()),
            d.holders > 0,
        )
    }
    pub fn blocked(&self) -> Blocked {
        let mut d = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if d.holders == 0 {
            d.since = Some(Instant::now());
        }
        d.holders += 1;
        Blocked(self.clone())
    }
}
#[derive(Debug)]
pub struct Blocked(Backpressure);
impl Drop for Blocked {
    fn drop(&mut self) {
        let mut d = self.0.0.lock().unwrap_or_else(|e| e.into_inner());
        d.holders -= 1;
        if d.holders == 0
            && let Some(t) = d.since.take()
        {
            d.total += t.elapsed();
        }
    }
}
#[derive(Debug)]
pub struct Pending {
    pub part: PublicPart,
    _bytes: OwnedSemaphorePermit,
}
#[derive(Debug)]
pub struct Receiver {
    inner: mpsc::Receiver<Pending>,
    pub backpressure: Backpressure,
}
impl Receiver {
    pub async fn recv(&mut self) -> Option<Pending> {
        self.inner.recv().await
    }
}
#[derive(Debug, Clone)]
pub struct Publisher {
    sender: mpsc::Sender<Pending>,
    bytes: Arc<Semaphore>,
    pressure: Backpressure,
    pub mode: Mode,
    pub version: String,
}
pub fn channel(mode: Mode, version: &str) -> (Publisher, Receiver) {
    let (sender, inner) = mpsc::channel(64);
    let pressure = Backpressure::default();
    (
        Publisher {
            sender,
            bytes: Arc::new(Semaphore::new(1 << 20)),
            pressure: pressure.clone(),
            mode,
            version: version.into(),
        },
        Receiver {
            inner,
            backpressure: pressure,
        },
    )
}
impl Publisher {
    pub fn for_mode(&self, mode: Mode) -> Self {
        let mut p = self.clone();
        p.mode = mode;
        p
    }
    pub fn backpressure(&self) -> Backpressure {
        self.pressure.clone()
    }
    pub async fn send(&self, part: PublicPart) -> Result<(), &'static str> {
        let count = u32::try_from(part.text.len().max(1)).map_err(|_| "public body too large")?;
        if count > 1 << 20 {
            return Err("public body exceeds one segment");
        }
        let bytes = match self.bytes.clone().try_acquire_many_owned(count) {
            Ok(p) => p,
            Err(_) => {
                let _blocked = self.pressure.blocked();
                self.bytes
                    .clone()
                    .acquire_many_owned(count)
                    .await
                    .map_err(|_| "public queue closed")?
            }
        };
        let pending = Pending {
            part,
            _bytes: bytes,
        };
        match self.sender.try_send(pending) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Closed(_)) => Err("public queue closed"),
            Err(mpsc::error::TrySendError::Full(p)) => {
                let _blocked = self.pressure.blocked();
                self.sender.send(p).await.map_err(|_| "public queue closed")
            }
        }
    }
}

/// No provider object or exception text is put in a public gap.
pub(crate) fn gap(mode: Mode, offset: u64, reason: &str) -> PublicPart {
    let n = Normalizer::new(mode, "");
    let mut p = n.part(
        format!("gap/{offset}/{reason}"),
        format!("gap/{offset}"),
        offset,
        PublicKind::Gap,
        String::new(),
    );
    p.activity = Some(reason.into());
    p
}
pub(crate) async fn publish(
    sender: &Publisher,
    parts: Vec<PublicPart>,
) -> Result<(), &'static str> {
    for part in parts {
        if part.text.len() <= 1 << 20 {
            sender.send(part).await?;
            continue;
        }
        let mut offset = 0;
        let mut segment = 0;
        while offset < part.text.len() {
            let mut end = (offset + (1 << 20)).min(part.text.len());
            while !part.text.is_char_boundary(end) {
                end -= 1;
            }
            let mut split = part.clone();
            split.native_item_id = format!("{}/segment/{segment}", part.native_item_id);
            split.first_offset += offset as u64;
            split.text = part.text[offset..end].into();
            split.complete = part.complete && end == part.text.len();
            sender.send(split).await?;
            offset = end;
            segment += 1;
        }
    }
    Ok(())
}
