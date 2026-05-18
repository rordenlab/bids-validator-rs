//! TSV loader.
//!
//! Mirrors `src/files/tsv.ts::_loadTSV`. The TS implementation throws on the
//! first parse error (`TSV_COLUMN_HEADER_DUPLICATE`, `TSV_EQUAL_ROWS`,
//! `TSV_EMPTY_LINE`) and the caller — `context.loadColumns` — catches it,
//! adds the file's path as `location`, and falls back to an empty columns
//! map. Downstream `evalColumns` then still runs and emits things like
//! `TSV_COLUMN_MISSING` for required columns. We reproduce that behavior
//! by returning both the parsed (possibly empty) columns and the parse
//! issue, if any.
//!
//! Scope: `.tsv` and `.tsv.gz` with a bounded in-memory buffer. A
//! streaming row iterator should eventually replace the full-buffer path.

use std::io::Read;
use std::path::Path;

use flate2::read::GzDecoder;
use indexmap::IndexMap;

use crate::issue::{Issue, Severity};
use crate::policy::ContentPolicy;

/// Parsed columns from a TSV file. Always keyed in header order.
#[derive(Debug, Default, Clone)]
pub struct TsvColumns {
    pub headers: Vec<String>,
    pub columns: IndexMap<String, Vec<String>>,
}

impl TsvColumns {
    pub fn is_empty(&self) -> bool {
        self.headers.is_empty()
    }
}

/// Hard cap on bytes read from a TSV (compressed or plain) before we
/// stop. 256 MiB is well above any legitimate BIDS events / channels
/// / participants file (typical: KB) and well under Deno's V8 string
/// limit (~2 GiB). Caps an adversarial / corrupt input from OOMing
/// the validator. Audited as High #9 (2026-05-17). The streaming
/// validator (audit Refactoring #7) will eventually replace the
/// full-buffer load with row-at-a-time iteration.
pub const TSV_BYTE_LIMIT: u64 = 256 * 1024 * 1024;

enum LimitedRead {
    Complete(Vec<u8>),
    TooLarge,
}

fn read_limited<R: Read>(reader: R, limit: u64) -> std::io::Result<LimitedRead> {
    let mut bytes = Vec::new();
    reader
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        Ok(LimitedRead::TooLarge)
    } else {
        Ok(LimitedRead::Complete(bytes))
    }
}

/// Load a TSV from disk, gated by `policy` and capped at
/// [`TSV_BYTE_LIMIT`] bytes. Returns the parsed columns (empty on
/// error, policy-skip, invalid UTF-8, or limit overflow) plus up to one
/// parse-time issue. The caller sets the issue's `location` to the
/// dataset-relative path.
pub fn load_tsv(path: &Path, policy: &ContentPolicy) -> (TsvColumns, Option<Issue>) {
    let Some(f) = policy.open(path) else {
        // Read errors (e.g. broken annex symlinks) and policy-skipped
        // reads produce no parse issue — matches TS where loadTSV
        // throws `Deno.errors.NotFound` and the catch-arm doesn't add
        // an issue (only `error.code` issues are surfaced). The empty
        // columns then propagate into evalColumns.
        return (TsvColumns::default(), None);
    };
    let bytes = match read_limited(f, TSV_BYTE_LIMIT) {
        Ok(LimitedRead::Complete(bytes)) => bytes,
        Ok(LimitedRead::TooLarge) => return (TsvColumns::default(), Some(file_read_limit_issue())),
        Err(_) => return (TsvColumns::default(), None),
    };
    let text = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => return (TsvColumns::default(), Some(invalid_file_encoding_issue())),
    };
    parse_tsv(&text)
}

/// Parse TSV content. Public so tests can drive it without the filesystem.
pub fn parse_tsv(text: &str) -> (TsvColumns, Option<Issue>) {
    // Mirrors Deno's `TextLineStream`: split on `\n`, then drop a trailing
    // `\r` per line. Trailing empty lines (text ending with `\n` or
    // `\r\n\r\n`) are *not* errors — TextLineStream's `peek next` in
    // `loadColumns` short-circuits on EOF before emitting `TSV_EMPTY_LINE`.
    let lines: Vec<&str> = text
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    let mut lines = lines;
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }

    if lines.is_empty() {
        return (TsvColumns::default(), None);
    }

    let header_line = lines[0];
    let headers: Vec<String> = if header_line.is_empty() {
        Vec::new()
    } else {
        header_line.split('\t').map(|s| s.to_string()).collect()
    };

    // `TSV_COLUMN_HEADER_DUPLICATE`: any repeat in headers. Message is the
    // full header list joined by `, ` (TS: `headers.join(', ')`).
    let mut seen = std::collections::HashSet::new();
    for h in &headers {
        if !seen.insert(h.as_str()) {
            let issue = Issue {
                code: "TSV_COLUMN_HEADER_DUPLICATE".to_string(),
                sub_code: None,
                severity: Severity::Error,
                location: String::new(), // filled by caller
                rule: None,
                issue_message: Some(headers.join(", ")),
                affects: None,
                line: None,
                code_message: None,
            };
            return (TsvColumns::default(), Some(issue));
        }
    }

    parse_rows(&lines[1..], headers, 1)
}

/// Load a `.tsv.gz` body using sidecar-provided `Columns` headers,
/// gated by `policy`. Mirrors `src/files/tsv.ts::loadTSVGZ`: there is
/// no header row in the compressed data, so duplicate-header checks
/// are not performed here.
pub fn load_tsvgz(
    path: &Path,
    headers: &[String],
    policy: &ContentPolicy,
) -> (TsvColumns, Option<Issue>) {
    let Some(f) = policy.open(path) else {
        return (TsvColumns::default(), None);
    };
    // Cap inflated bytes at TSV_BYTE_LIMIT to bound gzip-bomb exposure.
    // The decoder reads compressed input; the helper caps decompressed
    // output. If the cap is hit, skip tabular validation instead of
    // validating a silently truncated body.
    let decoder = GzDecoder::new(f);
    let bytes = match read_limited(decoder, TSV_BYTE_LIMIT) {
        Ok(LimitedRead::Complete(bytes)) => bytes,
        Ok(LimitedRead::TooLarge) => return (TsvColumns::default(), Some(file_read_limit_issue())),
        Err(_) => return (TsvColumns::default(), Some(invalid_gzip_issue())),
    };
    let text = match String::from_utf8(bytes) {
        Ok(s) => s,
        // Deno's loadTSVGZ catch-arm maps UTF-8 decoder failures to
        // INVALID_GZIP because they surface from the decompression/UTF-8
        // stream chain without a BIDS issue code.
        Err(_) => return (TsvColumns::default(), Some(invalid_gzip_issue())),
    };
    parse_tsv_body_with_headers(&text, headers)
}

fn file_read_limit_issue() -> Issue {
    Issue {
        code: "FILE_READ".to_string(),
        sub_code: Some("TooLarge".to_string()),
        severity: Severity::Error,
        location: String::new(),
        rule: None,
        issue_message: Some(format!(
            "TSV read limit exceeded {TSV_BYTE_LIMIT} bytes; skipped tabular validation."
        )),
        affects: None,
        line: None,
        code_message: None,
    }
}

fn invalid_file_encoding_issue() -> Issue {
    Issue {
        code: "INVALID_FILE_ENCODING".to_string(),
        sub_code: None,
        severity: Severity::Error,
        location: String::new(),
        rule: None,
        issue_message: None,
        affects: None,
        line: None,
        code_message: None,
    }
}

pub fn parse_tsv_body_with_headers(text: &str, headers: &[String]) -> (TsvColumns, Option<Issue>) {
    let mut lines: Vec<&str> = text
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    parse_rows(&lines, headers.to_vec(), 0)
}

fn parse_rows(
    lines: &[&str],
    headers: Vec<String>,
    start_row: usize,
) -> (TsvColumns, Option<Issue>) {
    let mut columns: Vec<Vec<String>> = headers.iter().map(|_| Vec::new()).collect();

    for (i, line) in lines.iter().enumerate() {
        // TS empty-line check is: if the line is empty AND it's not the
        // last line of the file, throw `TSV_EMPTY_LINE`. With our
        // pre-stripped trailing blank, any empty line here is mid-file.
        if line.is_empty() {
            let issue = Issue {
                code: "TSV_EMPTY_LINE".to_string(),
                sub_code: None,
                severity: Severity::Error,
                location: String::new(),
                rule: None,
                issue_message: None,
                affects: None,
                // TS: `line: rowIndex + startRow + 1`.
                line: Some(i + start_row + 1),
                code_message: None,
            };
            return (TsvColumns::default(), Some(issue));
        }
        let values: Vec<&str> = line.split('\t').collect();
        if values.len() != headers.len() {
            let issue = Issue {
                code: "TSV_EQUAL_ROWS".to_string(),
                sub_code: None,
                severity: Severity::Error,
                location: String::new(),
                rule: None,
                issue_message: None,
                affects: None,
                line: Some(i + start_row + 1),
                code_message: None,
            };
            return (TsvColumns::default(), Some(issue));
        }
        for (col, val) in columns.iter_mut().zip(values) {
            col.push(val.to_string());
        }
    }

    let mut map: IndexMap<String, Vec<String>> = IndexMap::new();
    for (h, c) in headers.iter().zip(columns) {
        map.insert(h.clone(), c);
    }

    (
        TsvColumns {
            headers,
            columns: map,
        },
        None,
    )
}

fn invalid_gzip_issue() -> Issue {
    Issue {
        code: "INVALID_GZIP".to_string(),
        sub_code: None,
        severity: Severity::Error,
        location: String::new(),
        rule: None,
        issue_message: None,
        affects: None,
        line: None,
        code_message: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn parses_simple_tsv() {
        let (cols, err) = parse_tsv("a\tb\n1\t2\n3\t4\n");
        assert!(err.is_none(), "got {err:?}");
        assert_eq!(cols.headers, vec!["a", "b"]);
        assert_eq!(cols.columns["a"], vec!["1", "3"]);
        assert_eq!(cols.columns["b"], vec!["2", "4"]);
    }

    #[test]
    fn duplicate_headers() {
        let (cols, err) = parse_tsv("a\tb\ta\n1\t2\t3\n");
        let issue = err.expect("expected duplicate-header issue");
        assert_eq!(issue.code, "TSV_COLUMN_HEADER_DUPLICATE");
        assert!(cols.is_empty());
        assert!(issue.issue_message.unwrap().contains("a, b, a"));
    }

    #[test]
    fn unequal_rows() {
        let (cols, err) = parse_tsv("a\tb\n1\t2\n3\n");
        let issue = err.expect("expected unequal-rows issue");
        assert_eq!(issue.code, "TSV_EQUAL_ROWS");
        assert_eq!(issue.line, Some(3));
        assert!(cols.is_empty());
    }

    #[test]
    fn empty_line_mid_file() {
        let (_cols, err) = parse_tsv("a\tb\n1\t2\n\n3\t4\n");
        let issue = err.expect("expected empty-line issue");
        assert_eq!(issue.code, "TSV_EMPTY_LINE");
        assert_eq!(issue.line, Some(3));
    }

    #[test]
    fn trailing_newline_is_fine() {
        let (cols, err) = parse_tsv("a\tb\n1\t2\n");
        assert!(err.is_none());
        assert_eq!(cols.columns["a"], vec!["1"]);
    }

    #[test]
    fn handles_crlf() {
        let (cols, err) = parse_tsv("a\tb\r\n1\t2\r\n");
        assert!(err.is_none());
        assert_eq!(cols.columns["a"], vec!["1"]);
    }

    #[test]
    fn crlf_with_trailing_blank_is_fine() {
        // Windows editors often emit `data\r\n\r\n`. After `split('\n')`
        // and CRLF strip, that's `["data", "", ""]`. All trailing empties
        // must be popped so this is not mistaken for a mid-file blank.
        let (cols, err) = parse_tsv("a\tb\r\n1\t2\r\n\r\n");
        assert!(err.is_none(), "got {err:?}");
        assert_eq!(cols.columns["a"], vec!["1"]);
    }

    #[test]
    fn body_with_headers_uses_first_line_as_data() {
        let headers = vec!["a".to_string(), "b".to_string()];
        let (cols, err) = parse_tsv_body_with_headers("1\t2\n3\t4\n", &headers);
        assert!(err.is_none());
        assert_eq!(cols.columns["a"], vec!["1", "3"]);
        assert_eq!(cols.columns["b"], vec!["2", "4"]);
    }

    #[test]
    fn read_limited_reports_limit_without_truncating() {
        match read_limited(Cursor::new(b"abcdef"), 5).unwrap() {
            LimitedRead::TooLarge => {}
            LimitedRead::Complete(_) => panic!("expected read limit to trip"),
        }
    }
}
