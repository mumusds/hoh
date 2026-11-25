//! LF-delimited protocol frames with a hard size limit (docs/13-adapters.md "进程与终结", docs/02 REQ-026/REQ-027).
//!
//! Frames are split on the byte 0x0A only, never on U+2028/U+2029 or other line separators, and one CR before the
//! LF is dropped (it does not count against the limit, however the CR and LF are split across reads); decoding
//! (UTF-8, JSON) is the adapter's job. A frame longer than the limit is refused without buffering further: the reader
//! holds at most `limit + chunk` bytes (`chunk` is at least 2), reports the length it saw, the sha256 of those bytes and their first 4 KiB,
//! and yields no further frames.
//!
//! The one exception (CR-040): a reader given a [`Skip`] rule skips a frame over the limit whose head the rule knows
//! (a record its adapter does not need): it drops what it held and then reads one chunk at a time up to the frame's
//! LF, hashing and counting the bytes without keeping them, still within `limit + chunk`, yields
//! [`Frame::Skipped`] and frames on after the LF. A known frame that the stream ends before its LF, or that grows past
//! the rule's `max_length`, is refused like any other.

use sha2::{Digest, Sha256};
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt};

/// Frame size limit (initial value of docs/13).
pub const FRAME_LIMIT: usize = 1 << 20;
/// Bytes read from the pipe at a time (initial value of docs/02 REQ-027).
pub const READ_CHUNK: usize = 64 << 10;
/// Bytes of a refused frame kept for diagnosis.
pub const HEAD_BYTES: usize = 4 << 10;

/// One unit read from a protocol stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// A complete frame without its LF (and without a CR right before it).
    Line(Vec<u8>),
    /// Bytes after the last LF when the stream ended: a frame cut short.
    Partial(Vec<u8>),
    /// A frame longer than the limit; nothing more is read as frames.
    TooLarge(FrameTooLarge),
    /// A frame longer than the limit that the reader's [`Skip`] rule knew by its head, read to its LF without being
    /// kept (CR-040); framing goes on after it.
    Skipped(SkippedFrame),
}

/// Knows, from the head of a frame longer than the limit (its first [`HEAD_BYTES`] bytes), a record whose payload its
/// adapter does not use: the record's name, or `None` to refuse the frame (CR-040).
pub type Known = fn(&[u8]) -> Option<&'static str>;

/// Which frames over the limit are skipped rather than refused (CR-040).
#[derive(Debug, Clone, Copy)]
pub struct Skip {
    pub known: Known,
    /// The longest frame skipped: one that grows past it is refused.
    pub max_length: u64,
}

/// The longest frame a [`Skip`] rule skips by default (CR-040).
pub const SKIP_LIMIT: u64 = 256 << 20;

/// A skipped frame (CR-040).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedFrame {
    /// The name the rule gave it.
    pub kind: &'static str,
    /// Its length without the LF (and a CR right before it).
    pub length: u64,
    /// sha256 (lowercase hex) of those bytes.
    pub sha256: String,
    /// Their first [`HEAD_BYTES`] bytes.
    pub head: Vec<u8>,
}

/// The name of the first entry of `table` whose byte prefix `head` starts with.
pub fn by_prefix(head: &[u8], table: &[(&[u8], &'static str)]) -> Option<&'static str> {
    table
        .iter()
        .find(|(prefix, _)| head.starts_with(prefix))
        .map(|(_, kind)| *kind)
}

/// Diagnosis for a refused frame (PROTOCOL_FRAME_TOO_LARGE).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameTooLarge {
    /// Bytes of the frame received when it was refused (more than the limit, at most limit + one read; for a frame
    /// a [`Skip`] rule knew, every byte of it read until the stream ended or it passed the rule's `max_length`).
    pub length: usize,
    /// sha256 (lowercase hex) of those bytes.
    pub prefix_sha256: String,
    /// Their first [`HEAD_BYTES`] bytes.
    pub head: Vec<u8>,
}

/// Reads [`Frame`]s from a byte stream; see the module documentation.
#[derive(Debug)]
pub struct FrameReader<R> {
    inner: R,
    buf: Vec<u8>,
    /// Start of the current frame in `buf`.
    start: usize,
    /// Bytes after `start` already searched for LF.
    scanned: usize,
    limit: usize,
    chunk: usize,
    max_buffered: usize,
    bytes: u64,
    done: bool,
    skip: Option<Skip>,
    skipping: Option<Skipping>,
}

/// A known frame over the limit being read to its LF without being kept (CR-040).
#[derive(Debug)]
struct Skipping {
    kind: &'static str,
    hasher: Sha256,
    length: u64,
    head: Vec<u8>,
    /// The bytes so far ended with a CR not yet counted: the one before the LF, unless more of the frame follows.
    cr: bool,
}

impl Skipping {
    /// Count `bytes` of the frame.
    fn take(&mut self, bytes: &[u8]) {
        self.hasher.update(bytes);
        self.length += bytes.len() as u64;
        let room = HEAD_BYTES.saturating_sub(self.head.len());
        self.head.extend_from_slice(&bytes[..bytes.len().min(room)]);
    }

    /// Count `bytes`, which the LF does not end: a CR held before them is content, a CR ending them is held.
    fn take_open(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if std::mem::take(&mut self.cr) {
            self.take(b"\r");
        }
        let (body, cr) = match bytes.split_last() {
            Some((b'\r', body)) => (body, true),
            _ => (bytes, false),
        };
        self.take(body);
        self.cr = cr;
    }

    /// Count `bytes`, the rest of the frame before its LF (without a CR right before the LF).
    fn take_last(&mut self, bytes: &[u8]) {
        let held = std::mem::take(&mut self.cr);
        if bytes.is_empty() {
            // A held CR was the one before the LF.
            return;
        }
        if held {
            self.take(b"\r");
        }
        self.take(bytes.strip_suffix(b"\r").unwrap_or(bytes));
    }
}

impl<R: AsyncRead + Unpin> FrameReader<R> {
    /// A reader refusing frames longer than `limit` bytes. `chunk` (raised to at least 2) is the allocation step: the
    /// buffer grows by at most `chunk` bytes per read and never holds more than `limit + chunk` bytes in total; a
    /// single read may return more than `chunk` bytes when earlier growth left spare capacity (M1T02R-04).
    pub fn new(inner: R, limit: usize, chunk: usize) -> Self {
        Self {
            inner,
            buf: Vec::new(),
            start: 0,
            scanned: 0,
            limit,
            // At least two, so a read is still possible while a frame at the limit waits for the LF after its CR.
            chunk: chunk.max(2),
            max_buffered: 0,
            bytes: 0,
            done: false,
            skip: None,
            skipping: None,
        }
    }

    /// The reader skipping the frames over the limit that `skip` knows (CR-040).
    pub fn skipping(mut self, skip: Skip) -> Self {
        self.skip = Some(skip);
        self
    }

    /// The most bytes the reader has held at once.
    pub fn max_buffered(&self) -> usize {
        self.max_buffered
    }

    /// Bytes read from the stream so far.
    pub fn bytes_read(&self) -> u64 {
        self.bytes
    }

    /// The stream, for draining what follows a refused frame.
    pub fn into_inner(self) -> R {
        self.inner
    }

    /// The next frame; `None` once the stream ended or a frame was refused. Cancel-safe: dropping the future loses
    /// no bytes already read.
    pub async fn next(&mut self) -> io::Result<Option<Frame>> {
        loop {
            if self.done {
                return Ok(None);
            }
            if self.skipping.is_some() {
                match self.skip_more().await? {
                    Some(frame) => return Ok(Some(frame)),
                    None => continue,
                }
            }
            let from = self.start + self.scanned;
            if let Some(i) = self.buf[from..].iter().position(|&b| b == b'\n') {
                let end = from + i;
                let frame_start = self.start;
                self.start = end + 1;
                self.scanned = 0;
                let content_end = if end > frame_start && self.buf[end - 1] == b'\r' {
                    end - 1
                } else {
                    end
                };
                if content_end - frame_start > self.limit {
                    // The whole frame is held: a known one within the rule's length is skipped at once.
                    if let Some((kind, max_length)) = self.known(frame_start, content_end)
                        && (content_end - frame_start) as u64 <= max_length
                    {
                        let content = &self.buf[frame_start..content_end];
                        return Ok(Some(Frame::Skipped(SkippedFrame {
                            kind,
                            length: content.len() as u64,
                            sha256: hex(&Sha256::digest(content)),
                            head: content[..content.len().min(HEAD_BYTES)].to_vec(),
                        })));
                    }
                    return Ok(Some(self.refuse(frame_start, content_end)));
                }
                return Ok(Some(Frame::Line(
                    self.buf[frame_start..content_end].to_vec(),
                )));
            }
            self.scanned = self.buf.len() - self.start;
            // Pending bytes may exceed the limit by one CR that an LF in the next read would drop (M1T02R-02).
            let over = self.scanned > self.limit + 1
                || (self.scanned == self.limit + 1 && self.buf.last() != Some(&b'\r'));
            if over {
                if let Some((kind, _)) = self.known(self.start, self.buf.len()) {
                    // Read the rest to its LF without keeping it (CR-040).
                    let mut skipping = Skipping {
                        kind,
                        hasher: Sha256::new(),
                        length: 0,
                        head: Vec::new(),
                        cr: false,
                    };
                    skipping.take_open(&self.buf[self.start..]);
                    self.skipping = Some(skipping);
                    self.buf.clear();
                    self.start = 0;
                    self.scanned = 0;
                    continue;
                }
                return Ok(Some(self.refuse(self.start, self.buf.len())));
            }
            if self.start > 0 {
                self.buf.drain(..self.start);
                self.start = 0;
            }
            // Reads are capped so that at most limit + chunk bytes are ever held (at least one byte: chunk >= 2);
            // memory grows by at most one chunk per read, never to the whole limit up front.
            let room = self.limit + self.chunk - self.buf.len();
            let want = room.min(self.chunk);
            if self.buf.capacity() - self.buf.len() < want {
                self.buf.reserve_exact(want);
            }
            let n = (&mut self.inner)
                .take(room as u64)
                .read_buf(&mut self.buf)
                .await?;
            self.max_buffered = self.max_buffered.max(self.buf.len());
            self.bytes += n as u64;
            if n == 0 {
                self.done = true;
                if self.buf.len() > self.limit {
                    // A trailing CR without LF is content.
                    return Ok(Some(self.refuse(0, self.buf.len())));
                }
                let rest = std::mem::take(&mut self.buf);
                return Ok((!rest.is_empty()).then_some(Frame::Partial(rest)));
            }
        }
    }

    /// The name and the rule's length of the frame in `buf[from..to]` over the limit, when the [`Skip`] rule knows its
    /// head.
    fn known(&self, from: usize, to: usize) -> Option<(&'static str, u64)> {
        let skip = self.skip?;
        let frame = &self.buf[from..to];
        (skip.known)(&frame[..frame.len().min(HEAD_BYTES)]).map(|kind| (kind, skip.max_length))
    }

    /// One read of a frame being skipped: the frame once its LF came, a refusal once the stream ended or the frame
    /// passed the rule's length, else `None`. Nothing but the read is held between calls.
    async fn skip_more(&mut self) -> io::Result<Option<Frame>> {
        if self.buf.capacity() < self.chunk {
            self.buf.reserve_exact(self.chunk);
        }
        let n = (&mut self.inner)
            .take(self.chunk as u64)
            .read_buf(&mut self.buf)
            .await?;
        self.max_buffered = self.max_buffered.max(self.buf.len());
        self.bytes += n as u64;
        let max_length = self.skip.map_or(0, |s| s.max_length);
        let Some(skipping) = self.skipping.as_mut() else {
            return Ok(None);
        };
        if n == 0 {
            // Cut short: not a complete record. A held CR is content.
            if std::mem::take(&mut skipping.cr) {
                skipping.take(b"\r");
            }
            return Ok(self.refuse_skipped());
        }
        match self.buf.iter().position(|&b| b == b'\n') {
            Some(i) => {
                skipping.take_last(&self.buf[..i]);
                self.start = i + 1;
                self.scanned = 0;
                if skipping.length > max_length {
                    return Ok(self.refuse_skipped());
                }
                let Some(done) = self.skipping.take() else {
                    return Ok(None);
                };
                Ok(Some(Frame::Skipped(SkippedFrame {
                    kind: done.kind,
                    length: done.length,
                    sha256: hex(&done.hasher.finalize()),
                    head: done.head,
                })))
            }
            None => {
                skipping.take_open(&self.buf);
                self.buf.clear();
                if skipping.length > max_length {
                    if std::mem::take(&mut skipping.cr) {
                        skipping.take(b"\r");
                    }
                    return Ok(self.refuse_skipped());
                }
                Ok(None)
            }
        }
    }

    /// Refuse the frame being skipped: every byte of it counted so far.
    fn refuse_skipped(&mut self) -> Option<Frame> {
        let skipping = self.skipping.take()?;
        self.done = true;
        self.buf = Vec::new();
        Some(Frame::TooLarge(FrameTooLarge {
            length: usize::try_from(skipping.length).unwrap_or(usize::MAX),
            prefix_sha256: hex(&skipping.hasher.finalize()),
            head: skipping.head,
        }))
    }

    fn refuse(&mut self, from: usize, to: usize) -> Frame {
        let seen = &self.buf[from..to];
        let refused = FrameTooLarge {
            length: seen.len(),
            prefix_sha256: hex(&Sha256::digest(seen)),
            head: seen[..seen.len().min(HEAD_BYTES)].to_vec(),
        };
        self.done = true;
        self.buf = Vec::new();
        Frame::TooLarge(refused)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    type R = Result<(), Box<dyn std::error::Error>>;

    async fn frames(input: &[u8], limit: usize, chunk: usize) -> io::Result<(Vec<Frame>, usize)> {
        let mut reader = FrameReader::new(input, limit, chunk);
        let mut out = Vec::new();
        while let Some(frame) = reader.next().await? {
            out.push(frame);
        }
        Ok((out, reader.max_buffered()))
    }

    fn line(s: &str) -> Frame {
        Frame::Line(s.as_bytes().to_vec())
    }

    #[tokio::test]
    async fn frames_split_on_lf_bytes_only() -> R {
        // U+2028 (e2 80 a8), U+2029 and a lone CR stay inside frames; one CR before LF is dropped.
        let input = "a\u{2028}b\r\nc\u{2029}\rd\n\ne\r\r\nlast";
        let (got, _) = frames(input.as_bytes(), 64, 3).await?;
        assert_eq!(
            got,
            vec![
                line("a\u{2028}b"),
                line("c\u{2029}\rd"),
                line(""),
                line("e\r"),
                Frame::Partial(b"last".to_vec()),
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_frame_at_the_limit_passes_and_one_byte_more_is_refused() -> R {
        let ok = [vec![b'x'; 16], b"\n".to_vec()].concat();
        assert_eq!(
            frames(&ok, 16, 4).await?.0,
            vec![Frame::Line(vec![b'x'; 16])]
        );
        // The CR before LF does not count.
        let crlf = [vec![b'x'; 16], b"\r\n".to_vec()].concat();
        assert_eq!(
            frames(&crlf, 16, 4).await?.0,
            vec![Frame::Line(vec![b'x'; 16])]
        );
        let long = [vec![b'y'; 17], b"\nnext\n".to_vec()].concat();
        let (got, _) = frames(&long, 16, 64).await?;
        let [Frame::TooLarge(refused)] = got.as_slice() else {
            return Err(format!("expected one refused frame, got {got:?}").into());
        };
        assert_eq!(refused.length, 17);
        assert_eq!(refused.head, vec![b'y'; 17]);
        assert_eq!(refused.prefix_sha256, hex(&Sha256::digest(vec![b'y'; 17])));
        Ok(())
    }

    /// Feeds `parts` as separate reads.
    struct Parts(std::collections::VecDeque<Vec<u8>>);

    impl AsyncRead for Parts {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            if let Some(mut part) = self.0.pop_front() {
                let n = part.len().min(buf.remaining());
                buf.put_slice(&part[..n]);
                if n < part.len() {
                    self.0.push_front(part.split_off(n));
                }
            }
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn a_crlf_split_across_reads_does_not_count_against_the_limit() -> R {
        // M1T02R-02: a frame at the limit followed by CR and LF in separate reads is the same frame as in one read.
        for chunk in [1, 2, 3, 16, 64] {
            for split in [16, 17] {
                let input = [vec![b'x'; 16], b"\r\nnext\n".to_vec()].concat();
                let (head, tail) = input.split_at(split);
                let parts = Parts([head.to_vec(), tail.to_vec()].into());
                let mut reader = FrameReader::new(parts, 16, chunk);
                let mut got = Vec::new();
                while let Some(frame) = reader.next().await? {
                    got.push(frame);
                }
                assert_eq!(
                    got,
                    vec![Frame::Line(vec![b'x'; 16]), line("next")],
                    "chunk {chunk}, split {split}"
                );
                assert!(
                    reader.max_buffered() <= 16 + chunk.max(2),
                    "chunk {chunk}: {}",
                    reader.max_buffered()
                );
            }
        }
        // A CR that is not followed by LF is content: one byte over the limit.
        for rest in [&b"y\n"[..], &b""[..]] {
            let input = [vec![b'x'; 16], b"\r".to_vec(), rest.to_vec()].concat();
            let (got, _) = frames(&input, 16, 4).await?;
            let [Frame::TooLarge(refused)] = got.as_slice() else {
                return Err(format!("expected a refusal for {rest:?}, got {got:?}").into());
            };
            assert!(refused.length >= 17, "{}", refused.length);
        }
        Ok(())
    }

    #[tokio::test]
    async fn an_unterminated_frame_is_refused_within_limit_plus_one_read() -> R {
        let limit = 1 << 20;
        let chunk = 64 << 10;
        let input = [b"ok\n".to_vec(), vec![b'z'; 8 << 20]].concat();
        let (got, max) = frames(&input, limit, chunk).await?;
        assert_eq!(got.first(), Some(&line("ok")));
        let Some(Frame::TooLarge(refused)) = got.get(1) else {
            return Err(format!("expected a refused frame, got {:?}", got.get(1)).into());
        };
        assert_eq!(got.len(), 2, "nothing is framed after a refusal");
        assert!(
            refused.length > limit && refused.length <= limit + chunk,
            "{}",
            refused.length
        );
        assert_eq!(refused.head, vec![b'z'; HEAD_BYTES]);
        assert_eq!(
            refused.prefix_sha256,
            hex(&Sha256::digest(vec![b'z'; refused.length]))
        );
        assert!(max <= limit + chunk, "buffered {max} bytes");
        Ok(())
    }

    #[tokio::test]
    async fn many_small_frames_across_reads_keep_order_and_bound() -> R {
        let input: String = (0..5000).map(|i| format!("{i}\n")).collect();
        let (got, max) = frames(input.as_bytes(), 8, 7).await?;
        let expected: Vec<Frame> = (0..5000).map(|i| line(&i.to_string())).collect();
        assert_eq!(got, expected);
        assert!(max <= 8 + 7, "buffered {max} bytes");
        Ok(())
    }

    /// The rule of the CR-040 tests: a frame starting `{"type":"big",` is known.
    fn big(head: &[u8]) -> Option<&'static str> {
        by_prefix(head, &[(br#"{"type":"big","#, "big")])
    }

    const RULE: Skip = Skip {
        known: big,
        max_length: 1 << 20,
    };

    /// Every frame of `parts` (fed as separate reads) through a reader skipping by [`RULE`], and its peak.
    async fn skipped(
        parts: Vec<Vec<u8>>,
        limit: usize,
        chunk: usize,
        skip: Skip,
    ) -> io::Result<(Vec<Frame>, usize)> {
        let mut reader = FrameReader::new(Parts(parts.into()), limit, chunk).skipping(skip);
        let mut out = Vec::new();
        while let Some(frame) = reader.next().await? {
            out.push(frame);
        }
        Ok((out, reader.max_buffered()))
    }

    fn big_frame(len: usize) -> Vec<u8> {
        let mut frame = br#"{"type":"big","pad":""#.to_vec();
        frame.resize(len - 2, b'p');
        frame.extend_from_slice(br#""}"#);
        frame
    }

    /// Splits `bytes` into reads of `size` bytes.
    fn reads(bytes: &[u8], size: usize) -> Vec<Vec<u8>> {
        bytes.chunks(size).map(<[u8]>::to_vec).collect()
    }

    #[tokio::test]
    async fn test_027_05_a_known_frame_over_the_limit_is_skipped_to_its_lf_within_the_bound() -> R {
        // CR-040: a known frame of 200 bytes over a 16-byte limit, between two normal frames, whole and in reads of
        // every size: it is skipped with its length, sha256 and head, framing goes on, and the reader holds at most
        // the limit and one read.
        let frame = big_frame(200);
        let input = [b"ok\n".to_vec(), frame.clone(), b"\nnext\n".to_vec()].concat();
        for chunk in [2, 3, 7, 64] {
            for size in [1, 2, 5, 17, 64, input.len()] {
                let (got, max) = skipped(reads(&input, size), 16, chunk, RULE).await?;
                assert_eq!(
                    got,
                    vec![
                        line("ok"),
                        Frame::Skipped(SkippedFrame {
                            kind: "big",
                            length: 200,
                            sha256: hex(&Sha256::digest(&frame)),
                            head: frame.clone(),
                        }),
                        line("next"),
                    ],
                    "chunk {chunk}, reads of {size}"
                );
                assert!(max <= 16 + chunk, "chunk {chunk}, reads of {size}: {max}");
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_027_05_the_cr_before_the_lf_of_a_skipped_frame_is_dropped_across_reads() -> R {
        // The CR right before the LF is not part of the frame, however the reads split them; a CR inside the frame is.
        let frame = [big_frame(100), b"\rq".to_vec()].concat();
        let input = [frame.clone(), b"\r\nnext\r\n".to_vec()].concat();
        for chunk in [2, 3, 64] {
            for at in 90..input.len() {
                let (head, tail) = input.split_at(at);
                let (got, max) =
                    skipped(vec![head.to_vec(), tail.to_vec()], 16, chunk, RULE).await?;
                let [Frame::Skipped(s), next] = got.as_slice() else {
                    return Err(format!("chunk {chunk}, split {at}: {got:?}").into());
                };
                assert_eq!(next, &line("next"), "chunk {chunk}, split {at}");
                assert_eq!(s.length, 102, "chunk {chunk}, split {at}");
                assert_eq!(
                    s.sha256,
                    hex(&Sha256::digest(&frame)),
                    "chunk {chunk}, split {at}"
                );
                assert!(max <= 16 + chunk, "{max}");
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_027_05_a_known_frame_cut_short_by_the_end_of_the_stream_is_refused() -> R {
        // With no LF the frame is not complete: refused as too large, with every byte seen (a trailing CR included).
        for tail in [&b""[..], &b"\r"[..]] {
            let input = [b"ok\n".to_vec(), big_frame(300), tail.to_vec()].concat();
            let (got, max) = skipped(reads(&input, 7), 16, 4, RULE).await?;
            let [first, Frame::TooLarge(refused)] = got.as_slice() else {
                return Err(format!("expected a refusal, got {got:?}").into());
            };
            assert_eq!(first, &line("ok"));
            assert_eq!(refused.length, 300 + tail.len());
            assert_eq!(refused.prefix_sha256, hex(&Sha256::digest(&input[3..])));
            assert_eq!(refused.head, input[3..].to_vec());
            assert!(max <= 16 + 4, "{max}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_027_05_a_frame_over_the_limit_whose_head_is_not_known_is_refused_as_before() -> R
    {
        // Other types, a type that only starts like a known one, spacing or another first key: refused within the limit
        // and one read, nothing framed after it.
        for frame in [
            br#"{"type":"bigger","pad":""#.to_vec(),
            br#"{"type": "big","pad":""#.to_vec(),
            br#" {"type":"big","pad":""#.to_vec(),
            br#"{"pad":"","type":"big","#.to_vec(),
            br#"{"type":"bi"#.to_vec(),
        ] {
            let mut long = frame.clone();
            long.resize(300, b'p');
            let input = [long, b"\nnext\n".to_vec()].concat();
            let (got, max) = skipped(reads(&input, 64), 16, 8, RULE).await?;
            let [Frame::TooLarge(refused)] = got.as_slice() else {
                return Err(format!("{:?}: {got:?}", String::from_utf8_lossy(&frame)).into());
            };
            assert!(
                refused.length > 16 && refused.length <= 16 + 8,
                "{}",
                refused.length
            );
            assert!(max <= 16 + 8, "{max}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_027_05_a_known_frame_longer_than_the_skip_limit_is_refused() -> R {
        let rule = Skip {
            known: big,
            max_length: 1000,
        };
        let frame = big_frame(5000);
        let input = [frame.clone(), b"\nnext\n".to_vec()].concat();
        let (got, max) = skipped(reads(&input, 64), 16, 8, rule).await?;
        let [Frame::TooLarge(refused)] = got.as_slice() else {
            return Err(format!("expected a refusal, got {got:?}").into());
        };
        assert!(
            refused.length > 1000 && refused.length <= 1000 + 8,
            "{}",
            refused.length
        );
        assert_eq!(
            refused.prefix_sha256,
            hex(&Sha256::digest(&frame[..refused.length]))
        );
        assert!(max <= 16 + 8, "{max}");
        // At the limit it is skipped.
        let frame = big_frame(1000);
        let input = [frame, b"\nnext\n".to_vec()].concat();
        let (got, _) = skipped(reads(&input, 64), 16, 8, rule).await?;
        assert!(
            matches!(got.as_slice(), [Frame::Skipped(s), n] if s.length == 1000 && *n == line("next")),
            "{got:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_027_05_an_8_mib_line_without_lf_is_refused_within_the_bound_with_a_rule() -> R {
        // AC-027-03 is unchanged by a rule: an unknown 8 MiB line with no LF is refused within the limit and one read.
        let limit = 1 << 20;
        let chunk = 64 << 10;
        let input = [b"ok\n".to_vec(), vec![b'z'; 8 << 20]].concat();
        let skip = Skip {
            known: big,
            max_length: SKIP_LIMIT,
        };
        let (got, max) = skipped(reads(&input, chunk), limit, chunk, skip).await?;
        let [first, Frame::TooLarge(refused)] = got.as_slice() else {
            return Err(format!("expected a refusal, got {} frames", got.len()).into());
        };
        assert_eq!(first, &line("ok"));
        assert!(
            refused.length > limit && refused.length <= limit + chunk,
            "{}",
            refused.length
        );
        assert_eq!(refused.head, vec![b'z'; HEAD_BYTES]);
        assert!(max <= limit + chunk, "buffered {max} bytes");
        Ok(())
    }
}
