use super::*;
use hoh_core::public_output::PublicKind;
use serde_json::json;
#[test]
fn test_011_05_native_modes_map_messages_tool_boundaries_and_usage() {
    for (mode, version, fixture) in [
        (
            Mode::Claude,
            "2.1.318",
            include_str!("../compat/fixtures/claude/tool.jsonl"),
        ),
        (
            Mode::CodexAppServer,
            "0.166.0",
            include_str!("../compat/fixtures/codex/tool.jsonl"),
        ),
        (
            Mode::CodexExec,
            "0.166.0",
            include_str!("../compat/fixtures/codex-exec/tool.jsonl"),
        ),
        (
            Mode::Pi,
            "1.6.0",
            include_str!("../compat/fixtures/pi/tool.jsonl"),
        ),
    ] {
        let mut n = Normalizer::new(mode, version);
        let parts: Vec<_> = fixture
            .lines()
            .enumerate()
            .flat_map(|(offset, line)| n.feed(&serde_json::from_str(line).unwrap(), offset as u64))
            .collect();
        let kinds: Vec<_> = parts.iter().map(|p| p.kind).collect();
        assert!(kinds.contains(&PublicKind::Message), "{mode:?} message");
        let start = kinds
            .iter()
            .position(|k| *k == PublicKind::ToolStart)
            .expect("tool start");
        let end = kinds
            .iter()
            .position(|k| *k == PublicKind::ToolEnd)
            .expect("tool end");
        assert!(start < end, "{mode:?} ordered boundaries");
        assert_eq!(
            parts[start].block_key, parts[end].block_key,
            "same native call"
        );
        assert!(
            parts.iter().any(|p| p.text.contains("HOH_SUITE_OK")),
            "{mode:?} public answer"
        );
        assert!(
            parts.iter().any(|p| p.usage.is_some()),
            "{mode:?} explicit usage"
        );
    }
}
#[test]
fn test_011_05_hidden_fields_and_tool_credentials_never_cross_public_boundary() {
    const SECRET: &str = "M4T09_UNIQUE_SECRET_e8c420af";
    let mut n = Normalizer::new(Mode::Claude, "2.1.318");
    let parts=n.feed(&json!({"type":"assistant","uuid":"frame-1","session_id":"s","env":{"TOKEN":SECRET},"system_prompt":SECRET,"message":{"id":"m","content":[{"type":"thinking","thinking":SECRET,"signature":SECRET},{"type":"text","text":"visible"}]}}),0);
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0].text, "visible");
    assert!(!serde_json::to_string(&parts).unwrap().contains(SECRET));
    let parts=n.feed(&json!({"type":"user","uuid":"frame-2","message":{"content":[{"type":"tool_result","tool_use_id":"c","content":format!("Authorization: Bearer {SECRET}\nvisible result"),"is_error":false}]}}),1);
    assert_eq!(parts.len(), 1);
    assert!(parts[0].redacted);
    assert!(parts[0].text.contains("visible result"));
    assert!(!serde_json::to_string(&parts).unwrap().contains(SECRET));
}
#[tokio::test]
async fn test_011_05_queue_accounts_for_inflight_body_and_marks_only_actual_backpressure() {
    let (sender, mut receiver) = channel(Mode::Claude, "2.1.318");
    let mut n = Normalizer::new(Mode::Claude, "2.1.318");
    let mut part=n.feed(&json!({"type":"assistant","uuid":"q","message":{"id":"q","content":[{"type":"text","text":"q"}]}}),0).remove(0);
    part.text = "x".repeat(512 << 10);
    sender.send(part.clone()).await.unwrap();
    let first = receiver.recv().await.unwrap();
    sender.send(part.clone()).await.unwrap();
    let second = receiver.recv().await.unwrap();
    let publishing = sender.clone();
    let blocked = tokio::spawn(async move { publishing.send(part).await });
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert!(
        !blocked.is_finished(),
        "in-flight bodies still hold the 1 MiB reservation"
    );
    assert!(sender.backpressure().snapshot().1);
    drop(first);
    blocked.await.unwrap().unwrap();
    let delay = sender.backpressure().snapshot();
    assert!(!delay.1);
    assert!(delay.0 >= std::time::Duration::from_millis(20));
    drop(second);
}
#[test]
fn test_011_05_recorded_compaction_retry_and_resolved_model_are_explicit() {
    let mut pi = Normalizer::new(Mode::Pi, "1.6.0");
    let parts: Vec<_> = include_str!("../compat/fixtures/pi/compaction.jsonl")
        .lines()
        .enumerate()
        .flat_map(|(i, l)| pi.feed(&serde_json::from_str(l).unwrap(), i as u64))
        .collect();
    assert!(
        parts
            .iter()
            .any(|p| p.activity.as_deref() == Some("compaction_start"))
    );
    assert!(
        parts
            .iter()
            .any(|p| p.activity.as_deref() == Some("compaction_end"))
    );
    let mut claude = Normalizer::new(Mode::Claude, "2.1.318");
    let parts: Vec<_> =
        include_str!("../../../../tests/fixtures/protocol/claude/2.1.318/success/stdout.jsonl")
            .lines()
            .enumerate()
            .flat_map(|(i, l)| claude.feed(&serde_json::from_str(l).unwrap(), i as u64))
            .collect();
    assert!(parts.iter().any(|p| {
        p.model.as_deref() == Some("claude-opus-5-5")
            && p.usage
                .as_ref()
                .is_some_and(|u| u.context_limit == Some(1000000))
    }));
}
#[test]
fn test_011_05_replayed_native_delta_does_not_advance_later_offsets() {
    let mut n = Normalizer::new(Mode::CodexAppServer, "0.166.0");
    // Newline-terminated: DC-M4T09b holds an unterminated line until its newline arrives.
    let a = json!({"method":"item/agentMessage/delta","params":{"itemId":"m","delta":"one\n"}});
    let b = json!({"method":"item/agentMessage/delta","params":{"itemId":"m","delta":"two\n"}});
    assert_eq!(n.feed(&a, 10)[0].first_offset, 0);
    n.feed(&a, 10);
    let second = n.feed(&b, 11);
    assert_eq!(second[0].first_offset, 4, "replay must not invent a gap");
}
#[test]
fn test_011_05_schema_public_handoff_and_error_exclude_prompts_and_auth() {
    let mut n = Normalizer::new(Mode::CodexAppServer, "0.166.0");
    let frame = json!({"method":"item/completed","params":{"threadId":"t","turnId":"u","item":{"type":"collabAgentToolCall","id":"c","tool":"sendInput","status":"completed","prompt":"M4T09_HANDOFF_SECRET_81c9","reasoningEffort":"M4T09_HANDOFF_SECRET_81c9","senderThreadId":"t","receiverThreadIds":["other"],"agentsStates":{}}}});
    let p = n.feed(&frame, 0);
    assert!(
        p.iter()
            .any(|p| p.kind == PublicKind::ToolEnd && p.activity.as_deref() == Some("handoff"))
    );
    assert!(
        !serde_json::to_string(&p)
            .unwrap()
            .contains("M4T09_HANDOFF_SECRET_81c9")
    );
    let error = json!({"method":"error","params":{"threadId":"t","turnId":"u","willRetry":true,"error":{"message":"service unavailable","additionalDetails":"Authorization: Bearer M4T09_ERROR_SECRET_fa71"}}});
    let p = n.feed(&error, 1);
    assert!(
        p.iter()
            .any(|p| p.text == "service unavailable" && p.activity.as_deref() == Some("error"))
    );
    assert!(
        !serde_json::to_string(&p)
            .unwrap()
            .contains("M4T09_ERROR_SECRET_fa71")
    );
}

#[test]
fn test_011_05_recorded_failures_keep_public_errors_for_each_native_mode() {
    let cases = [
        (
            Mode::Claude,
            "2.1.318",
            include_str!(
                "../../../../tests/fixtures/protocol/claude/2.1.318/max-turns-error/stdout.jsonl"
            ),
            "Reached maximum",
        ),
        (
            Mode::CodexExec,
            "0.166.0",
            include_str!(
                "../../../../tests/fixtures/protocol/codex/0.166.0/exec-turn-failed/stdout.jsonl"
            ),
            "not supported",
        ),
        (
            Mode::Pi,
            "1.6.0",
            include_str!(
                "../../../../tests/fixtures/protocol/pi/1.6.0/rpc-invalid-model/stdout.jsonl"
            ),
            "Model not found",
        ),
    ];
    for (mode, version, fixture, expected) in cases {
        let mut normalizer = Normalizer::new(mode, version);
        let mut parts = Vec::new();
        for (offset, line) in fixture.lines().enumerate() {
            parts.extend(normalizer.feed(
                &serde_json::from_str(line).expect("recorded JSON"),
                offset as u64,
            ));
        }
        assert!(
            parts.iter().any(|p| p.kind == PublicKind::Diagnostic
                && p.activity.as_deref() == Some("error")
                && p.text.contains(expected)),
            "{mode:?}: explicit native failure remains public"
        );
    }
}

#[test]
fn test_011_05_failed_tools_are_not_presented_as_successful_boundaries() {
    for (mode, version, frame) in [
        (
            Mode::Claude,
            "2.1.318",
            serde_json::json!({"type":"user","uuid":"failure","message":{"content":[{"type":"tool_result","tool_use_id":"tool","content":"failed","is_error":true}]}}),
        ),
        (
            Mode::CodexAppServer,
            "0.166.0",
            serde_json::json!({"method":"item/completed","params":{"item":{"id":"tool","type":"commandExecution","status":"failed","exitCode":2,"aggregatedOutput":"failed"}}}),
        ),
        (
            Mode::CodexExec,
            "0.166.0",
            serde_json::json!({"type":"item.completed","item":{"id":"tool","type":"command_execution","status":"failed","exit_code":2,"aggregated_output":"failed"}}),
        ),
        (
            Mode::Pi,
            "1.6.0",
            serde_json::json!({"type":"tool_execution_end","toolCallId":"tool","isError":true,"result":{"content":[{"type":"text","text":"failed"}]}}),
        ),
    ] {
        let parts = Normalizer::new(mode, version).feed(&frame, 0);
        assert!(
            parts
                .iter()
                .any(|p| p.kind == PublicKind::ToolEnd
                    && p.activity.as_deref() == Some("tool_error")),
            "{mode:?}"
        );
    }
}

#[test]
fn test_011_05_pi_message_update_deltas_with_shared_response_id_are_not_dropped() {
    let mut n = Normalizer::new(Mode::Pi, "1.6.0");
    let start = json!({"type":"message_start","message":{"role":"assistant","responseId":"resp-1","content":[]}});
    let d1 = json!({"type":"message_update","message":{"role":"assistant","responseId":"resp-1"},"assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"hello\n"}});
    let d2 = json!({"type":"message_update","message":{"role":"assistant","responseId":"resp-1"},"assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"world\n"}});
    let end = json!({"type":"message_end","message":{"role":"assistant","responseId":"resp-1","content":[{"type":"text","text":"hello\nworld\n"}]}});
    let _ = n.feed(&start, 0);
    let p1 = n.feed(&d1, 1);
    let p2 = n.feed(&d2, 2);
    let p3 = n.feed(&end, 3);
    assert_eq!(p1.len(), 1);
    // Newline-terminated deltas: DC-M4T09b publishes nothing after a block's last newline before it arrives.
    assert_eq!(p1[0].text, "hello\n");
    assert_eq!(
        p2.len(),
        1,
        "second text_delta with same responseId must not be dropped"
    );
    assert_eq!(p2[0].text, "world\n");
    assert_eq!(p2[0].first_offset, 6);
    assert_eq!(p1[0].block_key, p2[0].block_key);
    assert_ne!(
        p1[0].native_item_id, p2[0].native_item_id,
        "each message_update delta must keep a distinct native_item_id"
    );
    assert!(
        p3.iter()
            .any(|p| p.kind == PublicKind::Message && p.complete && p.text.is_empty())
    );
}

#[test]
fn test_011_05_claude_two_text_blocks_same_message_including_non_ascii_boundary() {
    let mut n = Normalizer::new(Mode::Claude, "2.1.318");
    let a = n.feed(
        &json!({"type":"assistant","uuid":"u1","session_id":"s","message":{"id":"msg_1","content":[{"type":"text","text":"First paragraph."}]}}),
        0,
    );
    let b = n.feed(
        &json!({"type":"assistant","uuid":"u2","session_id":"s","message":{"id":"msg_1","content":[{"type":"text","text":"Second paragraph is longer."}]}}),
        1,
    );
    assert_eq!(a.len(), 1);
    assert_eq!(b.len(), 1);
    assert_ne!(
        a[0].block_key, b[0].block_key,
        "distinct Claude assistant frames of one message must have distinct block keys"
    );
    assert_eq!(b[0].first_offset, 0);
    assert_eq!(
        b[0].text, "Second paragraph is longer.",
        "second text block must not be sliced by first block offset"
    );

    // Non-ASCII boundary: first block is 2 ASCII bytes; byte offset 2 falls inside the 3-byte UTF-8 '中'.
    let c = n.feed(
        &json!({"type":"assistant","uuid":"u3","session_id":"s","message":{"id":"msg_2","content":[{"type":"text","text":"ab"}]}}),
        2,
    );
    let d = n.feed(
        &json!({"type":"assistant","uuid":"u4","session_id":"s","message":{"id":"msg_2","content":[{"type":"text","text":"中文第二段"}]}}),
        3,
    );
    assert_eq!(c.len(), 1);
    assert_eq!(d.len(), 1);
    assert_ne!(c[0].block_key, d[0].block_key);
    assert_eq!(d[0].first_offset, 0);
    assert_eq!(
        d[0].text, "中文第二段",
        "non-ASCII second block must not be emptied by char-boundary slice"
    );
}

#[test]
fn test_011_05_split_credential_across_deltas_is_redacted_without_misaligning_final() {
    let mut n = Normalizer::new(Mode::CodexAppServer, "0.166.0");
    let d1 = n.feed(
        &json!({"method":"item/agentMessage/delta","params":{"itemId":"m","delta":"my pass"}}),
        0,
    );
    let d2 = n.feed(
        &json!({"method":"item/agentMessage/delta","params":{"itemId":"m","delta":"word is hunter2\n"}}),
        1,
    );
    let done = n.feed(
        &json!({"method":"item/completed","params":{"item":{"type":"agentMessage","id":"m","text":"my password is hunter2\nvisible tail"}}}),
        2,
    );
    let all: Vec<_> = d1.into_iter().chain(d2).chain(done).collect();
    let combined: String = all.iter().map(|p| p.text.as_str()).collect();
    assert!(
        !combined.contains("hunter2"),
        "split credential must never appear in plaintext: {combined:?}"
    );
    assert!(
        all.iter().any(|p| p.redacted),
        "split credential must be marked redacted: {all:?}"
    );
    assert!(
        combined.contains("visible tail"),
        "redaction length change must not misalign final snapshot tail: {combined:?}"
    );
}

#[test]
fn test_011_05_unmapped_and_unsupported_frames_are_aggregated_with_kind_and_count() {
    let mut pi = Normalizer::new(Mode::Pi, "1.6.0");
    let mut emitted = Vec::new();
    for offset in 0..47u64 {
        emitted.extend(pi.feed(
            &json!({"type":"message_update","assistantMessageEvent":{"type":"thinking_delta","delta":"secret thought"}}),
            offset,
        ));
    }
    assert!(
        emitted.len() <= 2,
        "47 unmapped thinking_delta frames must be aggregated, got {} diagnostics",
        emitted.len()
    );
    assert_eq!(
        emitted[0].frame_kind.as_deref(),
        Some("message_update/thinking_delta"),
        "diagnostic must record frame kind"
    );
    assert_eq!(emitted[0].count, Some(1));
    let summary = pi.flush();
    assert_eq!(summary.len(), 1);
    assert_eq!(
        summary[0].frame_kind.as_deref(),
        Some("message_update/thinking_delta")
    );
    assert_eq!(summary[0].count, Some(47));
    assert_eq!(summary[0].block_key, emitted[0].block_key);

    let mut unsupported = Normalizer::new(Mode::Claude, "9.9.9-unknown");
    let mut u_parts = Vec::new();
    for offset in 0..10u64 {
        u_parts.extend(unsupported.feed(
            &json!({"type":"assistant","uuid":format!("u{offset}")}),
            offset,
        ));
    }
    assert!(
        u_parts.len() <= 2,
        "10 unsupported version frames must be aggregated, got {}",
        u_parts.len()
    );
    assert_eq!(u_parts[0].frame_kind.as_deref(), Some("assistant"));
    assert_eq!(u_parts[0].count, Some(1));
    let u_summary = unsupported.flush();
    assert_eq!(u_summary.len(), 1);
    assert_eq!(u_summary[0].frame_kind.as_deref(), Some("assistant"));
    assert_eq!(u_summary[0].count, Some(10));
}

#[test]
fn test_011_05_coalescer_flushes_on_50ms_window_or_16kib_cap_and_preserves_offsets() {
    let t0 = Instant::now();
    let mut n = Normalizer::new_coalescing(Mode::CodexAppServer, "0.166.0");
    let p0 = n.feed_at(
        &json!({"method":"item/agentMessage/delta","params":{"itemId":"m","delta":"hello "}}),
        0,
        t0,
    );
    assert!(p0.is_empty(), "small delta buffers within 50 ms / 16 KiB");
    let p1 = n.feed_at(
        &json!({"method":"item/agentMessage/delta","params":{"itemId":"m","delta":"world\n"}}),
        1,
        t0 + Duration::from_millis(20),
    );
    assert!(p1.is_empty());
    assert!(n.flush_due(t0 + Duration::from_millis(49)).is_empty());
    let flushed = n.flush_due(t0 + Duration::from_millis(50));
    assert_eq!(flushed.len(), 1);
    assert_eq!(flushed[0].text, "hello world\n");
    assert_eq!(flushed[0].first_offset, 0);

    // 16 KiB size cap flushes immediately without waiting for 50 ms (a whole line, so redaction withholds nothing).
    let big = format!("{}\n", "a".repeat(COALESCE_BYTES - 1));
    let p2 = n.feed_at(
        &json!({"method":"item/agentMessage/delta","params":{"itemId":"m","delta":big}}),
        2,
        t0 + Duration::from_millis(51),
    );
    assert_eq!(p2.len(), 1);
    assert_eq!(p2[0].first_offset, 12);
    assert_eq!(p2[0].text.len(), COALESCE_BYTES);
}

#[tokio::test]
async fn test_011_05_publish_splits_over_one_mib_on_utf8_boundaries() {
    let (sender, mut receiver) = channel(Mode::Claude, "2.1.318");
    let mut part = Normalizer::new(Mode::Claude, "2.1.318")
        .feed(
            &json!({"type":"assistant","uuid":"big-1","message":{"id":"m","content":[{"type":"text","text":"x"}]}}),
            0,
        )
        .remove(0);
    // 1 MiB - 1 ASCII bytes + 3-byte UTF-8 '中' straddling the 1 MiB boundary + tail.
    let prefix = "a".repeat((1 << 20) - 1);
    let body = format!("{prefix}中{}", "b".repeat(1024));
    part.first_offset = 10;
    part.text = body.clone();
    part.complete = true;
    let drain = tokio::spawn(async move {
        let mut out = Vec::new();
        while let Some(pending) = receiver.recv().await {
            out.push(pending.part);
        }
        out
    });
    publish(&sender, vec![part]).await.unwrap();
    drop(sender);
    let got = drain.await.unwrap();
    assert_eq!(got.len(), 2, "over-1MiB payload must split into 2 segments");
    assert_eq!(got[0].text.len(), (1 << 20) - 1);
    assert_eq!(got[0].first_offset, 10);
    assert!(!got[0].complete);
    assert!(got[0].native_item_id.ends_with("/segment/0"));
    assert_eq!(got[1].first_offset, 10 + ((1 << 20) - 1) as u64);
    assert!(got[1].text.starts_with('中'));
    assert!(got[1].complete);
    assert!(got[1].native_item_id.ends_with("/segment/1"));
    assert_eq!(got[0].block_key, got[1].block_key);
    assert_eq!(format!("{}{}", got[0].text, got[1].text), body);
}

#[test]
fn test_011_05_cross_mode_equality_explicit_null_capabilities_and_native_echo_mapping() {
    let mut signatures = Vec::new();
    for (mode, version, fixture) in [
        (
            Mode::Claude,
            "2.1.318",
            include_str!("../compat/fixtures/claude/tool.jsonl"),
        ),
        (
            Mode::CodexAppServer,
            "0.166.0",
            include_str!("../compat/fixtures/codex/tool.jsonl"),
        ),
        (
            Mode::CodexExec,
            "0.166.0",
            include_str!("../compat/fixtures/codex-exec/tool.jsonl"),
        ),
        (
            Mode::Pi,
            "1.6.0",
            include_str!("../compat/fixtures/pi/tool.jsonl"),
        ),
    ] {
        let mut n = Normalizer::new(mode, version);
        let parts: Vec<_> = fixture
            .lines()
            .enumerate()
            .flat_map(|(offset, line)| n.feed(&serde_json::from_str(line).unwrap(), offset as u64))
            .collect();
        let tool_start = parts
            .iter()
            .position(|p| p.kind == PublicKind::ToolStart)
            .expect("tool start");
        let tool_end = parts
            .iter()
            .position(|p| p.kind == PublicKind::ToolEnd)
            .expect("tool end");
        let msg = parts
            .iter()
            .find(|p| p.kind == PublicKind::Message && p.text.contains("HOH_SUITE_OK"))
            .expect("assistant message");
        let usage_part = parts
            .iter()
            .find(|p| p.kind == PublicKind::Activity && p.activity.as_deref() == Some("usage"))
            .expect("usage activity");
        let usage = usage_part.usage.as_ref().expect("usage struct");
        let usage_json = serde_json::to_value(usage).unwrap();
        // Explicit null for unknown capability fields per mode.
        if matches!(mode, Mode::Pi | Mode::CodexExec) {
            assert_eq!(
                usage_json.as_object().unwrap().get("context_limit"),
                Some(&Value::Null),
                "{mode:?}: unknown context_limit must be explicit null"
            );
        }
        if matches!(mode, Mode::CodexAppServer | Mode::CodexExec) {
            assert_eq!(
                usage_json.as_object().unwrap().get("cost_usd"),
                Some(&Value::Null),
                "{mode:?}: unknown cost_usd must be explicit null"
            );
        }
        signatures.push((
            tool_start < tool_end,
            parts[tool_start].block_key == parts[tool_end].block_key,
            parts[tool_start].stream.clone(),
            parts[tool_end].stream.clone(),
            parts[tool_end].complete,
            msg.speaker.clone(),
            msg.text.trim().to_owned(),
            msg.complete,
            usage.input_tokens.is_some(),
            usage.output_tokens.is_some(),
        ));
    }
    for sig in &signatures[1..] {
        assert_eq!(
            &signatures[0], sig,
            "all native modes must produce equal normalized public shape, order, and identity invariants"
        );
    }

    // Native user echo carrying input_id maps with speaker=user and input_id across all modes; without input_id stays private.
    for (mode, version, with_id, without_id) in [
        (
            Mode::Claude,
            "2.1.318",
            json!({"type":"user","uuid":"e1","input_id":"in-1","message":{"role":"user","content":[{"type":"text","text":"hello"}]}}),
            json!({"type":"user","uuid":"e2","message":{"role":"user","content":[{"type":"text","text":"internal prompt"}]}}),
        ),
        (
            Mode::CodexAppServer,
            "0.166.0",
            json!({"method":"item/completed","params":{"item":{"type":"userMessage","id":"u1","input_id":"in-1","content":[{"type":"text","text":"hello"}]}}}),
            json!({"method":"item/completed","params":{"item":{"type":"userMessage","id":"u2","content":[{"type":"text","text":"internal prompt"}]}}}),
        ),
        (
            Mode::Pi,
            "1.6.0",
            json!({"type":"message_end","message":{"role":"user","input_id":"in-1","content":[{"type":"text","text":"hello"}]}}),
            json!({"type":"message_end","message":{"role":"user","content":[{"type":"text","text":"internal prompt"}]}}),
        ),
    ] {
        let mut n = Normalizer::new(mode, version);
        let mapped = n.feed(&with_id, 0);
        assert_eq!(mapped.len(), 1, "{mode:?} correlated echo");
        assert_eq!(mapped[0].kind, PublicKind::Message);
        assert_eq!(mapped[0].speaker.as_deref(), Some("user"));
        assert_eq!(mapped[0].input_id.as_deref(), Some("in-1"));
        let hidden = n.feed(&without_id, 1);
        assert!(
            hidden.iter().all(|p| p.kind != PublicKind::Message),
            "{mode:?} uncorrelatable prompt echo must not emit public.message"
        );
    }
}

/// Deterministic xorshift64 so every split and flush point is reproducible from its seed.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

fn random_split(text: &str, rng: &mut Rng) -> Vec<String> {
    let bounds: Vec<usize> = (1..text.len())
        .filter(|i| text.is_char_boundary(*i))
        .collect();
    let mut cuts: Vec<usize> = if bounds.is_empty() {
        Vec::new()
    } else {
        (0..rng.below(12))
            .map(|_| bounds[rng.below(bounds.len() as u64) as usize])
            .collect()
    };
    cuts.sort_unstable();
    cuts.dedup();
    let mut out = Vec::new();
    let mut at = 0;
    for cut in cuts.into_iter().chain([text.len()]) {
        out.push(text[at..cut].to_owned());
        at = cut;
    }
    out
}

/// Offsets are contiguous and each part is flagged `redacted` exactly when redaction altered it, that is when it
/// carries a `[redacted]` marker (the texts here never contain one themselves); returns the concatenation.
fn assert_parts_contiguous_and_flagged(parts: &[&PublicPart], label: &str) -> String {
    let mut at = 0u64;
    for p in parts {
        assert_eq!(p.first_offset, at, "{label}: contiguous offsets {parts:?}");
        at += p.text.len() as u64;
        assert_eq!(
            p.redacted,
            p.text.contains("[redacted]"),
            "{label}: redacted flag iff redaction altered the part: {p:?}"
        );
    }
    parts.iter().map(|p| p.text.as_str()).collect()
}

/// Asserts DC-M4T09b on one block's published parts: concatenation equals the batch redaction of `full`, offsets are
/// contiguous, and each part is marked redacted exactly when redaction altered it.
fn assert_block_invariant(parts: &[&PublicPart], full: &str, label: &str) {
    let combined = assert_parts_contiguous_and_flagged(parts, label);
    assert_eq!(combined, redact(full).0, "{label}: streamed == batch");
}

/// The longest line of `full`, newline included.
fn longest_line(full: &str) -> usize {
    full.split_inclusive('\n').map(str::len).max().unwrap_or(0)
}

/// M4T09R-400: no byte of `secret` in `full` within [`WITHHELD_WINDOW`] bytes before the end of a keyword on its line
/// may appear in `published`. Every test text places each secret once, before its keyword on the same line.
fn assert_no_secret_within_window(published: &str, full: &str, secret: &str, label: &str) {
    for line in full.split_inclusive('\n') {
        let (Some(at), Some(end)) = (line.find(secret), keyword_end(line)) else {
            continue;
        };
        if end <= at + secret.len() + WITHHELD_WINDOW {
            assert!(
                !published.contains(secret),
                "{label}: a secret within the window of its keyword was published"
            );
        }
    }
}

/// End (exclusive) of the first secret keyword in `line`: the shortest prefix [`line_contains_secret`] flags.
fn keyword_end(line: &str) -> Option<usize> {
    let low = line.to_ascii_lowercase();
    SECRET_KEYWORDS
        .iter()
        .filter_map(|k| low.find(k).map(|at| at + k.len()))
        .min()
}

#[derive(Clone, Copy, Debug)]
enum Ending {
    Completed,
    ProcessEnd,
}

fn drive(mode: Mode, coalescing: bool, full: &str, seed: u64, ending: Ending) -> Vec<PublicPart> {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let version = if mode == Mode::Pi { "1.6.0" } else { "0.166.0" };
    let mut n = if coalescing {
        Normalizer::new_coalescing(mode, version)
    } else {
        Normalizer::new(mode, version)
    };
    let t0 = Instant::now();
    let mut now = t0;
    let mut offset = 0u64;
    let mut out = Vec::new();
    let mut feed = |n: &mut Normalizer, frame: Value, now: Instant, out: &mut Vec<PublicPart>| {
        out.extend(n.feed_at(&frame, offset, now));
        offset += 1;
    };
    if mode == Mode::Pi {
        feed(
            &mut n,
            json!({"type":"message_start","message":{"role":"assistant","responseId":"r","content":[]}}),
            now,
            &mut out,
        );
    }
    for (k, delta) in random_split(full, &mut rng).into_iter().enumerate() {
        now += Duration::from_millis(rng.below(80));
        let frame = if mode == Mode::Pi {
            json!({"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":delta}})
        } else {
            json!({"method":"item/agentMessage/delta","params":{"itemId":"m","delta":delta}})
        };
        feed(&mut n, frame, now, &mut out);
        match rng.below(6) {
            0 | 1 => out.extend(n.flush_due(now + Duration::from_millis(rng.below(80)))),
            2 => {
                // A non-coalescible tool boundary in between.
                let tool = if mode == Mode::Pi {
                    json!({"type":"tool_execution_start","toolCallId":format!("c{k}"),"toolName":"bash","args":{"command":"ls"}})
                } else {
                    json!({"method":"item/started","params":{"item":{"type":"commandExecution","id":format!("c{k}"),"command":"ls"}}})
                };
                feed(&mut n, tool, now, &mut out);
            }
            3 => out.extend(n.flush_text()),
            _ => {}
        }
    }
    match ending {
        Ending::Completed => {
            let done = if mode == Mode::Pi {
                json!({"type":"message_end","message":{"role":"assistant","responseId":"r","content":[{"type":"text","text":full}]}})
            } else {
                json!({"method":"item/completed","params":{"item":{"type":"agentMessage","id":"m","text":full}}})
            };
            feed(&mut n, done, now, &mut out);
        }
        Ending::ProcessEnd => out.extend(n.flush()),
    }
    out
}

const PROPERTY_TEXTS: &[&str] = &[
    "hunter2 is my password\n",
    "my password is hunter2\nvisible tail",
    "line one\nsk-LIVE-hunter2 is my secret\nok line\napi_key=abc\n中文 token=xyz\nfin",
    "no secrets here\njust text\n",
    "Authorization: Bearer abc\n\n\nend hunter2 password",
    "中文第一行\n第二行 hunter2 秘密 password\n尾",
    "short\nhunter2 then access_token later on the same line\nvisible\n",
];

/// Lines near the 16 KiB cap and near the cap plus the withheld window (M4T09R-400, -401): a credential just before
/// the boundary with its keyword one byte after it, multi-byte UTF-8 straddling it, clean long lines, and lines whose
/// keyword lies far beyond the window.
fn near_cap_texts() -> Vec<String> {
    let mut texts = Vec::new();
    for limit in [COALESCE_BYTES, COALESCE_BYTES + WITHHELD_WINDOW] {
        for len in [limit - 1, limit, limit + 1] {
            // A credential ending at `len` with its keyword starting one byte later.
            texts.push(format!("{}hunter2 password\nafter\n", "a".repeat(len - 7)));
            // A clean line of exactly `len` bytes, newline included.
            texts.push(format!("{}\nnext\n", "b".repeat(len - 1)));
            // Multi-byte characters across the boundary, then the credential and its keyword.
            let wide = "中".repeat(len / 3 + 1);
            texts.push(format!("{wide}hunter2 secret\n中文"));
            // An unterminated tail of `len` bytes holding a credential and its keyword.
            texts.push(format!("ok\n{}hunter2 token=v", "c".repeat(len - 15)));
        }
    }
    // Keyword far beyond the window: only its own line's late bytes are redacted.
    texts.push(format!(
        "{}{}hunter2 api_key=K\nclean\n",
        "d".repeat(COALESCE_BYTES + WITHHELD_WINDOW + 4000),
        "e".repeat(100)
    ));
    texts.push(format!(
        "head\n{}\ntail password\n",
        "f".repeat(3 * (COALESCE_BYTES + WITHHELD_WINDOW))
    ));
    texts
}

#[test]
fn test_011_05_streamed_redaction_equals_batch_for_any_split_and_flush_timing() {
    let long = format!(
        "{}hunter2 private_key=K\n{}",
        "x".repeat(3000),
        "y".repeat(5000)
    );
    let near = near_cap_texts();
    let texts: Vec<&str> = PROPERTY_TEXTS
        .iter()
        .copied()
        .chain([long.as_str()])
        .chain(near.iter().map(String::as_str))
        .collect();
    let mut runs = 0;
    for full in &texts {
        for mode in [Mode::CodexAppServer, Mode::Pi] {
            for coalescing in [true, false] {
                for ending in [Ending::Completed, Ending::ProcessEnd] {
                    for seed in 1..=60u64 {
                        let label = format!(
                            "{mode:?} coalescing={coalescing} {ending:?} seed={seed} text={full:?}"
                        );
                        let parts = drive(mode, coalescing, full, seed, ending);
                        let block: Vec<&PublicPart> = parts
                            .iter()
                            .filter(|p| p.kind == PublicKind::Message)
                            .collect();
                        let label = format!("{label:.300} len={}", full.len());
                        if longest_line(full) < COALESCE_BYTES + WITHHELD_WINDOW {
                            // Every line fits the cap plus the window: nothing is published before its line ends.
                            assert!(
                                !block.iter().any(|p| p.text.contains("hunter2")),
                                "{label}: secret published"
                            );
                            assert_block_invariant(&block, full, &label);
                        } else {
                            // A longer line: only bytes more than the window before a keyword may be published.
                            let combined = assert_parts_contiguous_and_flagged(&block, &label);
                            assert_no_secret_within_window(&combined, full, "hunter2", &label);
                            if !full.split_inclusive('\n').any(line_contains_secret) {
                                assert_eq!(
                                    combined,
                                    redact(full).0,
                                    "{label}: clean text verbatim"
                                );
                            }
                            let batch = redact(full).0;
                            for ((raw, streamed), batch) in full
                                .split_inclusive('\n')
                                .zip(combined.split_inclusive('\n'))
                                .zip(batch.split_inclusive('\n'))
                            {
                                if raw.len() < COALESCE_BYTES + WITHHELD_WINDOW {
                                    assert_eq!(
                                        streamed, batch,
                                        "{label}: a short line equals batch"
                                    );
                                }
                            }
                        }
                        let complete = block.iter().filter(|p| p.complete).count();
                        match ending {
                            Ending::Completed => {
                                assert_eq!(complete, 1, "{label}: one completion");
                                assert!(block.last().is_some_and(|p| p.complete), "{label}");
                            }
                            Ending::ProcessEnd => assert_eq!(complete, 0, "{label}"),
                        }
                        runs += 1;
                    }
                }
            }
        }
    }
    assert!(runs >= 1000, "property runs: {runs}");
    // Cross-mode: a complete (batch) block of the same text is identical.
    for full in &texts {
        let mut claude = Normalizer::new(Mode::Claude, "2.1.318");
        let parts = claude.feed(
            &json!({"type":"assistant","uuid":"u","message":{"id":"m","content":[{"type":"text","text":full}]}}),
            0,
        );
        assert_eq!(parts[0].text, redact(full).0);
    }
}

#[test]
fn test_011_05_secret_before_keyword_never_flushes_ahead_of_its_keyword() {
    for coalescing in [true, false] {
        let t0 = Instant::now();
        let mut n = if coalescing {
            Normalizer::new_coalescing(Mode::CodexAppServer, "0.166.0")
        } else {
            Normalizer::new(Mode::CodexAppServer, "0.166.0")
        };
        let mut out = n.feed_at(
            &json!({"method":"item/agentMessage/delta","params":{"itemId":"m","delta":"hunter2 is my "}}),
            0,
            t0,
        );
        // The 50 ms window and a tool boundary both pass before the keyword arrives.
        out.extend(n.flush_due(t0 + Duration::from_millis(60)));
        out.extend(n.feed_at(
            &json!({"method":"item/started","params":{"item":{"type":"commandExecution","id":"c","command":"ls"}}}),
            1,
            t0 + Duration::from_millis(61),
        ));
        out.extend(n.feed_at(
            &json!({"method":"item/agentMessage/delta","params":{"itemId":"m","delta":"password\n"}}),
            2,
            t0 + Duration::from_millis(200),
        ));
        out.extend(n.flush_due(t0 + Duration::from_millis(300)));
        let msgs: Vec<&PublicPart> = out
            .iter()
            .filter(|p| p.kind == PublicKind::Message)
            .collect();
        assert!(
            !msgs.iter().any(|p| p.text.contains("hunter2")),
            "coalescing={coalescing}: {msgs:?}"
        );
        assert_block_invariant(&msgs, "hunter2 is my password\n", "secret-first");
    }
}

fn stream(n: &mut Normalizer, deltas: &[&str]) -> Vec<PublicPart> {
    let mut out = Vec::new();
    for (i, delta) in deltas.iter().enumerate() {
        out.extend(n.feed(
            &json!({"method":"item/agentMessage/delta","params":{"itemId":"m","delta":delta}}),
            i as u64,
        ));
    }
    out.extend(n.flush());
    out
}

fn normalizers() -> [Normalizer; 2] {
    [
        Normalizer::new(Mode::CodexAppServer, "0.166.0"),
        Normalizer::new_coalescing(Mode::CodexAppServer, "0.166.0"),
    ]
}

#[test]
fn test_011_05_credential_within_the_window_of_its_keyword_is_never_published() {
    let cap = COALESCE_BYTES;
    let window = WITHHELD_WINDOW;
    // M4T09R-400 X1: a 7-byte credential ending 4 bytes before the cap, its keyword one byte after the next delta.
    let head = format!("{}hunter2", "a".repeat(cap - 11));
    for mut n in normalizers() {
        let out = stream(&mut n, &[&head, " password\n"]);
        let parts: Vec<&PublicPart> = out.iter().collect();
        assert_block_invariant(&parts, &format!("{head} password\n"), "X1");
    }
    // The credential ends just before, at and just after the cap and the cap plus the window; multi-byte padding moves
    // the boundary inside a character.
    for limit in [cap, cap + window] {
        for end in [limit - 1, limit, limit + 1] {
            for pad in ["a", "é", "中"] {
                let n_pad = (end - 7).div_ceil(pad.len());
                let head = format!("{}hunter2", pad.repeat(n_pad));
                let full = format!("{head} secret\nafter\n");
                for mut n in normalizers() {
                    let out = stream(&mut n, &[&head, " secret\n", "after\n"]);
                    let parts: Vec<&PublicPart> = out.iter().collect();
                    let label = format!("limit={limit} end={end} pad={pad}");
                    let combined = assert_parts_contiguous_and_flagged(&parts, &label);
                    assert!(
                        !combined.contains("hunter2"),
                        "{label}: credential published"
                    );
                    assert!(combined.ends_with("[redacted]\nafter\n"), "{label}");
                    if full.len() < cap + window {
                        assert_eq!(combined, redact(&full).0, "{label}");
                    }
                }
            }
        }
    }
    // The exact bound: a keyword ending within the window after the credential always redacts it, whatever the split.
    for gap in [window - 8, window - 9, 1000] {
        let full = format!("hunter2{}password\n", "b".repeat(gap));
        let chunks: Vec<&str> = full
            .as_bytes()
            .chunks(997)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect();
        for mut n in normalizers() {
            let out = stream(&mut n, &chunks);
            let parts: Vec<&PublicPart> = out.iter().collect();
            let combined = assert_parts_contiguous_and_flagged(&parts, &format!("gap={gap}"));
            assert!(
                !combined.contains("hunter2"),
                "gap={gap}: credential published"
            );
        }
    }
}

#[test]
fn test_011_05_long_line_publishes_only_bytes_older_than_the_window() {
    let cap = COALESCE_BYTES;
    let window = WITHHELD_WINDOW;
    let line = "z".repeat(5 * (cap + window));
    for mut n in normalizers() {
        let mut published = 0usize;
        let mut received = 0usize;
        let mut first_early = None;
        for (i, chunk) in line.as_bytes().chunks(1000).enumerate() {
            received += chunk.len();
            let out = n.feed(
                &json!({"method":"item/agentMessage/delta","params":{"itemId":"m","delta":std::str::from_utf8(chunk).unwrap()}}),
                i as u64,
            );
            published += out.iter().map(|p| p.text.len()).sum::<usize>();
            if published > 0 && first_early.is_none() {
                first_early = Some(received);
            }
            let held: usize = n.streams.values().map(|s| s.tail.len()).sum();
            let pending = n.pending_text.as_ref().map_or(0, |p| p.raw_text.len());
            assert!(
                received - published >= window.min(received),
                "the last {window} bytes of an open line are withheld: received={received} published={published}"
            );
            assert!(
                held + pending < cap + window + cap + 1000,
                "the withheld tail is bounded: held={held} pending={pending}"
            );
        }
        assert!(
            first_early.is_some_and(|r| r <= cap + window + cap + 1000),
            "a long line is published before it ends: {first_early:?}"
        );
        let mut out = n.feed(
            &json!({"method":"item/agentMessage/delta","params":{"itemId":"m","delta":"\n"}}),
            line.len() as u64,
        );
        out.extend(n.flush());
        published += out.iter().map(|p| p.text.len()).sum::<usize>();
        assert_eq!(
            published,
            line.len() + 1,
            "the whole line is published at its newline"
        );
    }
}

#[test]
fn test_011_05_redacted_flag_marks_parts_altered_by_redaction_and_not_a_bare_terminator() {
    let cap = COALESCE_BYTES;
    let window = WITHHELD_WINDOW;
    // A keyword early in a line longer than the cap plus the window: the line is redacted as soon as it is published,
    // its later bytes are suppressed, and its newline and the next line are published unaltered and unflagged.
    let line = format!("hunter2 password {}", "x".repeat(2 * (cap + window)));
    let chunks: Vec<&str> = line
        .as_bytes()
        .chunks(1000)
        .map(|c| std::str::from_utf8(c).unwrap())
        .chain(["\nnext\n"])
        .collect();
    for mut n in normalizers() {
        let out = stream(&mut n, &chunks);
        let parts: Vec<&PublicPart> = out.iter().collect();
        let full = format!("{line}\nnext\n");
        assert_block_invariant(&parts, &full, "early keyword");
        let texts: Vec<(&str, bool)> = out.iter().map(|p| (p.text.as_str(), p.redacted)).collect();
        assert_eq!(
            texts,
            [("[redacted]", true), ("\nnext\n", false)],
            "the bare terminator after a redacted unit is not flagged"
        );
    }
    // A keyword straddling where the first early publication cuts the line is still seen.
    let head = format!("{}pass", "a".repeat(cap - 4));
    let rest = format!("word{}", "b".repeat(window + 100));
    for mut n in normalizers() {
        let out = stream(&mut n, &[&head, &rest, "hunter2\n"]);
        let parts: Vec<&PublicPart> = out.iter().collect();
        assert_block_invariant(&parts, &format!("{head}{rest}hunter2\n"), "straddle");
    }
}

#[test]
fn test_011_05_unmapped_frames_do_not_starve_the_coalescing_flush() {
    let t0 = Instant::now();
    let mut n = Normalizer::new_coalescing(Mode::CodexAppServer, "0.166.0");
    let unmapped =
        json!({"method":"item/reasoning/textDelta","params":{"itemId":"r","delta":"hidden"}});
    let _ = n.feed_at(&unmapped, 0, t0);
    assert!(
        n.feed_at(
            &json!({"method":"item/agentMessage/delta","params":{"itemId":"m","delta":"visible\n"}}),
            1,
            t0 + Duration::from_millis(1),
        )
        .is_empty()
    );
    let out = n.feed_at(&unmapped, 2, t0 + Duration::from_millis(60));
    assert!(
        out.iter()
            .any(|p| p.kind == PublicKind::Message && p.text == "visible\n"),
        "an overdue coalesced text must flush on any later frame: {out:?}"
    );
}

#[tokio::test]
async fn test_011_05_loss_ledger_counts_unsettled_closed_and_unqueued_parts() {
    let (sender, mut receiver) = channel(Mode::CodexAppServer, "0.166.0");
    let loss = receiver.loss.clone();
    let mut part = gap(Mode::CodexAppServer, 3, "x");
    part.text = "abcd".into();
    let mut later = part.clone();
    later.received_offset = 5;
    publish(&sender, vec![part.clone(), later.clone()])
        .await
        .unwrap();
    // The drain settles the first part and drops the second with its gap recorded.
    let first = receiver.recv().await.unwrap();
    loss.settled();
    assert_eq!(
        loss.summary().unrecorded,
        1,
        "an unsettled queued part counts lost"
    );
    assert_eq!(loss.summary().first_offset, Some(5));
    let second = receiver.recv().await.unwrap();
    loss.dropped(&second.part, true);
    loss.settled();
    let s = loss.summary();
    assert_eq!((s.dropped, s.unrecorded, s.bytes), (1, 0, 4));
    assert!(
        !s.needs_gap(),
        "a dropped part with its own gap needs no end gap"
    );
    drop((first, second));
    // A closed queue: the part and the rest of its batch count lost.
    drop(receiver);
    let mut a = part.clone();
    a.received_offset = 1;
    assert!(publish(&sender, vec![a, part.clone()]).await.is_err());
    let s = loss.summary();
    assert_eq!(s.unrecorded, 2);
    assert_eq!(s.first_offset, Some(1));
    assert!(s.needs_gap());
}

#[tokio::test]
async fn test_011_05_try_publish_never_waits_and_counts_what_does_not_fit() {
    let (sender, mut receiver) = channel(Mode::CodexAppServer, "0.166.0");
    let mut part = gap(Mode::CodexAppServer, 7, "x");
    part.text = "y".repeat(600 << 10);
    // Two 600 KiB bodies exceed the 1 MiB budget: the second is lost, not awaited.
    try_publish(&sender, vec![part.clone(), part.clone()]);
    assert!(receiver.recv().await.is_some());
    let s = receiver.loss.summary();
    assert_eq!(s.unrecorded, 2, "one lost, one queued and not yet settled");
    receiver.loss.settled();
    assert_eq!(receiver.loss.summary().unrecorded, 1);
    assert_eq!(receiver.loss.summary().bytes, 600 << 10);
}
