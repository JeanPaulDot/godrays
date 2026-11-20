//! Follows a log file as it grows.
//!
//! The packaged Kyber programs print nothing to their console; what they do
//! is only visible in their log files. The launcher follows those files to
//! show what is happening and to react to it (stream is up, host went away,
//! sign-in refused...).

use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

pub struct LogTail {
    path: PathBuf,
    offset: u64,
    /// Bytes of a line that has not been finished yet.
    partial: Vec<u8>,
}

impl LogTail {
    /// Follows `path` from its current end: only lines written from now on
    /// are reported. The file does not need to exist yet.
    pub fn from_end(path: PathBuf) -> Self {
        let offset = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        Self {
            path,
            offset,
            partial: Vec::new(),
        }
    }

    /// Complete lines added since the last call, without their timestamp.
    pub fn poll(&mut self) -> Vec<String> {
        let Ok(mut file) = std::fs::File::open(&self.path) else {
            return Vec::new();
        };
        let length = file.metadata().map(|m| m.len()).unwrap_or(0);
        if length < self.offset {
            // The file was replaced or truncated: start over.
            self.offset = 0;
            self.partial.clear();
        }
        if length == self.offset || file.seek(SeekFrom::Start(self.offset)).is_err() {
            return Vec::new();
        }
        let mut added = Vec::new();
        if file.read_to_end(&mut added).is_err() {
            return Vec::new();
        }
        self.offset += added.len() as u64;
        self.partial.extend_from_slice(&added);

        let mut lines = Vec::new();
        while let Some(end) = self.partial.iter().position(|b| *b == b'\n') {
            let raw: Vec<u8> = self.partial.drain(..=end).collect();
            let line = String::from_utf8_lossy(&raw);
            let line = strip_timestamp(line.trim_end());
            if !line.is_empty() {
                lines.push(line.to_owned());
            }
        }
        lines
    }
}

/// Drops a leading `2026-11-16T09:47:18.938[1234]` style timestamp.
fn strip_timestamp(line: &str) -> &str {
    match line.split_once(' ') {
        Some((first, rest))
            if first.len() >= 19
                && first.as_bytes()[4] == b'-'
                && first.as_bytes()[10] == b'T'
                && first.starts_with(|c: char| c.is_ascii_digit()) =>
        {
            rest
        }
        _ => line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn only_new_complete_lines_are_reported() {
        let dir = std::env::temp_dir().join(format!("godrays-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("kyclient.log");
        std::fs::write(&path, "2026-11-16T09:00:00.000 INFO old - before\n").unwrap();

        let mut tail = LogTail::from_end(path.clone());
        assert!(tail.poll().is_empty());

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        write!(
            file,
            "2026-11-16T09:47:18.938 INFO kyclient - Player started\nhalf a li"
        )
        .unwrap();
        assert_eq!(tail.poll(), ["INFO kyclient - Player started"]);
        assert!(tail.poll().is_empty());

        writeln!(file, "ne\r").unwrap();
        writeln!(file, "2026-11-16T09:51:28.544[1412] WARN x - y").unwrap();
        assert_eq!(tail.poll(), ["half a line", "WARN x - y"]);

        // A replaced, shorter file is read from its start.
        std::fs::write(&path, "fresh\n").unwrap();
        assert_eq!(tail.poll(), ["fresh"]);
    }

    #[test]
    fn a_missing_file_is_simply_empty_until_it_appears() {
        let path = std::env::temp_dir().join(format!("godrays-none-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut tail = LogTail::from_end(path.clone());
        assert!(tail.poll().is_empty());
        std::fs::write(&path, "now\n").unwrap();
        assert_eq!(tail.poll(), ["now"]);
        let _ = std::fs::remove_file(&path);
    }
}
