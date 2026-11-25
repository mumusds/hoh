//! CR-041: a streaming JSON string reducer. The JSON grammar outside strings is checked by serde at EOF;
//! every string unit (including the discarded suffix) is decoded and validated before it is counted/hashed.
//! No unbounded token, string, array or object is accumulated. The caller retains the original stream in its Tee.

use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const PREFIX: usize = 4 << 10;
pub const OUTPUT: usize = 512 << 10;

#[derive(Debug, Default)]
pub(super) struct Json {
    output: Vec<u8>,
    string: Option<Text>,
    last_truncated: bool,
    pub truncated: bool,
    peak: usize,
}

#[derive(Debug)]
struct Text {
    prefix: Vec<u8>,
    unit: Vec<u8>,
    length: u64,
    hash: Sha256,
    truncated: bool,
}

impl Default for Text {
    fn default() -> Self {
        Self {
            prefix: Vec::new(),
            unit: Vec::new(),
            length: 0,
            hash: Sha256::new(),
            truncated: false,
        }
    }
}

impl Text {
    /// A complete UTF-8 code point or JSON escape (a surrogate pair is one unit), without its surrounding quotes.
    fn take(&mut self, b: u8) -> Result<(), ()> {
        self.unit.push(b);
        let n = match self.unit[0] {
            b'\\' => {
                if self.unit.len() < 2 {
                    return Ok(());
                }
                if self.unit[1] != b'u' {
                    2
                } else {
                    if self.unit.len() < 6 {
                        return Ok(());
                    }
                    let digits = std::str::from_utf8(&self.unit[2..6]).map_err(|_| ())?;
                    let code = u16::from_str_radix(digits, 16).map_err(|_| ())?;
                    if (0xd800..=0xdbff).contains(&code) {
                        12
                    } else {
                        6
                    }
                }
            }
            0x20..=0x7f => 1,
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => return Err(()),
        };
        if self.unit.len() < n {
            return Ok(());
        }
        if self.unit[0] == b'\\' {
            // At most 14 bytes; serde validates escapes, hex digits and surrogate pairs, including in the suffix.
            let mut quoted = [0u8; 14];
            quoted[0] = b'"';
            quoted[1..=n].copy_from_slice(&self.unit);
            quoted[n + 1] = b'"';
            let decoded: String = serde_json::from_slice(&quoted[..n + 2]).map_err(|_| ())?;
            self.hash.update(decoded.as_bytes());
            self.length += decoded.len() as u64;
        } else {
            std::str::from_utf8(&self.unit).map_err(|_| ())?;
            self.hash.update(&self.unit);
            self.length += n as u64;
        }
        if !self.truncated && self.prefix.len() + n <= PREFIX {
            self.prefix.extend_from_slice(&self.unit);
        } else {
            self.truncated = true;
        }
        self.unit.clear();
        Ok(())
    }
}

impl Json {
    fn append(&mut self, bytes: &[u8]) -> Result<(), ()> {
        if self.output.len() + bytes.len() > OUTPUT {
            return Err(());
        }
        let capacity = (self.output.len() + bytes.len())
            .next_multiple_of(PREFIX)
            .min(OUTPUT);
        self.output.reserve_exact(capacity - self.output.len());
        self.output.extend_from_slice(bytes);
        Ok(())
    }

    pub fn buffered(&self) -> usize {
        self.peak.max(
            self.output.capacity()
                + self
                    .string
                    .as_ref()
                    .map_or(0, |s| s.prefix.capacity() + s.unit.capacity()),
        )
    }

    pub fn take(&mut self, bytes: &[u8]) -> Result<(), ()> {
        for &b in bytes {
            if let Some(s) = &mut self.string {
                if b == b'"' && s.unit.is_empty() {
                    let s = self.string.take().ok_or(())?;
                    let token_capacity = s.prefix.capacity() + s.unit.capacity();
                    self.last_truncated = s.truncated;
                    if s.truncated {
                        self.truncated = true;
                        self.append(b"{\"truncated\":true,\"prefix\":\"")?;
                        self.append(&s.prefix)?;
                        self.append(
                            format!(
                                "\",\"original_bytes\":{},\"sha256\":\"{}\"}}",
                                s.length,
                                super::hex(&s.hash.finalize())
                            )
                            .as_bytes(),
                        )?;
                    } else {
                        self.append(b"\"")?;
                        self.append(&s.prefix)?;
                        self.append(b"\"")?;
                    }
                    self.peak = self.peak.max(self.output.capacity() + token_capacity);
                } else {
                    s.take(b)?;
                }
            } else if b == b'"' {
                self.last_truncated = false;
                self.string = Some(Text::default());
            } else {
                // A key may never be reduced: replacing it would lose fields or change the object's meaning.
                if b == b':' && self.last_truncated {
                    return Err(());
                }
                if !b.is_ascii_whitespace() {
                    self.last_truncated = false;
                }
                self.append(&[b])?;
            }
            self.peak = self.peak.max(self.buffered());
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<Vec<u8>, ()> {
        if self.output.last() == Some(&b'\r') {
            self.output.pop();
        }
        if self.string.is_some() {
            return Err(());
        }
        // IgnoredAny validates the complete reduced grammar with serde's depth bound, without allocating a Value tree.
        let mut de = serde_json::Deserializer::from_slice(&self.output);
        serde::de::IgnoredAny::deserialize(&mut de).map_err(|_| ())?;
        de.end().map_err(|_| ())?;
        Ok(self.output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn test_027_06_string_suffix_is_validated_and_hashed_after_truncation() {
        // Raw escapes split anywhere, including a surrogate pair; digest/length describe decoded UTF-8, not spelling.
        let raw = format!(
            r#"{{"x":"{}\uD83D\uDE00\n\"\\é","id":"tail","n":7}}"#,
            "a".repeat(PREFIX)
        );
        let full: Value = serde_json::from_str(&raw).expect("valid");
        for chunk in [1, 2, 7, 4096, 65536] {
            let mut parser = Json::default();
            for part in raw.as_bytes().chunks(chunk) {
                parser.take(part).expect("valid chunk");
            }
            assert!(parser.buffered() <= OUTPUT);
            let value: Value =
                serde_json::from_slice(&parser.finish().expect("finish")).expect("reduced JSON");
            assert_eq!(value["x"]["truncated"], true);
            let original = full["x"].as_str().expect("string");
            assert_eq!(value["x"]["original_bytes"], original.len());
            assert_eq!(
                value["x"]["sha256"],
                super::super::hex(&Sha256::digest(original.as_bytes()))
            );
            assert_eq!(value["x"]["prefix"], "a".repeat(PREFIX));
            assert_eq!(value["id"], "tail");
            assert_eq!(value["n"], 7);
        }
    }

    #[test]
    fn test_027_06_invalid_discarded_tail_large_keys_and_structure_are_refused() {
        for bad in [
            b"\\q".as_slice(),
            b"\\uD800x",
            b"\\uDC00",
            b"\\uD800\\u0020",
            b"\xff",
            b"\xc0\x80",
            b"\xed\xa0\x80",
            b"\x01",
        ] {
            let bytes = [
                b"{\"x\":\"".to_vec(),
                vec![b'a'; PREFIX + 1],
                bad.to_vec(),
                b"\"}".to_vec(),
            ]
            .concat();
            let mut parser = Json::default();
            assert!(
                parser.take(&bytes).is_err() || parser.finish().is_err(),
                "{bad:?}"
            );
        }
        for bad in [
            format!("{{\"{}\":1}}", "k".repeat(PREFIX + 1)),
            format!("{{\"x\":\"{}\"}} trailing", "x".repeat(PREFIX + 1)),
            format!("[{}0]", "0,".repeat(OUTPUT)),
        ] {
            let mut parser = Json::default();
            assert!(parser.take(bad.as_bytes()).is_err() || parser.finish().is_err());
        }
        // The boundary itself is unchanged and valid escaped/UTF-8 keys and values survive exactly as values.
        let value = json!({"x":"é".repeat(PREFIX / 2), "\\\"":"\u{0000}"});
        let mut parser = Json::default();
        parser.take(value.to_string().as_bytes()).expect("boundary");
        assert_eq!(
            serde_json::from_slice::<Value>(&parser.finish().expect("finish")).expect("JSON"),
            value
        );
    }
}
