//! LF-delimited protocol frames with a hard size limit (docs/13-adapters.md "进程与终结", docs/02 REQ-026/REQ-027).
//!
//! Frames are split on the byte 0x0A only, never on U+2028/U+2029 or other line separators, and one CR before the
//! LF is dropped; decoding (UTF-8, JSON) is the adapter's job. A frame longer than the limit is refused without
//! buffering further: the reader holds at most `limit + chunk` bytes, reports the length it saw, the sha256 of
//! those bytes and their first 4 KiB, and yields no further frames.

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
}

/// Diagnosis for a refused frame (PROTOCOL_FRAME_TOO_LARGE).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameTooLarge {
    /// Bytes of the frame received when it was refused (more than the limit, at most limit + one read).
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
}

impl<R: AsyncRead + Unpin> FrameReader<R> {
    /// A reader refusing frames longer than `limit` bytes and reading at most `chunk` bytes at a time.
    pub fn new(inner: R, limit: usize, chunk: usize) -> Self {
        Self {
            inner,
            buf: Vec::new(),
            start: 0,
            scanned: 0,
            limit,
            chunk: chunk.max(1),
            max_buffered: 0,
            bytes: 0,
            done: false,
        }
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
                    return Ok(Some(self.refuse(frame_start, content_end)));
                }
                return Ok(Some(Frame::Line(
                    self.buf[frame_start..content_end].to_vec(),
                )));
            }
            self.scanned = self.buf.len() - self.start;
            if self.scanned > self.limit {
                return Ok(Some(self.refuse(self.start, self.buf.len())));
            }
            if self.start > 0 {
                self.buf.drain(..self.start);
                self.start = 0;
            }
            // Capacity stays within limit + chunk: it only grows here, while at most `limit` bytes are pending.
            if self.buf.capacity() - self.buf.len() < self.chunk {
                self.buf.reserve_exact(self.chunk);
            }
            let n = self.inner.read_buf(&mut self.buf).await?;
            self.max_buffered = self.max_buffered.max(self.buf.len());
            self.bytes += n as u64;
            if n == 0 {
                self.done = true;
                let rest = std::mem::take(&mut self.buf);
                return Ok((!rest.is_empty()).then_some(Frame::Partial(rest)));
            }
        }
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
}
