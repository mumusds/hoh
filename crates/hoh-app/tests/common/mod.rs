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

/// `child`'s output once it has ended, waited for at most `limit` (its stdout and stderr read meanwhile, so a full pipe
/// cannot hold it up); past the limit the child, this test's own and not yet reaped, is killed and reaped, and that is
/// an error naming what it wrote to stderr so far. A helper's wait has a bound of its own, so a hung child fails its
/// test instead of holding it until the suite's bound (ADR-022 decision 10; CIR-502).
pub fn output_within(
    mut child: std::process::Child,
    limit: std::time::Duration,
) -> R<std::process::Output> {
    fn drain(
        pipe: Option<impl std::io::Read + Send + 'static>,
    ) -> std::sync::mpsc::Receiver<Vec<u8>> {
        let (sent, got) = std::sync::mpsc::channel();
        match pipe {
            Some(mut pipe) => {
                std::thread::spawn(move || {
                    let mut bytes = Vec::new();
                    let _ = std::io::Read::read_to_end(&mut pipe, &mut bytes);
                    let _ = sent.send(bytes);
                });
            }
            None => {
                let _ = sent.send(Vec::new());
            }
        }
        got
    }
    let (stdout, stderr) = (drain(child.stdout.take()), drain(child.stderr.take()));
    let end = std::time::Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if std::time::Instant::now() > end {
            let _ = child.kill();
            let _ = child.wait();
            let said = stderr
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or_default();
            return Err(format!(
                "still running after {limit:?}, so killed; its stderr: {}",
                String::from_utf8_lossy(&said)
            )
            .into());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    // Its pipes close at its end, unless a process it left holds them: read for a bounded while more.
    let left = |pipe: &std::sync::mpsc::Receiver<Vec<u8>>| {
        pipe.recv_timeout(std::time::Duration::from_secs(5))
            .map_err(|_| "its output was still held open 5 s after it ended")
    };
    Ok(std::process::Output {
        status,
        stdout: left(&stdout)?,
        stderr: left(&stderr)?,
    })
}
