//! M3R-503: deterministic ready-reader fixtures, never live agent sessions.
use super::*;
use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::task::{Context, Poll};
use tokio::io::ReadBuf;
use tokio::sync::watch;

type R = Result<(), Box<dyn std::error::Error>>;

struct ReadyChunks {
    bytes: Vec<u8>,
    at: usize,
    chunk: usize,
    reads: Arc<AtomicUsize>,
    signal: watch::Sender<bool>,
    signal_at: usize,
    cooperative: bool,
}

impl AsyncRead for ReadyChunks {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let coop = if self.cooperative {
            Some(std::task::ready!(tokio::task::coop::poll_proceed(cx)))
        } else {
            None
        };
        let end = (self.at + self.chunk.min(buf.remaining())).min(self.bytes.len());
        buf.put_slice(&self.bytes[self.at..end]);
        self.at = end;
        let reads = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
        if reads == self.signal_at {
            self.signal.send_replace(true);
        }
        if let Some(coop) = coop {
            coop.made_progress();
        }
        Poll::Ready(Ok(()))
    }
}

fn reader(
    input: Vec<u8>,
    signal_at: usize,
) -> (
    FrameReader<ReadyChunks>,
    Arc<AtomicUsize>,
    watch::Receiver<bool>,
) {
    let reads = Arc::new(AtomicUsize::new(0));
    let (signal, stopped) = watch::channel(signal_at == 0);
    let inner = ReadyChunks {
        bytes: input,
        at: 0,
        chunk: READ_CHUNK,
        reads: reads.clone(),
        signal,
        signal_at,
        cooperative: false,
    };
    (
        FrameReader::new(inner, FRAME_LIMIT, READ_CHUNK),
        reads,
        stopped,
    )
}

fn skip_rule() -> Skip {
    Skip {
        known: super::big,
        max_length: SKIP_LIMIT,
    }
}

fn reduce_rule() -> Skip {
    Skip {
        known: super::item_head,
        max_length: SKIP_LIMIT,
    }
}

async fn cancel(
    mut reader: FrameReader<ReadyChunks>,
    reads: Arc<AtomicUsize>,
    mut stopped: watch::Receiver<bool>,
    signal_at: usize,
) -> R {
    let started = std::time::Instant::now();
    let observed = tokio::select! {
        // Same adverse ordering and cooperative watch receiver as process::read_stdout.
        biased;
        frame = reader.next() => { frame?; false },
        result = stopped.wait_for(|stop| *stop) => { result?; true },
    };
    let count = reads.load(Ordering::SeqCst);
    eprintln!(
        "M3R-503 signal_at={signal_at}, reads={count}, elapsed={:?}, cancellation={observed}",
        started.elapsed()
    );
    assert!(
        observed,
        "frame ended before cancellation was observed after {count} reads"
    );
    assert!(
        count <= signal_at.max(1),
        "read {count} chunks for signal at {signal_at}"
    );
    Ok(())
}

#[tokio::test]
async fn test_027_05_m3r503_skip_observes_abandon_within_one_read() -> R {
    let input = [
        super::big_frame(FRAME_LIMIT + 16 * READ_CHUNK),
        b"\n".to_vec(),
    ]
    .concat();
    for signal_at in [0, FRAME_LIMIT / READ_CHUNK + 2] {
        let (reader, reads, stop) = reader(input.clone(), signal_at);
        cancel(reader.skipping(skip_rule()), reads, stop, signal_at).await?;
    }
    Ok(())
}

#[tokio::test]
async fn test_027_06_m3r503_reduce_observes_cancel_within_one_read() -> R {
    let input = [
        super::large_item(&"x".repeat(2 * FRAME_LIMIT)),
        b"\n".to_vec(),
    ]
    .concat();
    for signal_at in [0, 3] {
        let (reader, reads, stop) = reader(input.clone(), signal_at);
        cancel(reader.reducing(reduce_rule()), reads, stop, signal_at).await?;
    }
    Ok(())
}

#[tokio::test]
async fn test_027_03_m3r503_ordinary_observes_cancel_within_one_read() -> R {
    let input = [vec![b'x'; FRAME_LIMIT / 2], b"\n".to_vec()].concat();
    for signal_at in [0, 3] {
        let (reader, reads, stop) = reader(input.clone(), signal_at);
        cancel(reader, reads, stop, signal_at).await?;
    }
    Ok(())
}

// Drop and recreate next() at every Pending point, including a held CR before LF,
// partial JSON escapes and UTF-8 code points. Unlike a timer this cannot miss a fast loop.
async fn interrupted(mut reader: FrameReader<ReadyChunks>) -> io::Result<(Vec<Frame>, usize)> {
    let mut frames = Vec::new();
    let mut interruptions = 0;
    let bound = 2 * (reader.inner.bytes.len().div_ceil(reader.chunk) + 1);
    loop {
        let before = reader.bytes_read();
        let polled = poll_fn(|cx| Poll::Ready(std::pin::pin!(reader.next()).poll(cx))).await;
        match polled {
            Poll::Ready(Ok(Some(frame))) => frames.push(frame),
            Poll::Ready(Ok(None)) => break,
            Poll::Ready(Err(e)) => return Err(e),
            Poll::Pending => {
                interruptions += 1;
                assert!(interruptions <= bound, "restarting next() made no progress");
                // Restarting must make progress; a yield before consuming input would loop forever.
                assert!(reader.bytes_read() > before || reader.reducing.is_some());
                assert!(reader.max_buffered() <= FRAME_LIMIT + READ_CHUNK);
            }
        }
    }
    Ok((frames, interruptions))
}

#[tokio::test]
async fn test_027_05_m3r503_drop_mid_skip_resumes_hash_crlf_and_next_frame() -> R {
    // CR is the last byte of a full chunk; LF and the next frame follow in a separate read.
    let raw = super::big_frame(2 * FRAME_LIMIT - 1);
    let input = [raw.clone(), b"\r\nafter\n".to_vec()].concat();
    let (reader, _, _) = reader(input, usize::MAX);
    let (frames, interruptions) = interrupted(reader.skipping(skip_rule())).await?;
    assert!(
        interruptions >= 2 * FRAME_LIMIT / READ_CHUNK,
        "no mid-skip suspension: {interruptions}"
    );
    assert_eq!(
        frames,
        vec![
            Frame::Skipped(SkippedFrame {
                kind: "big",
                length: raw.len() as u64,
                sha256: hex(&Sha256::digest(&raw)),
                head: raw[..HEAD_BYTES].to_vec()
            }),
            Frame::Line(b"after".to_vec())
        ]
    );
    Ok(())
}

#[tokio::test]
async fn test_027_06_m3r503_drop_mid_reduce_preserves_escaped_strings_and_routing() -> R {
    let raw = super::large_item(&"é\\uD83D\\uDE00\\n".repeat(100_000));
    let input = [raw.clone(), b"\r\nafter\n".to_vec()].concat();
    let (reader, _, _) = reader(input, usize::MAX);
    let (frames, interruptions) = interrupted(reader.reducing(reduce_rule())).await?;
    assert!(
        interruptions > 10,
        "no mid-reduction suspension: {interruptions}"
    );
    let [Frame::Reduced { line, original }, after] = frames.as_slice() else {
        return Err("expected reduced frame then after".into());
    };
    assert_eq!(after, &Frame::Line(b"after".to_vec()));
    assert_eq!(original.length, raw.len() as u64);
    assert_eq!(original.sha256, hex(&Sha256::digest(&raw)));
    assert_eq!(original.head, raw[..HEAD_BYTES]);
    let full: serde_json::Value = serde_json::from_slice(&raw)?;
    let value: serde_json::Value = serde_json::from_slice(line)?;
    let text = full["params"]["item"]["aggregatedOutput"]
        .as_str()
        .ok_or("output")?;
    let output = &value["params"]["item"]["aggregatedOutput"];
    assert_eq!(output["original_bytes"], text.len());
    assert_eq!(output["sha256"], hex(&Sha256::digest(text.as_bytes())));
    assert_eq!(output["truncated"], true);
    assert_eq!(value["params"]["threadId"], "t");
    assert_eq!(value["params"]["turnId"], "u");
    assert_eq!(value["params"]["item"]["id"], "i");
    assert_eq!(
        value["params"]["item"]["x_future"],
        serde_json::json!({"n":7})
    );
    Ok(())
}

#[tokio::test]
async fn test_027_03_m3r503_drop_mid_ordinary_frame_loses_no_bytes() -> R {
    let raw = vec![b'x'; FRAME_LIMIT / 2];
    let (reader, _, _) = reader([raw.clone(), b"\r\nafter\n".to_vec()].concat(), usize::MAX);
    let (frames, interruptions) = interrupted(reader).await?;
    assert!(
        interruptions >= 8,
        "no ordinary suspension: {interruptions}"
    );
    assert_eq!(
        frames,
        vec![Frame::Line(raw), Frame::Line(b"after".to_vec())]
    );
    Ok(())
}

#[tokio::test]
async fn test_027_05_m3r503_exhausted_budget_does_not_starve_abandon() -> R {
    let input = [super::big_frame(2 * FRAME_LIMIT), b"\n".to_vec()].concat();
    let (mut reader, reads, mut stopped) = reader(input, 0);
    reader.inner.cooperative = true;
    let mut reader = reader.skipping(skip_rule());
    let count = tokio::spawn(async move {
        // Leave just one operation: the first read exhausts the budget, so even an
        // already-ready watch initially returns Pending. Next scheduling must observe it.
        for _ in 0..127 {
            tokio::task::consume_budget().await;
        }
        let observed = tokio::select! {
            biased;
            frame = reader.next() => { frame?; false },
            result = stopped.wait_for(|stop| *stop) => { result.map_err(io::Error::other)?; true },
        };
        let count = reads.load(Ordering::SeqCst);
        assert!(
            observed,
            "exhaustion starved abandon until frame completion ({count} reads)"
        );
        assert!(
            count <= 2,
            "budget exhaustion delayed abandon for {count} reads"
        );
        Ok::<_, io::Error>(count)
    })
    .await??;
    eprintln!("M3R-503 one remaining budget operation: abandon observed after {count} reads");
    Ok(())
}

#[tokio::test]
async fn m3r503_tokio_pipe_consumes_budget_and_can_defer_ready_watch() -> R {
    let (tx, mut rx) = tokio::net::unix::pipe::pipe()?;
    tx.writable().await?;
    assert_eq!(tx.try_write(&[b'x'; 512])?, 512);
    rx.readable().await?;
    // A freshly scheduled task has 128 operations in the locked Tokio version. One-byte
    // reads keep a real kernel pipe ready throughout; no producer timing or fake I/O.
    let (reads, watch_pending) = tokio::spawn(async move {
        let (_stop, mut stopped) = watch::channel(true);
        let mut count = 0;
        poll_fn(|cx| {
            loop {
                let mut byte = [0];
                match Pin::new(&mut rx).poll_read(cx, &mut ReadBuf::new(&mut byte)) {
                    Poll::Ready(Ok(())) => count += 1,
                    Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                    Poll::Pending => {
                        let pending = std::pin::pin!(stopped.wait_for(|stop| *stop))
                            .poll(cx)
                            .is_pending();
                        return Poll::Ready(Ok((count, pending)));
                    }
                }
            }
        })
        .await
    })
    .await??;
    eprintln!(
        "M3R-503 real Tokio pipe: {reads} ready reads; ready watch Pending after exhaustion={watch_pending}"
    );
    assert_eq!(reads, 128);
    assert!(watch_pending);
    Ok(())
}
