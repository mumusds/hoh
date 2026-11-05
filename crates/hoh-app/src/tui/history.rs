//! The project's input history, `.hoh/input-history.jsonl`: one `{"text": …}` object per line, oldest first
//! (REQ-001 decision: kept under the project's `.hoh/`). Loading never writes; a line that does not parse is skipped
//! and counted, and a file that cannot be read leaves the history empty with a notice (ADR-014).

use std::io::Write;
use std::path::{Path, PathBuf};

pub const FILE: &str = ".hoh/input-history.jsonl";

#[derive(Debug)]
pub struct History {
    path: PathBuf,
    entries: Vec<String>,
    notice: Option<String>,
}

impl History {
    /// The history of the project at `project`.
    pub fn load(project: &Path) -> History {
        let path = project.join(FILE);
        let (entries, notice) = match std::fs::read(&path) {
            Ok(bytes) => {
                let mut entries = Vec::new();
                let mut skipped = 0;
                for line in bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
                    match serde_json::from_slice::<serde_json::Value>(line)
                        .ok()
                        .and_then(|v| v.get("text")?.as_str().map(str::to_owned))
                    {
                        Some(text) => entries.push(text),
                        None => skipped += 1,
                    }
                }
                let notice = (skipped > 0)
                    .then(|| format!("输入历史中有 {skipped} 行无法解析，已跳过（{FILE}）"));
                (entries, notice)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Vec::new(), None),
            Err(e) => (
                Vec::new(),
                Some(format!("输入历史无法读取（{FILE}）：{e}；本次从空历史开始")),
            ),
        };
        History {
            path,
            entries,
            notice,
        }
    }

    /// Oldest first.
    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    /// Why some or all of the file could not be used, if so.
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Append `text` (creating `.hoh/` if needed) and keep it; written as one line and synced.
    pub fn append(&mut self, text: &str) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&self.path)?;
        // An unterminated last line (a crash mid-write, an edit by hand) gets its line break first, so this entry is
        // a line of its own (M1T08R-02). Two shells doing this at once leave an empty line, which loading skips.
        let mut line = Vec::new();
        if !ends_with_newline(&mut file)? {
            line.push(b'\n');
        }
        line.extend(serde_json::to_vec(&serde_json::json!({ "text": text }))?);
        line.push(b'\n');
        file.write_all(&line)?;
        file.sync_data()?;
        self.entries.push(text.to_owned());
        Ok(())
    }

    /// The newest entry before index `before` that contains `query`.
    pub fn search(&self, query: &str, before: usize) -> Option<usize> {
        self.entries[..before.min(self.entries.len())]
            .iter()
            .rposition(|e| e.contains(query))
    }
}

/// The file is empty or its last byte is a line break.
fn ends_with_newline(file: &mut std::fs::File) -> std::io::Result<bool> {
    use std::io::{Read, Seek, SeekFrom};
    if file.metadata()?.len() == 0 {
        return Ok(true);
    }
    file.seek(SeekFrom::End(-1))?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last)?;
    Ok(last[0] == b'\n')
}
