//! Shared fixture builder for bids-core integration tests.
//!
//! Builds a `DatasetContext` from an in-memory list of `(bids_path, bytes)`
//! pairs written to a temp dir, mirroring what the CLI walker produces.
//! New tests should use this rather than copying the builder again (the
//! older integration tests predate it and still carry local copies).

use std::fs;

use bids_core::content::ContentCache;
use bids_core::filename::parse_filename;
use bids_core::inheritance::build_index;
use bids_core::metadata::DatasetSubjects;
use bids_core::policy::{ContentMode, ContentPolicy};
use bids_core::rules::DatasetType;
use bids_core::run::{DatasetContext, DiscoveredFile};
use tempfile::TempDir;

const DATATYPES: &[&str] = &[
    "anat", "func", "fmap", "dwi", "perf", "meg", "eeg", "ieeg", "beh", "pet", "micr", "motion",
    "nirs", "mrs", "emg",
];

/// Datatype = the directory immediately containing the file, when it's a
/// known BIDS datatype; otherwise empty.
pub fn infer_datatype(bids_path: &str) -> String {
    let parent = bids_path.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
    let last = parent.rsplit_once('/').map(|(_, l)| l).unwrap_or(parent);
    if DATATYPES.contains(&last) {
        last.to_string()
    } else {
        String::new()
    }
}

/// Minimal uncompressed NIfTI-1 with caller-chosen `dim` and `pixdim`
/// (8 entries each: index 0 is ndims / qfac, `1..` are extents / voxel
/// sizes). Only the header fields BIDS validation reads are set. Shared
/// by the crafted-image fixtures (dwi/fmap/nifti checks).
pub fn nifti(dim: [i16; 8], pixdim: [f32; 8]) -> Vec<u8> {
    let mut h = vec![0u8; 360];
    h[0..4].copy_from_slice(&348i32.to_le_bytes());
    for (i, d) in dim.iter().enumerate() {
        h[40 + i * 2..42 + i * 2].copy_from_slice(&d.to_le_bytes());
    }
    h[70..72].copy_from_slice(&2i16.to_le_bytes()); // datatype = uint8
    h[72..74].copy_from_slice(&8i16.to_le_bytes()); // bitpix
    for (i, p) in pixdim.iter().enumerate() {
        h[76 + i * 4..80 + i * 4].copy_from_slice(&p.to_le_bytes());
    }
    h[108..112].copy_from_slice(&352.0f32.to_le_bytes()); // vox_offset
    h[112..116].copy_from_slice(&1.0f32.to_le_bytes()); // scl_slope
    h[123] = 10; // xyzt_units: mm + sec
    h[252..254].copy_from_slice(&1i16.to_le_bytes()); // qform_code
    h[254..256].copy_from_slice(&1i16.to_le_bytes()); // sform_code
    h[344..348].copy_from_slice(b"n+1\0");
    h
}

/// Build a parity-mode `DatasetContext` from `(bids_path, bytes)` pairs.
/// Returns the `TempDir` guard too — keep it alive for the context's
/// lifetime (its `Drop` removes the files on disk).
///
/// NOTE: this is a narrow fixture builder, not a full discovery clone.
/// `dataset_modalities` and `dataset_subjects` are left EMPTY (the real
/// CLI walker populates them in `src/discover.rs`). Tests that exercise
/// rules depending on `dataset.modalities` or subject summaries must set
/// those fields explicitly rather than relying on this helper.
pub fn build_ctx(files: &[(&str, &[u8])]) -> (TempDir, DatasetContext) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let mut discovered = Vec::new();
    for (bids_path, body) in files {
        let rel = bids_path.trim_start_matches('/');
        let fs_path = root.join(rel);
        fs::create_dir_all(fs_path.parent().unwrap()).unwrap();
        fs::write(&fs_path, body).unwrap();
        let name = bids_path
            .rsplit_once('/')
            .map(|(_, n)| n)
            .unwrap_or(bids_path);
        discovered.push(DiscoveredFile {
            bids_path: bids_path.to_string(),
            fs_path,
            datatype: infer_datatype(bids_path),
            parsed: parse_filename(name),
        });
    }
    let index = build_index(&discovered);
    let cache = ContentCache::new(ContentPolicy::new(ContentMode::Parity));
    let ctx = DatasetContext {
        root,
        files: discovered,
        index,
        dataset_type: DatasetType::Raw,
        dataset_modalities: Vec::new(),
        dataset_subjects: DatasetSubjects {
            sub_dirs: Vec::new(),
            participant_id: None,
            phenotype: None,
        },
        ignore_nifti_headers: false,
        cache,
    };
    (tmp, ctx)
}

/// Sorted locations of every issue with the given code.
pub fn issue_locations(issues: &[bids_core::issue::Issue], code: &str) -> Vec<String> {
    let mut out: Vec<String> = issues
        .iter()
        .filter(|i| i.code == code)
        .map(|i| i.location.clone())
        .collect();
    out.sort();
    out
}
