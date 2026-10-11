//! Line-indexed, memory-mapped access to very large text files.
//!
//! The file is never read into memory. A sparse index stores the byte offset
//! of every `stride`-th line, so any window of lines can be located by one
//! index lookup plus a short newline scan, and the index itself stays small
//! (8 bytes per `stride` lines).

use std::fs::File;
use std::io;
use std::path::Path;

use memchr::{memchr, memchr_iter, memmem};
use memmap2::Mmap;

/// Lines between two index entries. 1024 keeps the index at ~8 KiB per
/// million lines while bounding a lookup's scan to 1023 newlines.
pub const DEFAULT_STRIDE: u64 = 1024;

/// How many bytes are scanned between two progress callbacks during indexing.
const PROGRESS_CHUNK: usize = 16 << 20;

pub struct BigFile {
    // `None` for an empty file: mapping zero bytes is an error on some platforms.
    map: Option<Mmap>,
    stride: u64,
    // offsets[k] = byte offset of the first byte of line k * stride.
    offsets: Vec<u64>,
    line_count: u64,
}

/// One line of a window, cut to the caller's byte budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    /// True when the line was longer than the budget and `text` is a prefix.
    pub truncated: bool,
}

impl BigFile {
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::open_with(path, DEFAULT_STRIDE, |_, _| {})
    }

    /// Opens and indexes `path`. `progress(done_bytes, total_bytes)` is called
    /// roughly every 16 MiB and once at the end.
    pub fn open_with(
        path: impl AsRef<Path>,
        stride: u64,
        mut progress: impl FnMut(u64, u64),
    ) -> io::Result<Self> {
        assert!(stride > 0, "stride must be positive");
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        if len == 0 {
            progress(0, 0);
            return Ok(Self { map: None, stride, offsets: vec![0], line_count: 0 });
        }
        // SAFETY: the mapping is read-only. If another process truncates the
        // file while it is mapped, reads past the new end fault (SIGBUS); a
        // viewer accepts that risk in exchange for not copying the file.
        let map = unsafe { Mmap::map(&file)? };
        let bytes: &[u8] = &map;

        let mut offsets = vec![0u64];
        let mut newlines = 0u64;
        for (i, chunk) in bytes.chunks(PROGRESS_CHUNK).enumerate() {
            let base = (i * PROGRESS_CHUNK) as u64;
            for pos in memchr_iter(b'\n', chunk) {
                newlines += 1;
                if newlines % stride == 0 {
                    offsets.push(base + pos as u64 + 1);
                }
            }
            progress(base + chunk.len() as u64, len);
        }
        let ends_with_newline = bytes[bytes.len() - 1] == b'\n';
        let line_count = newlines + u64::from(!ends_with_newline);
        Ok(Self { map: Some(map), stride, offsets, line_count })
    }

    pub fn line_count(&self) -> u64 {
        self.line_count
    }

    pub fn len_bytes(&self) -> u64 {
        self.bytes().len() as u64
    }

    fn bytes(&self) -> &[u8] {
        self.map.as_deref().unwrap_or(&[])
    }

    /// Byte offset of the first byte of `line`, or `None` past the end.
    fn offset_of_line(&self, line: u64) -> Option<usize> {
        if line >= self.line_count {
            return None;
        }
        let bytes = self.bytes();
        let mut pos = self.offsets[(line / self.stride) as usize] as usize;
        for _ in 0..line % self.stride {
            pos += memchr(b'\n', &bytes[pos..])? + 1;
        }
        Some(pos)
    }

    /// Returns up to `count` lines starting at zero-based line `start`.
    /// Line terminators (`\n`, `\r\n`) are stripped, invalid UTF-8 is replaced,
    /// and each line is cut to at most `max_line_bytes` bytes.
    pub fn lines(&self, start: u64, count: usize, max_line_bytes: usize) -> Vec<Line> {
        let bytes = self.bytes();
        let Some(mut pos) = self.offset_of_line(start) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(count.min(4096));
        while out.len() < count && pos < bytes.len() {
            let end = memchr(b'\n', &bytes[pos..]).map_or(bytes.len(), |i| pos + i);
            let mut raw = &bytes[pos..end];
            if let [head @ .., b'\r'] = raw {
                raw = head;
            }
            let truncated = raw.len() > max_line_bytes;
            let shown = &raw[..raw.len().min(max_line_bytes)];
            out.push(Line { text: String::from_utf8_lossy(shown).into_owned(), truncated });
            pos = end + 1;
        }
        out
    }

    /// Zero-based number of the line containing byte offset `pos`.
    fn line_of_offset(&self, pos: usize) -> u64 {
        // Last index entry whose offset is <= pos.
        let block = self.offsets.partition_point(|&o| o as usize <= pos) - 1;
        let from = self.offsets[block] as usize;
        block as u64 * self.stride + memchr_iter(b'\n', &self.bytes()[from..pos]).count() as u64
    }

    /// First line at or after `from_line` that contains `needle` (byte-exact).
    /// An empty needle never matches.
    pub fn find_next(&self, needle: &[u8], from_line: u64) -> Option<u64> {
        if needle.is_empty() {
            return None;
        }
        let start = self.offset_of_line(from_line)?;
        let hit = memmem::find(&self.bytes()[start..], needle)?;
        Some(self.line_of_offset(start + hit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn file_with(content: &[u8]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content).unwrap();
        f.flush().unwrap();
        f
    }

    fn texts(lines: Vec<Line>) -> Vec<String> {
        lines.into_iter().map(|l| l.text).collect()
    }

    #[test]
    fn empty_file_has_no_lines() {
        let f = file_with(b"");
        let big = BigFile::open(f.path()).unwrap();
        assert_eq!(big.line_count(), 0);
        assert!(big.lines(0, 10, 100).is_empty());
        assert_eq!(big.find_next(b"x", 0), None);
    }

    #[test]
    fn trailing_newline_does_not_add_a_line() {
        let with = BigFile::open(file_with(b"a\nb\n").path()).unwrap();
        let without = BigFile::open(file_with(b"a\nb").path()).unwrap();
        assert_eq!(with.line_count(), 2);
        assert_eq!(without.line_count(), 2);
        assert_eq!(texts(with.lines(0, 10, 100)), ["a", "b"]);
        assert_eq!(texts(without.lines(0, 10, 100)), ["a", "b"]);
    }

    #[test]
    fn crlf_and_blank_lines() {
        let big = BigFile::open(file_with(b"one\r\n\r\nthree\r\n").path()).unwrap();
        assert_eq!(big.line_count(), 3);
        assert_eq!(texts(big.lines(0, 10, 100)), ["one", "", "three"]);
    }

    #[test]
    fn windows_match_a_naive_split_at_every_stride_boundary() {
        let mut content = Vec::new();
        for i in 0..1000 {
            writeln!(content, "line {i} {}", "x".repeat(i % 7)).unwrap();
        }
        let expected: Vec<String> =
            String::from_utf8(content.clone()).unwrap().lines().map(str::to_owned).collect();
        let f = file_with(&content);
        // Small strides put many index boundaries inside the file, including
        // one exactly at the end (1000 % 8 == 0, 1000 % 1000 == 0).
        for stride in [1, 2, 7, 8, 64, 1000, 4096] {
            let big = BigFile::open_with(f.path(), stride, |_, _| {}).unwrap();
            assert_eq!(big.line_count(), 1000, "stride {stride}");
            for start in [0u64, 1, 6, 7, 8, 63, 64, 65, 500, 992, 998, 999] {
                let got = texts(big.lines(start, 5, 1000));
                let end = (start as usize + 5).min(1000);
                assert_eq!(got, expected[start as usize..end], "stride {stride} start {start}");
            }
            assert!(big.lines(1000, 5, 1000).is_empty(), "stride {stride}");
        }
    }

    #[test]
    fn long_lines_are_truncated_and_flagged() {
        let big = BigFile::open(file_with(b"abcdefgh\nxy\n").path()).unwrap();
        let lines = big.lines(0, 2, 3);
        assert_eq!(lines[0], Line { text: "abc".into(), truncated: true });
        assert_eq!(lines[1], Line { text: "xy".into(), truncated: false });
    }

    #[test]
    fn invalid_utf8_is_replaced_not_fatal() {
        let big = BigFile::open(file_with(b"ok\n\xff\xfe\nend").path()).unwrap();
        let lines = texts(big.lines(0, 3, 100));
        assert_eq!(lines[0], "ok");
        assert_eq!(lines[1], "\u{fffd}\u{fffd}");
        assert_eq!(lines[2], "end");
    }

    #[test]
    fn search_reports_the_line_of_each_hit() {
        let mut content = Vec::new();
        for i in 0..5000 {
            let mark = if i == 17 || i == 4097 { " NEEDLE" } else { "" };
            writeln!(content, "row {i}{mark}").unwrap();
        }
        let f = file_with(&content);
        for stride in [1, 16, 1024] {
            let big = BigFile::open_with(f.path(), stride, |_, _| {}).unwrap();
            assert_eq!(big.find_next(b"NEEDLE", 0), Some(17), "stride {stride}");
            assert_eq!(big.find_next(b"NEEDLE", 17), Some(17), "stride {stride}");
            assert_eq!(big.find_next(b"NEEDLE", 18), Some(4097), "stride {stride}");
            assert_eq!(big.find_next(b"NEEDLE", 4098), None, "stride {stride}");
            assert_eq!(big.find_next(b"", 0), None, "stride {stride}");
            assert_eq!(big.find_next(b"NEEDLE", 5000), None, "stride {stride}");
        }
    }

    #[test]
    fn progress_ends_at_the_file_size() {
        let f = file_with(b"a\nb\nc\n");
        let mut last = (0, 0);
        BigFile::open_with(f.path(), 2, |done, total| last = (done, total)).unwrap();
        assert_eq!(last, (6, 6));
    }
}
