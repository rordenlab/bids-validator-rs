//! Rust consumers for Deno-exported portable oracle cases.
//!
//! Regenerate the JSON manifests with:
//!
//! ```sh
//! /Users/chris/.deno/bin/deno run --allow-read --allow-write --allow-env rust/tests/oracle/export.ts
//! ```

use std::fs::File;
use std::path::{Path, PathBuf};

use bids_core::filename::parse_filename;
use bids_core::gzip::{gzip_to_json, parse_gzip};
use bids_core::tiff::{ome_to_json, parse_tiff, tiff_to_json};
use serde::Deserialize;
use serde_json::{json, Value as J};

#[derive(Debug, Deserialize)]
struct Manifest<T> {
    #[serde(rename = "schemaVersion")]
    schema_version: u8,
    cases: Vec<T>,
}

#[derive(Debug, Deserialize)]
struct FilenameCase {
    id: String,
    input: String,
    expected: J,
}

#[derive(Debug, Deserialize)]
struct GzipCase {
    id: String,
    dataset: PathBuf,
    path: PathBuf,
    #[serde(rename = "maxBytes")]
    max_bytes: usize,
    expected: J,
}

#[derive(Debug, Deserialize)]
struct TiffCase {
    id: String,
    dataset: PathBuf,
    path: PathBuf,
    #[serde(rename = "parseOme")]
    parse_ome: bool,
    expected: J,
}

#[test]
fn deno_filename_oracle_cases_pass() {
    let Some(manifest) = read_manifest::<FilenameCase>("entities.json") else {
        return;
    };
    assert_eq!(manifest.schema_version, 1);
    assert!(!manifest.cases.is_empty());

    for case in manifest.cases {
        let parsed = parse_filename(&case.input);
        let actual = json!({
            "stem": parsed.stem,
            "entities": parsed.entities,
            "suffix": parsed.suffix,
            "extension": parsed.extension,
        });
        assert_json_eq(&actual, &case.expected, &case.id);
    }
}

#[test]
fn deno_gzip_oracle_cases_pass() {
    let Some(manifest) = read_manifest::<GzipCase>("gzip.json") else {
        return;
    };
    assert_eq!(manifest.schema_version, 1);
    assert!(!manifest.cases.is_empty());

    for case in manifest.cases {
        let Some(root) = repo_root() else {
            eprintln!("skipping Deno oracle file cases; run scripts/fetch_upstream.py first");
            return;
        };
        let file = File::open(root.join(case.dataset).join(case.path)).unwrap();
        let actual = parse_gzip(file, case.max_bytes)
            .map(|gzip| gzip_to_json(&gzip))
            .unwrap_or(J::Null);
        assert_json_eq(&actual, &case.expected, &case.id);
    }
}

#[test]
fn deno_tiff_oracle_cases_pass() {
    let Some(manifest) = read_manifest::<TiffCase>("tiff.json") else {
        return;
    };
    assert_eq!(manifest.schema_version, 1);
    assert!(!manifest.cases.is_empty());

    for case in manifest.cases {
        let Some(root) = repo_root() else {
            eprintln!("skipping Deno oracle file cases; run scripts/fetch_upstream.py first");
            return;
        };
        let file = File::open(root.join(case.dataset).join(case.path)).unwrap();
        let (tiff, ome) = parse_tiff(file, case.parse_ome);
        let actual = json!({
            "tiff": tiff.map(|t| tiff_to_json(&t)).unwrap_or(J::Null),
            "ome": ome.map(|o| ome_to_json(&o)).unwrap_or(J::Null),
        });
        assert_json_eq(&actual, &case.expected, &case.id);
    }
}

fn read_manifest<T: for<'de> Deserialize<'de>>(name: &str) -> Option<Manifest<T>> {
    let path = oracle_cases_dir().join(name);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "skipping Deno oracle cases; run tests/oracle/export.ts to generate {}",
                path.display()
            );
            return None;
        }
        Err(err) => panic!("failed to read {}: {err}", path.display()),
    };
    Some(serde_json::from_str(&text).unwrap())
}

fn oracle_cases_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/oracle/cases")
}

fn repo_root() -> Option<PathBuf> {
    let candidate = std::env::var_os("BIDS_VALIDATOR_UPSTREAM_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/bids-validator-2.4.1")
        });
    candidate.canonicalize().ok()
}

fn assert_json_eq(actual: &J, expected: &J, id: &str) {
    assert!(
        json_eq(actual, expected),
        "{id}\nactual:   {actual}\nexpected: {expected}"
    );
}

fn json_eq(actual: &J, expected: &J) -> bool {
    match (actual, expected) {
        (J::Number(a), J::Number(e)) => a.as_f64() == e.as_f64(),
        (J::Array(a), J::Array(e)) => {
            a.len() == e.len() && a.iter().zip(e.iter()).all(|(a, e)| json_eq(a, e))
        }
        (J::Object(a), J::Object(e)) => {
            a.len() == e.len()
                && e.iter().all(|(key, expected_value)| {
                    a.get(key).is_some_and(|v| json_eq(v, expected_value))
                })
        }
        _ => actual == expected,
    }
}
