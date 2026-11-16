//! What several of this crate's integration tests share, whatever their world: a run command made at a revision the
//! test read before it, made again while the host says that revision is stale. A test binary using it declares
//! `mod common;`.

#![allow(dead_code)] // each test binary uses a part of it

use serde_json::Value;

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// How many times a command is made, at most.
pub const FRESH_TRIES: usize = 20;

/// A run command made at `revision` (read from the database before it), and made again at the revision the host's
/// 409 STALE_REVISION names while it answers so — a host with a runner moves the run on between the test's read and
/// its command (a round's dispatch, a Commander's decision, the pace waking it) — as the client does
/// (`client::fresh`), at most [`FRESH_TRIES`] times. `submit` makes the command at a revision; the last answer, its
/// status and body.
pub async fn fresh<F, Fut>(revision: Value, mut submit: F) -> R<(u16, Value)>
where
    F: FnMut(Value) -> Fut,
    Fut: std::future::Future<Output = R<(u16, Value)>>,
{
    let mut revision = revision;
    for _ in 1..FRESH_TRIES {
        let (status, body) = submit(revision.clone()).await?;
        match stale(status, &body) {
            Some(current) => revision = current,
            None => return Ok((status, body)),
        }
    }
    submit(revision).await
}

/// [`fresh`] for a `submit` that blocks.
pub fn fresh_blocking(
    revision: Value,
    mut submit: impl FnMut(Value) -> R<(u16, Value)>,
) -> R<(u16, Value)> {
    let mut revision = revision;
    for _ in 1..FRESH_TRIES {
        let (status, body) = submit(revision.clone())?;
        match stale(status, &body) {
            Some(current) => revision = current,
            None => return Ok((status, body)),
        }
    }
    submit(revision)
}

/// The revision a 409 STALE_REVISION names, if `status` and `body` are one.
fn stale(status: u16, body: &Value) -> Option<Value> {
    (status == 409 && body["code"] == "STALE_REVISION").then(|| body["current_revision"].clone())
}
