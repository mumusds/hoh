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
/// Maximum time and raw byte size for coalescing consecutive streaming text deltas (CR-028 §2.2).
pub const COALESCE_WINDOW: Duration = Duration::from_millis(50);
pub const COALESCE_BYTES: usize = 16 << 10;

#[derive(Debug, Default)]
struct TextStreamState {
    raw_accumulated: String,
    raw_line: String,
    line_redacted: bool,
    pub_offset: u64,
    any_redacted: bool,
}

#[derive(Debug)]
struct CoalescedText {
    native_item_id: String,
    block_key: String,
    received_offset: u64,
    part: u64,
    model: Option<String>,
    raw_text: String,
    started_at: Instant,
}

#[derive(Debug, Default)]
struct UnmappedSummary {
    last_offset: u64,
    count: u64,
    emitted: u64,
}

#[derive(Debug)]
pub struct Normalizer {
    mode: Mode,
    version: String,
    streams: BTreeMap<String, TextStreamState>,
    pi_message: Option<String>,
    received: Option<u64>,
    native_seen: BTreeSet<String>,
    coalescing: bool,
    pending_text: Option<CoalescedText>,
    unmapped: BTreeMap<(String, String), UnmappedSummary>,
}
impl Normalizer {
    pub fn new(mode: Mode, version: &str) -> Self {
        Self {
            mode,
            version: version.into(),
            streams: BTreeMap::new(),
            pi_message: None,
            received: None,
            native_seen: BTreeSet::new(),
            coalescing: false,
            pending_text: None,
            unmapped: BTreeMap::new(),
        }
    }
    pub fn new_coalescing(mode: Mode, version: &str) -> Self {
        let mut n = Self::new(mode, version);
        n.coalescing = true;
        n
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
        self.part_already_redacted(id, block, offset, kind, text, redacted)
    }
    fn part_already_redacted(
        &self,
        id: String,
        block: String,
        offset: u64,
        kind: PublicKind,
        text: String,
        redacted: bool,
    ) -> PublicPart {
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
            frame_kind: None,
            count: None,
        }
    }
    /// Complete non-delta text block (e.g. Claude assistant content block): never slices from another block's offset.
    fn complete_text(&self, id: String, block: String, offset: u64, text: &str) -> PublicPart {
        let mut p = self.part(id, block, offset, PublicKind::Message, text.into());
        p.first_offset = 0;
        p.complete = true;
        p
    }
    fn emit_stream_chunk(
        &mut self,
        id: String,
        block: String,
        offset: u64,
        part: u64,
        model: Option<String>,
        raw_chunk: &str,
        complete: bool,
    ) -> PublicPart {
        let state = self.streams.entry(block.clone()).or_default();
        let (clean, changed) = redact_streaming_chunk(state, raw_chunk);
        let first_offset = state.pub_offset;
        state.pub_offset += clean.len() as u64;
        let redacted = changed || (complete && state.any_redacted);
        let mut p =
            self.part_already_redacted(id, block, offset, PublicKind::Message, clean, redacted);
        p.part = part;
        p.model = model;
        p.first_offset = first_offset;
        p.complete = complete;
        p
    }
    fn flush_pending_text(&mut self, out: &mut Vec<PublicPart>) {
        if let Some(pending) = self.pending_text.take() {
            out.push(self.emit_stream_chunk(
                pending.native_item_id,
                pending.block_key,
                pending.received_offset,
                pending.part,
                pending.model,
                &pending.raw_text,
                false,
            ));
        }
    }
    fn push_delta(
        &mut self,
        id: String,
        block: String,
        offset: u64,
        delta: &str,
        now: Instant,
        out: &mut Vec<PublicPart>,
    ) {
        if !self.coalescing {
            out.push(self.emit_stream_chunk(id, block, offset, 0, None, delta, false));
            return;
        }
        if self
            .pending_text
            .as_ref()
            .is_some_and(|p| p.block_key != block)
        {
            self.flush_pending_text(out);
        }
        let pending = self.pending_text.get_or_insert_with(|| CoalescedText {
            native_item_id: id,
            block_key: block,
            received_offset: offset,
            part: 0,
            model: None,
            raw_text: String::new(),
            started_at: now,
        });
        pending.raw_text.push_str(delta);
        if pending.raw_text.len() >= COALESCE_BYTES
            || now.saturating_duration_since(pending.started_at) >= COALESCE_WINDOW
        {
            self.flush_pending_text(out);
        }
    }
    fn finish_stream_text(
        &mut self,
        id: String,
        block: String,
        offset: u64,
        part: u64,
        model: Option<String>,
        final_text: &str,
        out: &mut Vec<PublicPart>,
    ) {
        if self
            .pending_text
            .as_ref()
            .is_some_and(|p| p.block_key != block)
        {
            self.flush_pending_text(out);
        }
        if let Some(mut pending) = self.pending_text.take() {
            let committed_len = self
                .streams
                .get(&block)
                .map_or(0, |s| s.raw_accumulated.len());
            let prefix_len = committed_len + pending.raw_text.len();
            if final_text.len() > prefix_len
                && final_text.is_char_boundary(committed_len)
                && final_text.is_char_boundary(prefix_len)
                && &final_text[..committed_len]
                    == self
                        .streams
                        .get(&block)
                        .map_or("", |s| s.raw_accumulated.as_str())
                && &final_text[committed_len..prefix_len] == pending.raw_text
            {
                pending.raw_text.push_str(&final_text[prefix_len..]);
            }
            let model = model.or(pending.model);
            out.push(self.emit_stream_chunk(
                pending.native_item_id,
                pending.block_key,
                pending.received_offset,
                part,
                model,
                &pending.raw_text,
                true,
            ));
            return;
        }
        let remaining = match self.streams.get(&block) {
            Some(state) if !state.raw_accumulated.is_empty() => final_text
                .strip_prefix(&state.raw_accumulated)
                .unwrap_or(""),
            _ => final_text,
        };
        out.push(self.emit_stream_chunk(id, block, offset, part, model, remaining, true));
    }
    pub fn next_flush_deadline(&self) -> Option<Instant> {
        self.pending_text
            .as_ref()
            .map(|p| p.started_at + COALESCE_WINDOW)
    }
    pub fn flush_due(&mut self, now: Instant) -> Vec<PublicPart> {
        let mut out = Vec::new();
        if self
            .pending_text
            .as_ref()
            .is_some_and(|p| now.saturating_duration_since(p.started_at) >= COALESCE_WINDOW)
        {
            self.flush_pending_text(&mut out);
        }
        out
    }
    pub fn flush_text(&mut self) -> Vec<PublicPart> {
        let mut out = Vec::new();
        self.flush_pending_text(&mut out);
        out
    }
    pub fn flush(&mut self) -> Vec<PublicPart> {
        let mut out = Vec::new();
        self.flush_pending_text(&mut out);
        let keys: Vec<(String, String)> = self
            .unmapped
            .iter()
            .filter(|(_, s)| s.count > s.emitted)
            .map(|(k, _)| k.clone())
            .collect();
        for (activity, kind) in keys {
            if let Some(summary) = self.unmapped.get_mut(&(activity.clone(), kind.clone())) {
                let count = summary.count;
                let offset = summary.last_offset;
                summary.emitted = count;
                let mut p = self.part_already_redacted(
                    format!("diagnostic/{activity}/{kind}/summary/{count}"),
                    format!("diagnostic/{activity}/{kind}"),
                    offset,
                    PublicKind::Diagnostic,
                    String::new(),
                    false,
                );
                p.activity = Some(activity);
                p.frame_kind = Some(kind);
                p.count = Some(count);
                p.complete = true;
                out.push(p);
            }
        }
        out
    }
    fn record_unmapped(&mut self, activity: &str, frame: &Value, offset: u64) -> Vec<PublicPart> {
        let kind = frame_kind(self.mode, frame);
        let entry = self
            .unmapped
            .entry((activity.into(), kind.clone()))
            .or_default();
        entry.count += 1;
        entry.last_offset = offset;
        if entry.count == 1 {
            entry.emitted = 1;
            let mut out = Vec::new();
            self.flush_pending_text(&mut out);
            let mut p = self.part_already_redacted(
                format!("diagnostic/{activity}/{kind}/first"),
                format!("diagnostic/{activity}/{kind}"),
                offset,
                PublicKind::Diagnostic,
                String::new(),
                false,
            );
            p.activity = Some(activity.into());
            p.frame_kind = Some(kind);
            p.count = Some(1);
            out.push(p);
            out
        } else {
            Vec::new()
        }
    }
    pub fn feed(&mut self, frame: &Value, offset: u64) -> Vec<PublicPart> {
        self.feed_at(frame, offset, Instant::now())
    }
    pub fn feed_at(&mut self, frame: &Value, offset: u64, now: Instant) -> Vec<PublicPart> {
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
            return self.record_unmapped("unsupported_native_version", frame, offset);
        }
        let mut handled = false;
        let mut parts = match self.mode {
            Mode::Claude => self.claude(frame, offset, &mut handled),
            Mode::CodexAppServer => self.codex(frame, offset, true, now, &mut handled),
            Mode::CodexExec => self.codex(frame, offset, false, now, &mut handled),
            Mode::Pi => self.pi(frame, offset, now, &mut handled),
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
            handled = true;
            self.flush_pending_text(&mut parts);
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
        if !handled && parts.is_empty() {
            self.record_unmapped("private_or_unmapped_native_frame", frame, offset)
        } else {
            parts
        }
    }
    fn claude(&mut self, f: &Value, offset: u64, handled: &mut bool) -> Vec<PublicPart> {
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
                        ("assistant", "text") => {
                            self.flush_pending_text(&mut out);
                            self.complete_text(
                                id.clone(),
                                format!(
                                    "message/{}/{id}/text/{i}",
                                    m["id"].as_str().unwrap_or(&id)
                                ),
                                offset,
                                c["text"].as_str().unwrap_or(""),
                            )
                        }
                        ("assistant", "tool_use") => {
                            let Some(call) = c["id"].as_str() else {
                                continue;
                            };
                            self.flush_pending_text(&mut out);
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
                            self.flush_pending_text(&mut out);
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
                    *handled = true;
                    out.push(p);
                }
            }
        } else if kind == "result" {
            self.flush_pending_text(&mut out);
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
            *handled = true;
            out.push(p);
        } else if kind == "system" && f["subtype"] == "compact_boundary" {
            self.flush_pending_text(&mut out);
            let mut p = self.part(id.clone(), id, offset, PublicKind::Activity, String::new());
            p.activity = Some("compaction_completed".into());
            *handled = true;
            out.push(p);
        }
        out
    }
    fn codex(
        &mut self,
        f: &Value,
        offset: u64,
        app: bool,
        now: Instant,
        handled: &mut bool,
    ) -> Vec<PublicPart> {
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
                "agentMessage" | "agent_message" if done => {
                    *handled = true;
                    self.finish_stream_text(
                        key,
                        format!("message/{id}"),
                        offset,
                        0,
                        None,
                        item["text"].as_str().unwrap_or(""),
                        &mut out,
                    );
                }
                "commandExecution" | "command_execution" => {
                    self.flush_pending_text(&mut out);
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
                    *handled = true;
                    out.push(p);
                }
                "collabAgentToolCall" | "mcpToolCall" | "dynamicToolCall" => {
                    self.flush_pending_text(&mut out);
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
                    *handled = true;
                    out.push(p);
                }
                "contextCompaction" => {
                    self.flush_pending_text(&mut out);
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
                    *handled = true;
                    out.push(p);
                }
                _ => {}
            }
        } else if app && method == "error" {
            self.flush_pending_text(&mut out);
            let mut p = self.part(
                format!("error/{offset}"),
                format!("error/{offset}"),
                offset,
                PublicKind::Diagnostic,
                params["error"]["message"].as_str().unwrap_or("").into(),
            );
            p.activity = Some("error".into());
            *handled = true;
            out.push(p);
        } else if app && method == "item/agentMessage/delta" {
            if let (Some(id), Some(delta)) = (params["itemId"].as_str(), params["delta"].as_str()) {
                *handled = true;
                self.push_delta(
                    format!("delta/{id}/{offset}"),
                    format!("message/{id}"),
                    offset,
                    delta,
                    now,
                    &mut out,
                );
            }
        } else if method == "thread/tokenUsage/updated" || method == "turn.completed" {
            self.flush_pending_text(&mut out);
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
            *handled = true;
            out.push(p);
        }
        out
    }
    fn pi(&mut self, f: &Value, offset: u64, now: Instant, handled: &mut bool) -> Vec<PublicPart> {
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
            *handled = true;
            self.push_delta(
                id,
                block,
                offset,
                e["delta"].as_str().unwrap_or(""),
                now,
                &mut out,
            );
        } else if kind == "message_end" && m["role"] == "assistant" {
            let model = public_name(&m["model"]);
            if let Some(cs) = m["content"].as_array() {
                for (i, c) in cs.iter().enumerate() {
                    if c["type"] == "text" {
                        let block =
                            format!("{}/text/{i}", self.pi_message.as_deref().unwrap_or(&id));
                        self.finish_stream_text(
                            id.clone(),
                            block,
                            offset,
                            i as u64,
                            model.clone(),
                            c["text"].as_str().unwrap_or(""),
                            &mut out,
                        );
                    }
                }
            }
            self.flush_pending_text(&mut out);
            let mut p = self.part(
                format!("usage/{id}"),
                format!("usage/{id}"),
                offset,
                PublicKind::Activity,
                String::new(),
            );
            p.part = out.len() as u64;
            p.model = model;
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
            *handled = true;
            out.push(p);
        } else if matches!(kind, "tool_execution_start" | "tool_execution_end") {
            if let Some(call) = f["toolCallId"].as_str() {
                self.flush_pending_text(&mut out);
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
                *handled = true;
                out.push(p);
            }
        } else if matches!(
            kind,
            "compaction_start" | "compaction_end" | "auto_retry_start" | "auto_retry_end"
        ) {
            self.flush_pending_text(&mut out);
            let mut p = self.part(id.clone(), id, offset, PublicKind::Activity, String::new());
            p.activity = Some(kind.into());
            *handled = true;
            out.push(p);
        }
        out
    }
}

fn safe_discriminant(v: &Value) -> Option<String> {
    v.as_str()
        .filter(|s| !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control))
        .map(str::to_owned)
}

fn frame_kind(mode: Mode, frame: &Value) -> String {
    match mode {
        Mode::Claude => {
            let kind = safe_discriminant(&frame["type"]).unwrap_or_else(|| "unknown".into());
            if kind == "system" {
                if let Some(sub) = safe_discriminant(&frame["subtype"]) {
                    return format!("system/{sub}");
                }
            } else if matches!(kind.as_str(), "assistant" | "user")
                && let Some(first) = frame["message"]["content"]
                    .as_array()
                    .and_then(|arr| arr.first())
                    .and_then(|c| safe_discriminant(&c["type"]))
            {
                return format!("{kind}/{first}");
            }
            kind
        }
        Mode::CodexAppServer => {
            if let Some(method) = safe_discriminant(&frame["method"]) {
                if matches!(method.as_str(), "item/started" | "item/completed")
                    && let Some(item_type) = safe_discriminant(&frame["params"]["item"]["type"])
                {
                    return format!("{method}/{item_type}");
                }
                method
            } else if !frame["id"].is_null() {
                "jsonrpc_response".into()
            } else {
                "unknown".into()
            }
        }
        Mode::CodexExec => {
            let kind = safe_discriminant(&frame["type"]).unwrap_or_else(|| "unknown".into());
            if matches!(kind.as_str(), "item.started" | "item.completed")
                && let Some(item_type) = safe_discriminant(&frame["item"]["type"])
            {
                return format!("{kind}/{item_type}");
            }
            kind
        }
        Mode::Pi => {
            let kind = safe_discriminant(&frame["type"]).unwrap_or_else(|| "unknown".into());
            if kind == "message_update"
                && let Some(ev) = safe_discriminant(&frame["assistantMessageEvent"]["type"])
            {
                return format!("message_update/{ev}");
            }
            kind
        }
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
fn line_contains_secret(line: &str) -> bool {
    let low = line.to_ascii_lowercase();
    [
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
}
fn redact_streaming_chunk(state: &mut TextStreamState, raw: &str) -> (String, bool) {
    let mut changed = false;
    let mut out = String::with_capacity(raw.len());
    for segment in raw.split_inclusive('\n') {
        let ends_nl = segment.ends_with('\n');
        state.raw_line.push_str(segment);
        if state.line_redacted {
            changed = true;
            if ends_nl {
                out.push('\n');
                state.line_redacted = false;
                state.raw_line.clear();
            }
        } else if line_contains_secret(&state.raw_line) {
            changed = true;
            if ends_nl {
                out.push_str("[redacted]\n");
                state.raw_line.clear();
                state.line_redacted = false;
            } else {
                out.push_str("[redacted]");
                state.line_redacted = true;
            }
        } else {
            out.push_str(segment);
            if ends_nl {
                state.raw_line.clear();
                state.line_redacted = false;
            }
        }
    }
    state.raw_accumulated.push_str(raw);
    state.any_redacted |= changed;
    (out, changed)
}
fn redact(text: &str) -> (String, bool) {
    let mut changed = false;
    let clean = text
        .split_inclusive('\n')
        .map(|line| {
            if line_contains_secret(line) {
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
