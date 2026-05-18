//! M4 regression test: parallel and `BIDS_VALIDATOR_JOBS=1`
//! validation runs must produce byte-identical issue lists.
//!
//! The dedup pass in `validate_dataset` keeps the lowest-file-index
//! observer of any `(sidecar_location, key)` tuple; chunking and
//! rayon's work-stealing preserve that order because each chunk is
//! `collect()`-ed in file-index order before sequential merge. This
//! test exists to break loudly if anyone refactors away that
//! ordering invariant.

use std::ffi::OsString;
use std::fs;
use std::sync::{Mutex, OnceLock};

use bids_core::filename::parse_filename;
use bids_core::inheritance::build_index;
use bids_core::metadata::DatasetSubjects;
use bids_core::policy::{ContentMode, ContentPolicy};
use bids_core::rules::DatasetType;
use bids_core::run::{validate_dataset, DatasetContext, DiscoveredFile};
use tempfile::TempDir;

fn build_ctx(files: &[(&str, &[u8])]) -> (TempDir, DatasetContext) {
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
        let datatype = infer_datatype(bids_path);
        let parsed = parse_filename(name);
        discovered.push(DiscoveredFile {
            bids_path: bids_path.to_string(),
            fs_path,
            datatype,
            parsed,
        });
    }

    let index = build_index(&discovered);
    let policy = ContentPolicy::new(ContentMode::Parity);
    let cache = bids_core::content::ContentCache::new(policy);
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

fn infer_datatype(bids_path: &str) -> String {
    const DATATYPES: &[&str] = &[
        "anat", "func", "fmap", "dwi", "perf", "meg", "eeg", "ieeg", "beh", "pet", "micr",
        "motion", "nirs", "mrs", "emg",
    ];
    let parent = match bids_path.rsplit_once('/') {
        Some((p, _)) => p,
        None => return String::new(),
    };
    let last = match parent.rsplit_once('/') {
        Some((_, l)) => l,
        None => parent,
    };
    if DATATYPES.contains(&last) {
        last.to_string()
    } else {
        String::new()
    }
}

struct EnvGuard {
    key: &'static str,
    old: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let old = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, old }
    }

    fn remove(key: &'static str) -> Self {
        let old = std::env::var_os(key);
        std::env::remove_var(key);
        Self { key, old }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(old) = &self.old {
            std::env::set_var(self.key, old);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// 20-subject fixture, large enough to cross `PARALLEL_FILE_THRESHOLD`
/// (256) and exercise the chunked parallel path. Each subject inherits
/// a shared root sidecar — the dedup-winner path is exercised once
/// per (origin, key) pair.
fn many_subjects_fixture() -> Vec<(&'static str, &'static [u8])> {
    // Build the fixture at compile-time via a const slice of subject
    // ids; expanded into BIDS paths below.
    static FILES: &[(&str, &[u8])] = &[
        (
            "/dataset_description.json",
            br#"{"Name": "ds", "BIDSVersion": "1.10.0"}"#,
        ),
        (
            "/task-rest_bold.json",
            br#"{"RepetitionTime": 2.0, "EchoTime": 0.03}"#,
        ),
    ];
    let mut out: Vec<(&'static str, &'static [u8])> = FILES.to_vec();
    macro_rules! subj {
        ($($s:literal),* $(,)?) => {{
            $(
                out.push((concat!("/sub-", $s, "/func/sub-", $s, "_task-rest_bold.nii.gz"), b"x"));
                out.push((concat!("/sub-", $s, "/anat/sub-", $s, "_T1w.nii.gz"), b"x"));
            )*
        }};
    }
    // 130 subjects × 2 files + 2 root files = 262 files → above
    // PARALLEL_FILE_THRESHOLD, triggers the parallel path.
    subj!(
        "001", "002", "003", "004", "005", "006", "007", "008", "009", "010", "011", "012", "013",
        "014", "015", "016", "017", "018", "019", "020", "021", "022", "023", "024", "025", "026",
        "027", "028", "029", "030", "031", "032", "033", "034", "035", "036", "037", "038", "039",
        "040", "041", "042", "043", "044", "045", "046", "047", "048", "049", "050", "051", "052",
        "053", "054", "055", "056", "057", "058", "059", "060", "061", "062", "063", "064", "065",
        "066", "067", "068", "069", "070", "071", "072", "073", "074", "075", "076", "077", "078",
        "079", "080", "081", "082", "083", "084", "085", "086", "087", "088", "089", "090", "091",
        "092", "093", "094", "095", "096", "097", "098", "099", "100", "101", "102", "103", "104",
        "105", "106", "107", "108", "109", "110", "111", "112", "113", "114", "115", "116", "117",
        "118", "119", "120", "121", "122", "123", "124", "125", "126", "127", "128", "129", "130",
    );
    out
}

#[test]
fn parallel_and_sequential_produce_byte_identical_issue_json() {
    let _guard = env_lock().lock().unwrap();
    // Force sequential first via `BIDS_VALIDATOR_JOBS=1`, then default
    // (parallel). Issue JSON must be byte-identical.
    let files = many_subjects_fixture();

    // Sequential run.
    let seq_env = EnvGuard::set("BIDS_VALIDATOR_JOBS", "1");
    let (_tmp_seq, ctx_seq) = build_ctx(&files);
    assert!(ctx_seq.files.len() >= 256, "fixture must cross threshold");
    let seq = validate_dataset(&ctx_seq);
    let seq_json = serde_json::to_string(&seq.issues).unwrap();
    drop(seq_env);

    // Parallel run (default jobs).
    let _par_env = EnvGuard::remove("BIDS_VALIDATOR_JOBS");
    let (_tmp_par, ctx_par) = build_ctx(&files);
    let par = validate_dataset(&ctx_par);
    let par_json = serde_json::to_string(&par.issues).unwrap();

    assert_eq!(
        seq_json, par_json,
        "parallel JSON diverged from sequential — ordering or dedup invariant broken"
    );
}
