//! BIDS filename parser. Matches the algorithm in
//! `src/schema/entities.ts` (`_readEntities`) so output is comparable.

use indexmap::IndexMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFilename {
    pub stem: String,
    pub suffix: String,
    pub extension: String,
    /// Short-name → label. Preserves first-occurrence order.
    pub entities: IndexMap<String, String>,
}

/// Parse a BIDS filename into entities, suffix, and extension.
///
/// Mirrors the TS implementation:
///   * extension = substring from first `.`
///   * stem = everything before that
///   * suffix = last `_`-separated token in stem, **iff** there's more than
///     one token OR the last token is alphanumeric
///   * remaining tokens parse as `key-value` pairs on the first `-`
///   * tokens with no `-` get the sentinel label `"NOENTITY"`
pub fn parse_filename(filename: &str) -> ParsedFilename {
    let (stem, extension) = match filename.find('.') {
        Some(i) => (&filename[..i], &filename[i..]),
        None => (filename, ""),
    };

    let mut parts: Vec<&str> = stem.split('_').collect();
    let mut suffix = String::new();
    if !parts.is_empty() {
        let last = parts[parts.len() - 1];
        let alnum = !last.is_empty() && last.chars().all(|c| c.is_ascii_alphanumeric());
        if parts.len() > 1 || alnum {
            suffix = parts.pop().unwrap().to_string();
        }
    }

    let mut entities = IndexMap::new();
    for part in parts {
        match part.split_once('-') {
            Some((k, v)) => {
                entities.insert(k.to_string(), v.to_string());
            }
            None => {
                entities.insert(part.to_string(), "NOENTITY".to_string());
            }
        }
    }

    ParsedFilename {
        stem: stem.to_string(),
        suffix,
        extension: extension.to_string(),
        entities,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ent(pairs: &[(&str, &str)]) -> IndexMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn raw_t1w() {
        let p = parse_filename("sub-01_T1w.nii.gz");
        assert_eq!(p.suffix, "T1w");
        assert_eq!(p.extension, ".nii.gz");
        assert_eq!(p.entities, ent(&[("sub", "01")]));
    }

    #[test]
    fn bold_with_run() {
        let p = parse_filename("sub-01_ses-02_task-rest_run-1_bold.nii.gz");
        assert_eq!(p.suffix, "bold");
        assert_eq!(p.extension, ".nii.gz");
        assert_eq!(
            p.entities,
            ent(&[("sub", "01"), ("ses", "02"), ("task", "rest"), ("run", "1")])
        );
    }

    #[test]
    fn no_dot_no_extension() {
        let p = parse_filename("README");
        // Single alphanumeric token with no underscore: TS treats it as a suffix.
        assert_eq!(p.suffix, "README");
        assert_eq!(p.extension, "");
        assert!(p.entities.is_empty());
    }

    #[test]
    fn noentity_token() {
        let p = parse_filename("sub-01_weird_T1w.nii");
        assert_eq!(p.suffix, "T1w");
        assert_eq!(p.entities, ent(&[("sub", "01"), ("weird", "NOENTITY")]));
    }

    #[test]
    fn multi_dot_extension() {
        let p = parse_filename("sub-01_dwi.bval");
        assert_eq!(p.suffix, "dwi");
        assert_eq!(p.extension, ".bval");
    }
}
