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
/// The trailing bytes of an unterminated line that are never published before the line ends (M4T09R-400): a byte is
/// published early only once this many keyword-free bytes follow it on its line.
pub const WITHHELD_WINDOW: usize = 16 << 10;

/// One streamed block (DC-M4T09b): nothing after the block's last newline is published until the newline arrives or
/// the block ends, except that an unterminated line holding [`COALESCE_BYTES`] more than [`WITHHELD_WINDOW`] publishes
/// its bytes older than the window (M4T09R-400). Every keyword check sees the whole withheld window, so a byte is
/// published unredacted before its line ends only if no keyword ends within the window after it. Only the
/// unterminated tail (under the cap plus the window plus one push) and the received length and digest (for the final
/// snapshot's prefix check) are kept, never the block's whole text (M4T09R-302).
#[derive(Default)]
struct TextStreamState {
    tail: String,
    /// The current line's start was published before its end.
    line_published: bool,
    /// The current line was redacted before its end: its remaining bytes are suppressed up to its newline.
    line_redacted: bool,
    received_len: usize,
    received_digest: sha2::Sha256,
    pub_offset: u64,
}
impl std::fmt::Debug for TextStreamState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextStreamState")
            .field("tail_len", &self.tail.len())
            .field("line_published", &self.line_published)
            .field("line_redacted", &self.line_redacted)
            .field("received_len", &self.received_len)
            .field("pub_offset", &self.pub_offset)
            .finish()
    }
}
impl TextStreamState {
    fn received(&mut self, raw: &str) {
        use sha2::Digest as _;
        self.received_len += raw.len();
        self.received_digest.update(raw.as_bytes());
    }
    /// The part of `text` after what this block already received, if `text` extends it; else `None`.
    fn after_received<'a>(&self, text: &'a str) -> Option<&'a str> {
        use sha2::Digest as _;
        let n = self.received_len;
        (text.len() >= n
            && text.is_char_boundary(n)
            && sha2::Sha256::digest(&text.as_bytes()[..n])
                == self.received_digest.clone().finalize())
        .then(|| &text[n..])
    }
    /// Takes raw text of this block and returns what may be published now, redacted, and whether redaction altered it:
    /// it carries a `[redacted]` marker. The bytes a marker stands for are suppressed, also when they arrive later; a
    /// redacted line's newline is published unaltered. `end`: no more text follows, so the unterminated tail is
    /// published too.
    fn push(&mut self, raw: &str, end: bool) -> (String, bool) {
        let mut out = String::new();
        let mut changed = false;
        self.tail.push_str(raw);
        if let Some(last) = self.tail.rfind('\n') {
            let rest = self.tail.split_off(last + 1);
            let lines = std::mem::replace(&mut self.tail, rest);
            let first = lines.find('\n').map_or(lines.len(), |i| i + 1);
            let (head, others) = lines.split_at(first);
            if self.line_redacted {
                out.push('\n');
            } else if self.line_published {
                // The rest of a line whose start was published early: what was published cannot be recalled.
                if line_contains_secret(head) {
                    out.push_str("[redacted]\n");
                    changed = true;
                } else {
                    out.push_str(head);
                }
            } else {
                // The batch path's function on the whole line: identical output for an identical line.
                let (clean, more) = redact(head);
                out.push_str(&clean);
                changed |= more;
            }
            self.line_published = false;
            self.line_redacted = false;
            let (clean, more) = redact(others);
            out.push_str(&clean);
            changed |= more;
        }
        if self.line_redacted {
            self.tail.clear();
        } else if end {
            let tail = std::mem::take(&mut self.tail);
            if self.line_published {
                if line_contains_secret(&tail) {
                    out.push_str("[redacted]");
                    changed = true;
                } else {
                    out.push_str(&tail);
                }
            } else {
                let (clean, more) = redact(&tail);
                out.push_str(&clean);
                changed |= more;
            }
        } else if self.tail.len() >= COALESCE_BYTES + WITHHELD_WINDOW {
            if line_contains_secret(&self.tail) {
                out.push_str("[redacted]");
                changed = true;
                self.line_redacted = true;
                self.tail.clear();
            } else {
                let mut cut = self.tail.len() - WITHHELD_WINDOW;
                while !self.tail.is_char_boundary(cut) {
                    cut -= 1;
                }
                let window = self.tail.split_off(cut);
                out.push_str(&std::mem::replace(&mut self.tail, window));
                self.line_published = true;
            }
        }
        if end {
            self.line_published = false;
            self.line_redacted = false;
            self.tail.clear();
        }
        (out, changed)
    }
    fn holds_text(&self) -> bool {
        !self.tail.is_empty()
    }
}

struct StreamMeta {
    id: String,
    block: String,
    offset: u64,
    part: u64,
    model: Option<String>,
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
            input_id: None,
        }
    }
    /// Complete non-delta text block (e.g. Claude assistant content block): never slices from another block's offset.
    fn complete_text(&self, id: String, block: String, offset: u64, text: &str) -> PublicPart {
        let mut p = self.part(id, block, offset, PublicKind::Message, text.into());
        p.first_offset = 0;
        p.complete = true;
        p
    }
    /// Publishes what DC-M4T09b allows of `raw_chunk` (already counted as received); `end` also publishes the held
    /// tail. A chunk with nothing to publish yields no part unless it completes the block.
    fn emit_stream_chunk(
        &mut self,
        meta: StreamMeta,
        raw_chunk: &str,
        end: bool,
        complete: bool,
    ) -> Option<PublicPart> {
        let state = self.streams.entry(meta.block.clone()).or_default();
        let (clean, redacted) = state.push(raw_chunk, end);
        if clean.is_empty() && !complete {
            return None;
        }
        let first_offset = state.pub_offset;
        state.pub_offset += clean.len() as u64;
        let mut p = self.part_already_redacted(
            meta.id,
            meta.block,
            meta.offset,
            PublicKind::Message,
            clean,
            redacted,
        );
        p.part = meta.part;
        p.model = meta.model;
        p.first_offset = first_offset;
        p.complete = complete;
        Some(p)
    }
    fn flush_pending_text(&mut self, out: &mut Vec<PublicPart>) {
        if let Some(pending) = self.pending_text.take() {
            out.extend(self.emit_stream_chunk(
                StreamMeta {
                    id: pending.native_item_id,
                    block: pending.block_key,
                    offset: pending.received_offset,
                    part: pending.part,
                    model: pending.model,
                },
                &pending.raw_text,
                false,
                false,
            ));
        }
    }
    /// The stream ends (process end, cancel or abandon) with blocks still open: their held tails are published,
    /// redacted as the batch path would, never complete (DC-M4T09b, M4T09R-305).
    fn flush_held_tails(&mut self, out: &mut Vec<PublicPart>) {
        self.flush_pending_text(out);
        let open: Vec<String> = self
            .streams
            .iter()
            .filter(|(_, s)| s.holds_text())
            .map(|(k, _)| k.clone())
            .collect();
        let offset = self.received.unwrap_or(0);
        for block in open {
            let first = self.streams.get(&block).map_or(0, |s| s.pub_offset);
            out.extend(self.emit_stream_chunk(
                StreamMeta {
                    id: format!("tail/{block}/{first}"),
                    block,
                    offset,
                    part: 0,
                    model: None,
                },
                "",
                true,
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
        self.streams
            .entry(block.clone())
            .or_default()
            .received(delta);
        if !self.coalescing {
            out.extend(self.emit_stream_chunk(
                StreamMeta {
                    id,
                    block,
                    offset,
                    part: 0,
                    model: None,
                },
                delta,
                false,
                false,
            ));
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
        meta: StreamMeta,
        final_text: &str,
        out: &mut Vec<PublicPart>,
    ) {
        if self
            .pending_text
            .as_ref()
            .is_some_and(|p| p.block_key != meta.block)
        {
            self.flush_pending_text(out);
        }
        // The final snapshot adds only what extends the received text; a snapshot that differs adds nothing.
        let state = self.streams.entry(meta.block.clone()).or_default();
        let remaining = state.after_received(final_text).unwrap_or("");
        state.received(remaining);
        if let Some(mut pending) = self.pending_text.take() {
            pending.raw_text.push_str(remaining);
            let model = meta.model.or(pending.model);
            out.extend(self.emit_stream_chunk(
                StreamMeta {
                    id: pending.native_item_id,
                    block: pending.block_key,
                    offset: pending.received_offset,
                    part: meta.part,
                    model,
                },
                &pending.raw_text,
                true,
                true,
            ));
            return;
        }
        out.extend(self.emit_stream_chunk(meta, remaining, true, true));
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
    /// The stream ended: pending and held text (redacted per DC-M4T09b) and the aggregated diagnostics.
    pub fn flush(&mut self) -> Vec<PublicPart> {
        let mut out = Vec::new();
        self.flush_held_tails(&mut out);
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
    /// Any overdue coalesced text goes out first, so frames that publish nothing cannot starve the 50 ms flush
    /// (M4T09R-202).
    pub fn feed_at(&mut self, frame: &Value, offset: u64, now: Instant) -> Vec<PublicPart> {
        let mut out = self.flush_due(now);
        out.extend(self.feed_frame(frame, offset, now));
        out
    }
    fn feed_frame(&mut self, frame: &Value, offset: u64, now: Instant) -> Vec<PublicPart> {
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
                        ("user", "text") => {
                            let Some(input_id) = safe_discriminant(&f["input_id"])
                                .or_else(|| safe_discriminant(&m["input_id"]))
                            else {
                                continue;
                            };
                            self.flush_pending_text(&mut out);
                            let mut p = self.complete_text(
                                id.clone(),
                                format!("input/{input_id}"),
                                offset,
                                c["text"].as_str().unwrap_or(""),
                            );
                            p.speaker = Some("user".into());
                            p.input_id = Some(input_id);
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
                        StreamMeta {
                            id: key,
                            block: format!("message/{id}"),
                            offset,
                            part: 0,
                            model: None,
                        },
                        item["text"].as_str().unwrap_or(""),
                        &mut out,
                    );
                }
                "userMessage" | "user_message" if done => {
                    if let Some(input_id) = safe_discriminant(&item["input_id"])
                        .or_else(|| safe_discriminant(&params["input_id"]))
                    {
                        self.flush_pending_text(&mut out);
                        let mut p = self.complete_text(
                            key,
                            format!("input/{input_id}"),
                            offset,
                            &public_text(&item["content"]),
                        );
                        p.speaker = Some("user".into());
                        p.input_id = Some(input_id);
                        *handled = true;
                        out.push(p);
                    }
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
        } else if kind == "message_end" && m["role"] == "user" {
            if let Some(input_id) =
                safe_discriminant(&m["input_id"]).or_else(|| safe_discriminant(&f["input_id"]))
            {
                self.flush_pending_text(&mut out);
                let mut p = self.complete_text(
                    id,
                    format!("input/{input_id}"),
                    offset,
                    &public_text(&m["content"]),
                );
                p.speaker = Some("user".into());
                p.input_id = Some(input_id);
                *handled = true;
                out.push(p);
            }
        } else if kind == "message_end" && m["role"] == "assistant" {
            let model = public_name(&m["model"]);
            if let Some(cs) = m["content"].as_array() {
                for (i, c) in cs.iter().enumerate() {
                    if c["type"] == "text" {
                        let block =
                            format!("{}/text/{i}", self.pi_message.as_deref().unwrap_or(&id));
                        self.finish_stream_text(
                            StreamMeta {
                                id: id.clone(),
                                block,
                                offset,
                                part: i as u64,
                                model: model.clone(),
                            },
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
/// A line holding any of these (ASCII case-insensitive) is redacted whole.
const SECRET_KEYWORDS: [&str; 10] = [
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
];
fn line_contains_secret(line: &str) -> bool {
    let low = line.to_ascii_lowercase();
    SECRET_KEYWORDS.iter().any(|needle| low.contains(needle))
}
/// The one redaction function of both the batch and the streaming path (DC-M4T09b): a line with a secret keyword is
/// replaced whole.
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
/// Public output that was not persisted, shared by the reader's publisher and the host's drain and read at process end
/// even if the drain was aborted (M4T09R-300, M4T09R-301).
#[derive(Debug, Clone, Default)]
pub struct Loss(Arc<std::sync::Mutex<LossState>>);
#[derive(Debug, Default)]
struct LossState {
    summary: LossSummary,
    /// Parts handed to the queue and not yet settled by the drain, in queue order: (ticket, offset, bytes).
    queued: std::collections::VecDeque<(u64, u64, u64)>,
    next: u64,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LossSummary {
    /// First receive offset of any public output that was dropped or lost.
    pub first_offset: Option<u64>,
    /// Parts the drain dropped with their own gap recorded.
    pub dropped: u64,
    /// Parts lost without a gap of their own: their gap could not be written, the drain never settled them, or they
    /// could not be queued (closed queue, abandoned stream).
    pub unrecorded: u64,
    /// Body bytes of all dropped and lost parts.
    pub bytes: u64,
    /// Native frames not normalized because the public queue had closed.
    pub frames_after_close: u64,
}
impl LossSummary {
    /// Whether the loss is recorded nowhere but in a process-end gap.
    pub fn needs_gap(&self) -> bool {
        self.unrecorded > 0 || self.frames_after_close > 0
    }
    fn note(&mut self, offset: u64, bytes: u64) {
        self.first_offset = Some(self.first_offset.map_or(offset, |o| o.min(offset)));
        self.bytes += bytes;
    }
}
impl Loss {
    fn state(&self) -> std::sync::MutexGuard<'_, LossState> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn enqueue(&self, part: &PublicPart) -> Ticket<'_> {
        let mut s = self.state();
        let id = s.next;
        s.next += 1;
        s.queued
            .push_back((id, part.received_offset, part.text.len() as u64));
        Ticket {
            loss: self,
            id,
            armed: true,
        }
    }
    /// The drain settled the oldest queued part (persisted, or dropped and reported through [`Loss::dropped`]).
    ///
    /// Invariant (M4T09R-504): one task publishes into a channel (the stdout reader, through [`publish`] and
    /// [`try_publish`]) and one drain receives from it, so parts enter `queued` in channel order and the oldest entry is
    /// the part being settled. A second concurrent publisher could interleave enqueue and send, and would need settling
    /// by ticket id.
    pub fn settled(&self) {
        self.state().queued.pop_front();
    }
    /// The drain dropped a part's body; `gap_recorded` says whether its own gap was written.
    pub fn dropped(&self, part: &PublicPart, gap_recorded: bool) {
        let mut s = self.state();
        if gap_recorded {
            s.summary.dropped += 1;
        } else {
            s.summary.unrecorded += 1;
        }
        s.summary.note(part.received_offset, part.text.len() as u64);
    }
    fn lost(&self, part: &PublicPart) {
        let mut s = self.state();
        s.summary.unrecorded += 1;
        s.summary.note(part.received_offset, part.text.len() as u64);
    }
    pub(crate) fn frame_after_close(&self, offset: u64) {
        let mut s = self.state();
        s.summary.frames_after_close += 1;
        s.summary.note(offset, 0);
    }
    /// Everything lost so far; parts still queued count as lost (the drain is gone when this is read).
    pub fn summary(&self) -> LossSummary {
        let s = self.state();
        let mut summary = s.summary.clone();
        for (_, offset, bytes) in &s.queued {
            summary.unrecorded += 1;
            summary.note(*offset, *bytes);
        }
        summary
    }
}
/// A part entering the queue: dropped before it is in the channel, it is counted lost.
struct Ticket<'a> {
    loss: &'a Loss,
    id: u64,
    armed: bool,
}
impl Drop for Ticket<'_> {
    fn drop(&mut self) {
        if self.armed {
            let mut s = self.loss.state();
            if let Some(at) = s.queued.iter().position(|(id, _, _)| *id == self.id) {
                let (_, offset, bytes) = s.queued.remove(at).unwrap_or_default();
                s.summary.unrecorded += 1;
                s.summary.note(offset, bytes);
            }
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
    pub loss: Loss,
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
    loss: Loss,
    pub mode: Mode,
    pub version: String,
}
pub fn channel(mode: Mode, version: &str) -> (Publisher, Receiver) {
    let (sender, inner) = mpsc::channel(64);
    let pressure = Backpressure::default();
    let loss = Loss::default();
    (
        Publisher {
            sender,
            bytes: Arc::new(Semaphore::new(1 << 20)),
            pressure: pressure.clone(),
            loss: loss.clone(),
            mode,
            version: version.into(),
        },
        Receiver {
            inner,
            backpressure: pressure,
            loss,
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
    pub fn loss(&self) -> Loss {
        self.loss.clone()
    }
    pub async fn send(&self, part: PublicPart) -> Result<(), &'static str> {
        let count = u32::try_from(part.text.len().max(1)).map_err(|_| "public body too large")?;
        if count > 1 << 20 {
            return Err("public body exceeds one segment");
        }
        let mut ticket = self.loss.enqueue(&part);
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
        let sent = match self.sender.try_send(pending) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Closed(_)) => Err("public queue closed"),
            Err(mpsc::error::TrySendError::Full(p)) => {
                let _blocked = self.pressure.blocked();
                self.sender.send(p).await.map_err(|_| "public queue closed")
            }
        };
        ticket.armed = sent.is_err();
        sent
    }
    /// Queues without waiting (an abandoned stream cannot wait); what does not fit is counted lost.
    fn try_send(&self, part: PublicPart) -> bool {
        let Ok(count) = u32::try_from(part.text.len().max(1)) else {
            self.loss.lost(&part);
            return false;
        };
        let Ok(bytes) = self.bytes.clone().try_acquire_many_owned(count) else {
            self.loss.lost(&part);
            return false;
        };
        let mut ticket = self.loss.enqueue(&part);
        let sent = self
            .sender
            .try_send(Pending {
                part,
                _bytes: bytes,
            })
            .is_ok();
        ticket.armed = !sent;
        sent
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
/// Parts larger than one segment split on UTF-8 boundaries.
fn segments(parts: Vec<PublicPart>) -> Vec<PublicPart> {
    let mut out = Vec::new();
    for part in parts {
        if part.text.len() <= 1 << 20 {
            out.push(part);
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
            out.push(split);
            offset = end;
            segment += 1;
        }
    }
    out
}
/// Parts not yet handed to the queue when a publish ends early (closed queue, or the publish dropped on abandon) are
/// counted lost.
struct Unsent<'a> {
    loss: &'a Loss,
    parts: std::vec::IntoIter<PublicPart>,
}
impl Drop for Unsent<'_> {
    fn drop(&mut self) {
        for part in self.parts.by_ref() {
            self.loss.lost(&part);
        }
    }
}
/// Publishes in order, waiting for room. The parts are held by an [`Unsent`] from the call on, so a publish dropped
/// before it is first polled (a biased select that finds abandon latched) counts them lost too (M4T09R-501).
pub(crate) fn publish(
    sender: &Publisher,
    parts: Vec<PublicPart>,
) -> impl Future<Output = Result<(), &'static str>> + '_ {
    let mut unsent = Unsent {
        loss: &sender.loss,
        parts: segments(parts).into_iter(),
    };
    async move {
        for part in unsent.parts.by_ref() {
            sender.send(part).await?;
        }
        Ok(())
    }
}
/// Publishes without waiting; what does not fit now is counted lost.
pub(crate) fn try_publish(sender: &Publisher, parts: Vec<PublicPart>) {
    let mut unsent = Unsent {
        loss: &sender.loss,
        parts: segments(parts).into_iter(),
    };
    for part in unsent.parts.by_ref() {
        if !sender.try_send(part) {
            break;
        }
    }
}
