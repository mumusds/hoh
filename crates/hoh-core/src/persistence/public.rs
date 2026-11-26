//! Public occurrence identities are distinct from reusable content-addressed objects and input delivery.
use super::{PersistError, sqlite_error, writer};
use crate::{ids::child_id, public_output::PublicPart};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
type NativeIdentity = (
    String,
    String,
    String,
    String,
    Option<i64>,
    Option<String>,
    i64,
    Option<String>,
);
const INLINE: usize = 16 << 10;
const SEGMENT: usize = 1 << 20;
const RESERVE: u64 = 32 << 20;
fn digest(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub(super) fn accepted(
    tx: &Transaction<'_>,
    run: &str,
    input: &str,
    text: &str,
    target: Option<&str>,
    ctx: &writer::Ctx<'_>,
) -> Result<(), PersistError> {
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM public_input_blocks WHERE input_id=?1)",
            [input],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    if exists {
        return Ok(());
    }
    let (project, spec): (String, Option<String>) = tx
        .query_row(
            "SELECT project_id,spec_id FROM runs WHERE run_id=?1",
            [run],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sqlite_error)?;
    let role = target.unwrap_or("commander");
    let epoch: Option<i64> = tx
        .query_row(
            "SELECT role_epoch FROM role_bindings WHERE run_id=?1 AND role=?2",
            params![run, role],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let block = child_id(input, "public");
    let mut payload = json!({"block_id":block,"input_id":input,"speaker":"user","role":role,"role_epoch":epoch,"attempt_id":Value::Null,"session_id":Value::Null,"task_id":Value::Null,"spec_id":spec,"stream":"text","first_offset":0,"sha256":digest(&text),"complete":true});
    let segments = body(tx, &text, &mut payload, ctx, false)?;
    let seq = writer::insert_event(
        tx,
        &writer::NewEvent {
            intent: writer::Intent::Progress,
            project_id: project,
            run_id: Some(run.into()),
            event_id: child_id(input, "public/message"),
            kind: "public.message".into(),
            payload,
        },
    )?;
    occurrences(tx, &segments, None, Some(input), &block, "text", 0, seq)?;
    tx.execute(
        "INSERT INTO public_input_blocks(input_id,block_id,created_seq) VALUES(?1,?2,?3)",
        params![input, block, seq],
    )
    .map_err(sqlite_error)?;
    Ok(())
}

pub(super) fn native(
    conn: &mut Connection,
    attempt: &str,
    epoch: i64,
    part: &PublicPart,
    ctx: &writer::Ctx<'_>,
) -> Result<Option<i64>, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let facts:Option<NativeIdentity>=tx.query_row("SELECT r.project_id,t.run_id,t.task_id,t.role,a.role_epoch,a.session_id,a.config_revision,t.spec_id FROM attempts a JOIN tasks t USING(task_id) JOIN runs r USING(run_id) WHERE attempt_id=?1",[attempt],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))).optional().map_err(sqlite_error)?;
    let Some((project, run, task, role, actual, session, revision, spec)) = facts else {
        return Err(PersistError::IntegrityViolation(
            "public output has no attempt".into(),
        ));
    };
    if actual != Some(epoch) || epoch <= 0 {
        return Err(PersistError::IntegrityViolation(
            "public output epoch differs from its attempt".into(),
        ));
    }
    let prior:Option<i64>=tx.query_row("SELECT created_seq FROM public_native_parts WHERE attempt_id=?1 AND native_stream=?2 AND native_item_id=?3 AND part=?4",params![attempt,part.native_stream,part.native_item_id,sql_number(part.part)?],|r|r.get(0)).optional().map_err(sqlite_error)?;
    if let Some(seq) = prior {
        return Ok(Some(seq));
    }
    let current:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM role_bindings b JOIN attempts a ON a.attempt_id=?1 WHERE b.run_id=?2 AND b.role=?3 AND b.role_epoch=?4 AND a.state='RUNNING')",params![attempt,run,role,epoch],|r|r.get(0)).map_err(sqlite_error)?;
    let block = child_id(
        attempt,
        &format!("public/{}/{}", part.native_stream, part.block_key),
    );
    let mut payload = json!({"block_id":block,"attempt_id":attempt,"run_id":run,"task_id":task,"role":role,"role_epoch":epoch,"session_id":session,"config_revision":revision,"spec_id":spec,"native_stream":part.native_stream,"native_item_id":part.native_item_id,"received_offset":part.received_offset,"part":part.part,"stream":part.stream,"first_offset":part.first_offset,"speaker":part.speaker,"name":part.name,"activity":part.activity,"model":part.model,"usage":part.usage,"redacted":part.redacted,"complete":part.complete,"current":current,"sha256":digest(&part.text)});
    if let Some(frame_kind) = &part.frame_kind {
        payload["frame_kind"] = json!(frame_kind);
    }
    if let Some(count) = part.count {
        payload["count"] = json!(count);
    }
    let segments = body(&tx, &part.text, &mut payload, ctx, true)?;
    let event_id = child_id(
        attempt,
        &format!(
            "public-part/{}/{}/{}",
            part.native_stream, part.native_item_id, part.part
        ),
    );
    // Record semantics preserve a stopped attempt's history without moving its Run or current activity.
    let seq = writer::insert_event(
        &tx,
        &writer::NewEvent {
            intent: writer::Intent::Record,
            project_id: project,
            run_id: Some(run),
            event_id,
            kind: part.kind.event_kind().into(),
            payload,
        },
    )?;
    occurrences(
        &tx,
        &segments,
        Some(attempt),
        None,
        &block,
        &part.stream,
        part.first_offset,
        seq,
    )?;
    tx.execute("INSERT INTO public_native_parts(attempt_id,native_stream,native_item_id,part,block_id,received_offset,created_seq) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![attempt,part.native_stream,part.native_item_id,sql_number(part.part)?,block,sql_number(part.received_offset)?,seq]).map_err(sqlite_error)?;
    writer::commit(tx, ctx)?;
    Ok(Some(seq))
}

fn body(
    tx: &Transaction<'_>,
    text: &str,
    payload: &mut Value,
    ctx: &writer::Ctx<'_>,
    streaming: bool,
) -> Result<Vec<super::runner::ArtifactRow>, PersistError> {
    if text.len() <= INLINE {
        payload["text"] = json!(text);
        return Ok(Vec::new());
    }
    let root = tx
        .path()
        .and_then(|p| std::path::Path::new(p).parent())
        .and_then(std::path::Path::parent)
        .ok_or_else(|| {
            PersistError::Storage("public object needs a file-backed project database".into())
        })?;
    let available = if let Some(n) = ctx.public_available_bytes() {
        n
    } else {
        let fs = rustix::fs::statvfs(root).map_err(|e| PersistError::Storage(e.to_string()))?;
        fs.f_bavail.saturating_mul(fs.f_frsize)
    };
    let required = RESERVE
        + (if streaming {
            text.len().max(SEGMENT)
        } else {
            text.len()
        }) as u64;
    if available < required {
        return Err(PersistError::StorageLowSpace {
            available_bytes: available,
            required_bytes: required,
            min_free_bytes: RESERVE,
        });
    }
    let mut rest = text;
    let mut segments = Vec::new();
    while !rest.is_empty() {
        let mut end = rest.len().min(SEGMENT);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let bytes = &rest.as_bytes()[..end];
        let sha = crate::objects::put_public_io(root, bytes)
            .map_err(|e| PersistError::Storage(format!("public object seal: {e}")))?;
        let row = super::runner::ArtifactRow {
            path: crate::objects::relative_path(&sha),
            sha256: sha,
            bytes: bytes.len() as u64,
            mime: "text/plain; charset=utf-8".into(),
            kind: "public_segment".into(),
            name: "public text".into(),
        };
        super::runner::register(tx, std::slice::from_ref(&row))?;
        let text_kind = if bytes.contains(&0) { "binary" } else { "text" };
        tx.execute("INSERT OR REPLACE INTO ui_object_metadata(object_sha256,bytes,text_kind,validated_at) VALUES(?1,?2,?3,datetime('now'))",params![row.sha256,sql_number(row.bytes)?,text_kind]).map_err(sqlite_error)?;
        segments.push(row);
        rest = &rest[end..];
    }
    payload["refs"] = json!(
        segments
            .iter()
            .map(super::runner::ArtifactRow::json)
            .collect::<Vec<_>>()
    );
    Ok(segments)
}
#[allow(clippy::too_many_arguments)]
fn occurrences(
    tx: &Transaction<'_>,
    segments: &[super::runner::ArtifactRow],
    attempt: Option<&str>,
    input: Option<&str>,
    block: &str,
    stream: &str,
    first: u64,
    seq: i64,
) -> Result<(), PersistError> {
    let mut offset = first;
    for segment in segments {
        let artifact = super::runner::artifact_id(&segment.sha256, &segment.path);
        tx.execute("INSERT INTO public_segments(artifact_id,attempt_id,input_id,block_id,stream,first_offset,bytes,sha256,sealed,created_seq) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,1,?9)",params![artifact,attempt,input,block,stream,sql_number(offset)?,sql_number(segment.bytes)?,segment.sha256,seq]).map_err(sqlite_error)?;
        tx.execute("INSERT OR IGNORE INTO ui_object_references(owner_kind,owner_id,object_sha256,artifact_id) VALUES('public_block',?1,?2,?3)",params![block,segment.sha256,artifact]).map_err(sqlite_error)?;
        offset += segment.bytes;
    }
    Ok(())
}
#[cfg(test)]
mod tests;

fn sql_number(n: u64) -> Result<i64, PersistError> {
    i64::try_from(n)
        .map_err(|_| PersistError::IntegrityViolation("public offset exceeds SQLite range".into()))
}

/// An attempt's interrupted stream is history, never current agent activity.
pub(super) fn interrupted(
    tx: &Transaction<'_>,
    attempt: &str,
    reason: &str,
) -> Result<(), PersistError> {
    let has: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM public_native_parts WHERE attempt_id=?1)",
            [attempt],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    if !has {
        return Ok(());
    }
    let (project,run,task,role,epoch,session,revision,spec):NativeIdentity=tx.query_row("SELECT r.project_id,t.run_id,t.task_id,t.role,a.role_epoch,a.session_id,a.config_revision,t.spec_id FROM attempts a JOIN tasks t USING(task_id) JOIN runs r USING(run_id) WHERE attempt_id=?1",[attempt],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))).map_err(sqlite_error)?;
    writer::insert_event(
        tx,
        &writer::NewEvent {
            intent: writer::Intent::Record,
            project_id: project,
            run_id: Some(run),
            event_id: child_id(attempt, "public/interrupted"),
            kind: "public.gap".into(),
            payload: json!({"block_id":child_id(attempt,"public/interrupted"),"attempt_id":attempt,"task_id":task,"role":role,"role_epoch":epoch,"session_id":session,"config_revision":revision,"spec_id":spec,"reason":reason,"complete":false,"current":false}),
        },
    )?;
    Ok(())
}
