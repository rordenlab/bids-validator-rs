//! Gzip header metadata parser.
//!
//! The BIDS checks only need the privacy-sensitive header fields:
//! timestamp, original filename, and comment. This mirrors
//! `src/files/gzip.ts::parseGzip` and reads only the first kilobyte.

use std::io::Read;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GzipMetadata {
    pub timestamp: u32,
    pub filename: String,
    pub comment: String,
}

pub fn parse_gzip<R: Read>(mut reader: R, max_bytes: usize) -> Option<GzipMetadata> {
    let mut buf = vec![0u8; max_bytes];
    let mut n = 0;
    while n < max_bytes {
        match reader.read(&mut buf[n..]).ok()? {
            0 => break,
            k => n += k,
        }
    }
    buf.truncate(n);
    parse_gzip_header(&buf)
}

fn parse_gzip_header(buf: &[u8]) -> Option<GzipMetadata> {
    if buf.len() < 10 || buf[0] != 0x1f || buf[1] != 0x8b {
        return None;
    }

    let flags = buf[3];
    let has_extra = flags & 0x04 != 0;
    let has_filename = flags & 0x08 != 0;
    let has_comment = flags & 0x10 != 0;
    let timestamp = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);

    let mut offset = 10usize;
    if has_extra {
        if offset + 2 > buf.len() {
            return None;
        }
        let xlen = u16::from_le_bytes([buf[offset], buf[offset + 1]]) as usize;
        offset = offset.checked_add(2 + xlen)?;
        if offset > buf.len() {
            return None;
        }
    }

    let mut filename = String::new();
    if has_filename {
        let end = buf[offset..].iter().position(|b| *b == 0)? + offset;
        filename = String::from_utf8_lossy(&buf[offset..end]).into_owned();
        offset = end + 1;
    }

    let mut comment = String::new();
    if has_comment {
        let end = buf[offset..].iter().position(|b| *b == 0)? + offset;
        comment = String::from_utf8_lossy(&buf[offset..end]).into_owned();
    }

    Some(GzipMetadata {
        timestamp,
        filename,
        comment,
    })
}

pub fn gzip_to_json(gzip: &GzipMetadata) -> serde_json::Value {
    serde_json::json!({
        "timestamp": gzip.timestamp,
        "filename": gzip.filename,
        "comment": gzip.comment,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn parses_plain_header() {
        let buf = [0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3];
        let gzip = parse_gzip_header(&buf).unwrap();
        assert_eq!(gzip.timestamp, 0);
        assert_eq!(gzip.filename, "");
        assert_eq!(gzip.comment, "");
    }

    #[test]
    fn parses_filename_and_comment() {
        let mut buf = vec![0x1f, 0x8b, 8, 0x18, 0xfe, 0xca, 0xd5, 0xb1, 0, 3];
        buf.extend_from_slice(b"stamped\0comment\0");
        let gzip = parse_gzip_header(&buf).unwrap();
        assert_eq!(gzip.timestamp, 0xb1d5cafe);
        assert_eq!(gzip.filename, "stamped");
        assert_eq!(gzip.comment, "comment");
    }

    #[test]
    fn parses_from_reader() {
        let buf = [0x1f, 0x8b, 8, 0, 1, 2, 3, 4, 0, 3];
        let gzip = parse_gzip(Cursor::new(buf), 1024).unwrap();
        assert_eq!(gzip.timestamp, 0x04030201);
    }
}
