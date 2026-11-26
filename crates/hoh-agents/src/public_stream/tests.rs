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
    let a = json!({"method":"item/agentMessage/delta","params":{"itemId":"m","delta":"one"}});
    let b = json!({"method":"item/agentMessage/delta","params":{"itemId":"m","delta":"two"}});
    assert_eq!(n.feed(&a, 10)[0].first_offset, 0);
    n.feed(&a, 10);
    let second = n.feed(&b, 11);
    assert_eq!(second[0].first_offset, 3, "replay must not invent a gap");
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
    let d1 = json!({"type":"message_update","message":{"role":"assistant","responseId":"resp-1"},"assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"hello "}});
    let d2 = json!({"type":"message_update","message":{"role":"assistant","responseId":"resp-1"},"assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"world"}});
    let end = json!({"type":"message_end","message":{"role":"assistant","responseId":"resp-1","content":[{"type":"text","text":"hello world"}]}});
    let _ = n.feed(&start, 0);
    let p1 = n.feed(&d1, 1);
    let p2 = n.feed(&d2, 2);
    let p3 = n.feed(&end, 3);
    assert_eq!(p1.len(), 1);
    assert_eq!(p1[0].text, "hello ");
    assert_eq!(
        p2.len(),
        1,
        "second text_delta with same responseId must not be dropped"
    );
    assert_eq!(p2[0].text, "world");
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
        &json!({"method":"item/agentMessage/delta","params":{"itemId":"m","delta":"world"}}),
        1,
        t0 + Duration::from_millis(20),
    );
    assert!(p1.is_empty());
    assert!(n.flush_due(t0 + Duration::from_millis(49)).is_empty());
    let flushed = n.flush_due(t0 + Duration::from_millis(50));
    assert_eq!(flushed.len(), 1);
    assert_eq!(flushed[0].text, "hello world");
    assert_eq!(flushed[0].first_offset, 0);

    // 16 KiB size cap flushes immediately without waiting for 50 ms.
    let big = "a".repeat(COALESCE_BYTES);
    let p2 = n.feed_at(
        &json!({"method":"item/agentMessage/delta","params":{"itemId":"m","delta":big}}),
        2,
        t0 + Duration::from_millis(51),
    );
    assert_eq!(p2.len(), 1);
    assert_eq!(p2[0].first_offset, 11);
    assert_eq!(p2[0].text.len(), COALESCE_BYTES);
}
