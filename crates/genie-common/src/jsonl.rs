//! Bounded tail reads for append-only JSONL logs.
//!
//! Dashboard and tool paths only need the last *N* lines; loading the entire
//! file on every poll grows memory with log age (issue #223).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const TAIL_CHUNK_BYTES: usize = 4096;
/// Skip individual lines larger than this instead of allocating them whole.
pub const DEFAULT_MAX_JSONL_LINE_BYTES: usize = 256 * 1024;

/// Return up to `limit` trailing non-empty lines from `path`, in file order.
///
/// Peak buffering is bounded by `max_line_bytes` regardless of how long the
/// file's longest line is: a line is abandoned as soon as it passes the limit,
/// not after it has been read into memory in full.
pub fn tail_lines(
    path: &Path,
    limit: usize,
    max_line_bytes: usize,
) -> std::io::Result<Vec<String>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    if !path.exists() {
        return Ok(Vec::new());
    }

    let mut file = File::open(path)?;
    let mut pos = file.metadata()?.len();
    if pos == 0 {
        return Ok(Vec::new());
    }

    let mut collected: Vec<Vec<u8>> = Vec::new();
    let mut current_line: Vec<u8> = Vec::new();
    // Set once the line under construction has passed `max_line_bytes`. From
    // that point its remaining bytes are discarded rather than buffered: the
    // line is already known to be droppable, so holding the rest of it only
    // costs memory. Previously the size test happened at the terminating
    // newline, which meant a multi-megabyte line was allocated in full and
    // *then* thrown away — the opposite of what `max_line_bytes` documents, and
    // the allocation lands on every poll of a log that contains one.
    let mut current_line_oversize = false;
    let mut chunk = [0u8; TAIL_CHUNK_BYTES];

    while pos > 0 && collected.len() < limit {
        let read_size = std::cmp::min(TAIL_CHUNK_BYTES as u64, pos) as usize;
        pos -= read_size as u64;
        file.seek(SeekFrom::Start(pos))?;
        file.read_exact(&mut chunk[..read_size])?;

        for &byte in chunk[..read_size].iter().rev() {
            if byte == b'\n' {
                if current_line_oversize {
                    // Reached the start of an over-long line; drop it and
                    // resume with the next one. `current_line` is already empty.
                    current_line_oversize = false;
                    continue;
                }
                if !current_line.is_empty() {
                    current_line.reverse();
                    collected.push(std::mem::take(&mut current_line));
                    if collected.len() >= limit {
                        break;
                    }
                }
            } else if !current_line_oversize {
                current_line.push(byte);
                if current_line.len() > max_line_bytes {
                    current_line.clear();
                    current_line_oversize = true;
                }
            }
        }

        if collected.len() >= limit {
            break;
        }
    }

    // A file whose first line has no leading newline leaves a trailing
    // fragment. Skip it when it overflowed, exactly as the newline path does.
    if collected.len() < limit && !current_line_oversize && !current_line.is_empty() {
        current_line.reverse();
        collected.push(current_line);
    }

    collected.reverse();
    Ok(collected
        .into_iter()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_log(path: &Path, lines: &[&str]) {
        let mut file = std::fs::File::create(path).unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
    }

    #[test]
    fn tail_lines_returns_last_entries_in_order() {
        let path = std::env::temp_dir().join(format!(
            "geniepod-jsonl-tail-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);
        write_log(
            &path,
            &[r#"{"n":1}"#, r#"{"n":2}"#, r#"{"n":3}"#, r#"{"n":4}"#],
        );

        let lines = tail_lines(&path, 2, DEFAULT_MAX_JSONL_LINE_BYTES).unwrap();
        assert_eq!(
            lines,
            vec![r#"{"n":3}"#.to_string(), r#"{"n":4}"#.to_string()]
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn tail_lines_skips_oversize_lines() {
        let path = std::env::temp_dir().join(format!(
            "geniepod-jsonl-tail-big-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);
        write_log(
            &path,
            &[
                r#"{"n":1}"#,
                &format!(r#"{{"blob":"{}"}}"#, "x".repeat(512)),
                r#"{"n":3}"#,
            ],
        );

        let lines = tail_lines(&path, 3, 64).unwrap();
        assert_eq!(
            lines,
            vec![r#"{"n":1}"#.to_string(), r#"{"n":3}"#.to_string()]
        );
        let _ = std::fs::remove_file(&path);
    }

    /// The existing oversize test uses a 512-byte line, which fits inside a
    /// single 4096-byte read. An oversize line long enough to span many chunks
    /// exercises the state that has to survive across loop iterations — the
    /// abandon flag is set in one chunk and cleared by a newline several chunks
    /// later, and the lines on either side must still come back intact.
    #[test]
    fn tail_lines_skips_an_oversize_line_spanning_many_chunks() {
        let path = std::env::temp_dir().join(format!(
            "geniepod-jsonl-tail-spanning-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);
        // ~64 KiB of payload: 16 full TAIL_CHUNK_BYTES reads.
        let huge = format!(r#"{{"blob":"{}"}}"#, "x".repeat(64 * 1024));
        write_log(&path, &[r#"{"n":1}"#, &huge, r#"{"n":3}"#]);

        let lines = tail_lines(&path, 5, 4096).unwrap();
        assert_eq!(
            lines,
            vec![r#"{"n":1}"#.to_string(), r#"{"n":3}"#.to_string()]
        );

        // Raising the cap above the line length brings it back, so the skip is
        // driven by `max_line_bytes` and not by the chunking.
        let lines = tail_lines(&path, 5, 1024 * 1024).unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[1], huge);

        let _ = std::fs::remove_file(&path);
    }

    /// A line of exactly `max_line_bytes` is kept; one byte more is dropped.
    /// The abandon check fires on `len > max`, so an off-by-one here would
    /// silently start discarding lines that used to be returned.
    #[test]
    fn tail_lines_boundary_line_length_is_inclusive() {
        let path = std::env::temp_dir().join(format!(
            "geniepod-jsonl-tail-boundary-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);
        let exact = "a".repeat(32);
        let over = "b".repeat(33);
        write_log(&path, &[&exact, &over]);

        let lines = tail_lines(&path, 5, 32).unwrap();
        assert_eq!(lines, vec![exact]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn tail_lines_handles_large_files_without_reading_whole_file() {
        let path = std::env::temp_dir().join(format!(
            "geniepod-jsonl-tail-large-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);
        let mut file = std::fs::File::create(&path).unwrap();
        for idx in 0..5_000 {
            writeln!(file, r#"{{"idx":{idx}}}"#).unwrap();
        }
        drop(file);

        let lines = tail_lines(&path, 3, DEFAULT_MAX_JSONL_LINE_BYTES).unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], r#"{"idx":4997}"#);
        assert_eq!(lines[2], r#"{"idx":4999}"#);
        let _ = std::fs::remove_file(&path);
    }
}
