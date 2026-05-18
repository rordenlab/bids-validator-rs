//! Validation orchestration.
//!
//! Owns the per-file validation loop that previously lived inline in
//! `bids-cli/src/main.rs`. The CLI is responsible for argument parsing
//! and dataset discovery (walking the filesystem); after that it hands
//! a [`DatasetContext`] to [`validate_dataset`] and emits whatever
//! that returns.
//!
//! This split makes the per-file logic testable without going through
//! a CLI invocation, and gives non-CLI callers (Python wheels, WASM
//! bindings, etc.) a clean entry point.

use std::collections::HashSet;
use std::path::PathBuf;

use serde_json::Value as J;

use crate::associations::build_associations;
use crate::checks::check_rules;
use crate::filename::parse_filename;
use crate::gzip::{gzip_to_json, parse_gzip};
use crate::inheritance::{
    merge_with_origins_and_override_issues, read_sidecars_with_candidates, DirIndex,
};
use crate::issue::{Issue, IssueList, Severity, ValidationResult};
use crate::metadata::{
    build_eval_context, check_json, check_sidecars, set_eval_context_associations, DatasetSubjects,
    SidecarContext, SidecarKeyValidated,
};
use crate::nifti::{header_to_json, read_header};
use crate::rules::{validate_file, DatasetType, FileContext};
use crate::tables::check_tables;
use crate::tiff::{ome_to_json, parse_tiff, tiff_to_json};
use crate::tsv::{load_tsv, load_tsvgz, TsvColumns};
use bids_schema_codegen::modality_of;

/// A file discovered by the dataset walker. Produced by the CLI (or
/// any other front-end) and handed to [`DatasetContext`] for
/// validation.
#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    /// Dataset-relative POSIX path with leading `/`.
    pub bids_path: String,
    /// Absolute filesystem path. May be a broken symlink.
    pub fs_path: PathBuf,
    /// Inferred datatype (e.g. `"anat"`); empty string if not one of
    /// the recognized BIDS datatypes.
    pub datatype: String,
    /// Parsed filename fields (entities, suffix, extension).
    /// Computed once at discovery time; shared with the inheritance
    /// walker and the per-file validator via the canonical arena.
    pub parsed: crate::filename::ParsedFilename,
}

/// All inputs the per-file validator needs. Constructed by the
/// front-end (CLI today; library callers later) after dataset
/// discovery completes. Consumed by [`validate_dataset`].
pub struct DatasetContext {
    pub root: PathBuf,
    pub files: Vec<DiscoveredFile>,
    pub index: DirIndex,
    pub dataset_type: DatasetType,
    /// Pretty modality names (`"MRI"`, `"PET"`, …) as exposed via
    /// `dataset.modalities` in the selector eval context. Computed by
    /// the front-end during discovery.
    pub dataset_modalities: Vec<String>,
    pub dataset_subjects: DatasetSubjects,
    pub ignore_nifti_headers: bool,
    /// Per-run cache for repeated small reads (sidecar text and other
    /// small text payloads). Constructed in the front-end and owned by the
    /// context for the duration of one `validate_dataset` call.
    pub cache: crate::content::ContentCache,
}

/// Run validation across every file in `ctx`. Emits filename-level,
/// sidecar-level, JSON-schema-level, `rules.checks.*`, and TSV-table
/// issues. Stable across re-invocations on the same `ctx` (issue
/// ordering depends only on the input file order).
///
/// This is the per-file Pass 2 of the previous monolithic CLI; Pass 1
/// (filesystem walking, modality summary, etc.) is the caller's
/// responsibility because it touches CLI-specific concerns
/// (`walkdir`, `.bidsignore`, etc.).
pub fn validate_dataset(ctx: &DatasetContext) -> ValidationResult {
    // Build the dataset's file path set from the canonical arena for
    // `bids_expr::exists(...)` selectors. Keeping this derivation here
    // prevents library callers from accidentally validating with an
    // empty `exists()` universe.
    let dataset_files =
        bids_expr::exists::normalize_dataset_files(ctx.files.iter().map(|f| f.bids_path.clone()));

    let mut result = ValidationResult {
        issues: IssueList::default(),
    };
    emit_missing_dataset_description_issue(ctx, &mut result.issues.issues);

    // Per-file work is independent given a stable arena + cache. We
    // process files in fixed-size chunks: each chunk is rayon-parallel
    // internally, then merged sequentially into the dataset-wide state.
    // Chunking bounds peak RSS — without it, materializing a
    // `Vec<PerFileBundle>` for all files on `ds005016` added ~15 MiB
    // beyond M3 baseline. Chunk size 1024 keeps the bundles slice
    // under ~1 MiB while preserving full parallelism within a chunk
    // (rayon work-stealing inside `par_iter::map`).
    //
    // Determinism: bundles are collected in file-index order within a
    // chunk; chunks are processed sequentially. So the merge sees
    // (chunk_idx_0, file_idx_0), (chunk_idx_0, file_idx_1), …,
    // (chunk_idx_1, file_idx_0), … — i.e., overall file-index order.
    // The dedup-winner contract ("lowest file index wins") is
    // preserved.
    //
    // `BIDS_VALIDATOR_JOBS=1` forces sequential — used by the M4 byte-
    // equality regression test and as an escape hatch for debugging.
    let mut validated = SidecarKeyValidated::new();
    let mut viewed_sidecars: HashSet<PathBuf> = HashSet::new();
    let merge_bundle = |bundle: PerFileBundle,
                        issues: &mut Vec<Issue>,
                        validated: &mut SidecarKeyValidated,
                        viewed_sidecars: &mut HashSet<PathBuf>| {
        issues.extend(bundle.issues);
        for candidate in bundle.dedup_candidates {
            if !validated.check_and_record(&candidate.key.0, &candidate.key.1) {
                issues.extend(candidate.issues);
            }
        }
        viewed_sidecars.extend(bundle.viewed_sidecars);
    };

    if jobs_env() == Some(1) || ctx.files.len() < PARALLEL_FILE_THRESHOLD {
        // Sequential streaming: no bundles vector materialized, no
        // rayon thread pool. Used when the user forces it via
        // `BIDS_VALIDATOR_JOBS=1` and on small datasets where the
        // thread-pool startup cost dominates.
        for file in &ctx.files {
            let bundle = validate_one_file(ctx, file, &dataset_files);
            merge_bundle(
                bundle,
                &mut result.issues.issues,
                &mut validated,
                &mut viewed_sidecars,
            );
        }
    } else {
        use rayon::iter::{IntoParallelIterator, ParallelIterator};
        const CHUNK_SIZE: usize = 1024;
        let pool = configured_rayon_pool();
        for chunk in ctx.files.chunks(CHUNK_SIZE) {
            let bundles: Vec<PerFileBundle> = match &pool {
                Some(p) => p.install(|| {
                    chunk
                        .into_par_iter()
                        .map(|f| validate_one_file(ctx, f, &dataset_files))
                        .collect()
                }),
                None => chunk
                    .iter()
                    .map(|f| validate_one_file(ctx, f, &dataset_files))
                    .collect(),
            };
            for bundle in bundles {
                merge_bundle(
                    bundle,
                    &mut result.issues.issues,
                    &mut validated,
                    &mut viewed_sidecars,
                );
            }
        }
    }

    emit_orphan_sidecar_issues(ctx, &viewed_sidecars, &mut result.issues.issues);

    result
}

/// Datasets smaller than this go fully sequential, skipping rayon
/// entirely. The thread pool startup overhead alone is ~5–8 MiB on
/// macOS, which dwarfs the absolute time saved on tiny trees.
const PARALLEL_FILE_THRESHOLD: usize = 256;

/// Default cap on rayon worker threads. Picked empirically against
/// `ds005016` to stay under the M4 +15% RSS gate; rayon's default
/// (`num_cpus`) pushed peak RSS ~30% over baseline on macOS M-series.
const DEFAULT_RAYON_THREADS: usize = 4;

/// Read `BIDS_VALIDATOR_JOBS` if set to a positive integer; `None`
/// otherwise. `0` and unparseable values fall back to the
/// `DEFAULT_RAYON_THREADS` cap.
fn jobs_env() -> Option<usize> {
    std::env::var("BIDS_VALIDATOR_JOBS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| n > 0)
}

/// Build a dedicated `rayon::ThreadPool`. Uses `BIDS_VALIDATOR_JOBS`
/// when set, otherwise `DEFAULT_RAYON_THREADS`. Always a scoped pool
/// (not the global one) so concurrent validation runs in a single
/// process don't interfere.
fn configured_rayon_pool() -> Option<rayon::ThreadPool> {
    let n = jobs_env().unwrap_or(DEFAULT_RAYON_THREADS);
    rayon::ThreadPoolBuilder::new().num_threads(n).build().ok()
}

/// What one file's validator returns. Built in parallel; consumed
/// sequentially by the merge step.
struct PerFileBundle {
    /// Issues that fire regardless of dedup (filename checks,
    /// JSON_INVALID, override issues, missing-key emissions, etc.).
    issues: Vec<Issue>,
    /// Schema-validation candidates tagged by `(sidecar_location, key)`.
    /// Merge keeps the first observer in file-index order.
    dedup_candidates: Vec<crate::metadata::DedupCandidate>,
    /// Sidecar paths this file's inheritance walk picked up; unioned
    /// across files for the orphan-sidecar pass.
    viewed_sidecars: HashSet<PathBuf>,
}

fn emit_missing_dataset_description_issue(ctx: &DatasetContext, issues: &mut Vec<Issue>) {
    if ctx
        .files
        .iter()
        .any(|f| f.bids_path == "/dataset_description.json")
    {
        return;
    }
    issues.push(Issue {
        code: "MISSING_DATASET_DESCRIPTION".to_string(),
        sub_code: None,
        severity: Severity::Error,
        location: String::new(),
        rule: None,
        issue_message: None,
        affects: Some(vec!["/dataset_description.json".to_string()]),
        line: None,
        code_message: None,
    });
}

/// Walks the dataset's `.json` files and emits SIDECAR_WITHOUT_DATAFILE
/// for any whose path wasn't picked up by some other file's
/// inheritance walk. Mirrors `sidecarWithoutDatafile` in
/// `src/validators/internal/unusedFile.ts`.
///
/// Exemptions:
/// * `dataset_description.json` and `genetic_info.json` — schema-level
///   "standalone" JSONs that exist without a data file by design.
/// * `suffix == "coordsystem" && modality == "emg"` — matches the
///   schema selector `suffix != "coordsystem" || modality != "emg"`
///   on `rules.errors.SidecarWithoutDatafile`.
///
/// Files under opaque directories (`sourcedata/`, `derivatives/`,
/// `code/`, `stimuli/`, `log/`) don't show up here because they're
/// pruned by the CLI walker before discovery; no extra filter needed.
fn emit_orphan_sidecar_issues(
    ctx: &DatasetContext,
    viewed: &HashSet<PathBuf>,
    issues: &mut Vec<Issue>,
) {
    const STANDALONE: &[&str] = &["/dataset_description.json", "/genetic_info.json"];

    for file in &ctx.files {
        if !file.bids_path.ends_with(".json") {
            continue;
        }
        if STANDALONE.contains(&file.bids_path.as_str()) {
            continue;
        }
        let name = file
            .bids_path
            .rsplit_once('/')
            .map(|(_, n)| n)
            .unwrap_or(&file.bids_path);
        let parsed = parse_filename(name);
        // EMG coordsystem exemption — per schema selector.
        if parsed.suffix == "coordsystem" && modality_of(&file.datatype) == Some("emg") {
            continue;
        }
        if viewed.contains(&file.fs_path) {
            continue;
        }
        issues.push(Issue {
            code: "SIDECAR_WITHOUT_DATAFILE".to_string(),
            sub_code: None,
            severity: Severity::Error,
            location: file.bids_path.clone(),
            rule: None,
            issue_message: None,
            affects: None,
            line: None,
            code_message: None,
        });
    }
}

fn validate_one_file(
    ctx: &DatasetContext,
    file: &DiscoveredFile,
    dataset_files: &HashSet<String>,
) -> PerFileBundle {
    let mut issues: Vec<Issue> = Vec::new();
    let mut viewed_sidecars: HashSet<PathBuf> = HashSet::new();
    let mut dedup_candidates: Vec<crate::metadata::DedupCandidate> = Vec::new();

    let bids_path = &file.bids_path;
    let fs_path = file.fs_path.as_path();
    let datatype = file.datatype.as_str();
    // `parsed` is now precomputed on the arena entry — no per-file
    // re-parse, and the linear `find(bids_path == ..)` lookup that used
    // to follow has been replaced by direct arena access.
    let parsed = &file.parsed;

    // Filename-level checks.
    let file_ctx = FileContext {
        path: bids_path,
        datatype,
        parsed,
        dataset_type: ctx.dataset_type,
    };
    issues.extend(validate_file(&file_ctx));

    // Sidecar metadata: walk the inheritance chain, merge, evaluate.
    let parent = bids_path
        .rsplit_once('/')
        .map(|(p, _)| if p.is_empty() { "/" } else { p })
        .unwrap_or("/");
    emit_case_collision_issue(
        bids_path,
        file,
        &ctx.files,
        ctx.index.get(parent),
        &mut issues,
    );

    let sidecar_read = read_sidecars_with_candidates(&ctx.files, file, &ctx.index, &ctx.cache);
    issues.extend(sidecar_read.issues);
    // Mark sidecars this file's inheritance walk picked up as "viewed"
    // for the SIDECAR_WITHOUT_DATAFILE pass below (mirrors Deno's
    // `file.viewed = true` side-effect in `walkBack`). Mark selected
    // candidates, not only successfully parsed JSON, so invalid or
    // policy-skipped sidecars are not also reported as orphaned. Skip self —
    // `sidecar_candidates_with_issues` returns the source file itself
    // as a candidate when source is `.json` with the same suffix
    // (it's its own subset). Including it here would make every
    // sidecar mark itself viewed, defeating the orphan detection.
    for fs_path in &sidecar_read.candidates {
        if fs_path == &file.fs_path {
            continue;
        }
        viewed_sidecars.insert(fs_path.clone());
    }
    // read_sidecars returns closest-first. Keep that order so the
    // merge helper can both preserve closest-wins semantics and mirror
    // Deno's SIDECAR_FIELD_OVERRIDE warnings.
    let inputs: Vec<(String, _)> = sidecar_read
        .loaded
        .into_iter()
        .map(|(fs, j)| {
            let bp = fs
                .strip_prefix(&ctx.root)
                .map(|p| format!("/{}", p.to_string_lossy().replace('\\', "/")))
                .unwrap_or_else(|_| fs.to_string_lossy().into_owned());
            (bp, j)
        })
        .collect();
    let (merged, origin, override_issues) = merge_with_origins_and_override_issues(inputs);
    if file.parsed.extension != ".json" {
        issues.extend(override_issues);
    }

    // .json file? Parse own contents for rules.json / rules.dataset_metadata.
    let mut file_json = J::Null;
    if file.parsed.extension == ".json" {
        // Mirrors `src/files/json.ts::loadJSON`: parse failure →
        // JSON_INVALID; non-object body → JSON_NOT_AN_OBJECT. Read
        // errors and policy-skipped reads are silent (Deno's catch-arm).
        if let Some(text) = ctx.cache.read_to_string(&file.fs_path) {
            match serde_json::from_str::<J>(&text) {
                Ok(v) => {
                    if !v.is_object() {
                        issues.push(Issue {
                            code: "JSON_NOT_AN_OBJECT".to_string(),
                            sub_code: None,
                            severity: Severity::Error,
                            location: bids_path.clone(),
                            rule: None,
                            issue_message: None,
                            affects: None,
                            line: None,
                            code_message: None,
                        });
                    }
                    file_json = v;
                }
                Err(e) => issues.push(Issue {
                    code: "JSON_INVALID".to_string(),
                    sub_code: None,
                    severity: Severity::Error,
                    location: bids_path.clone(),
                    rule: None,
                    issue_message: Some(e.to_string()),
                    affects: None,
                    line: None,
                    code_message: None,
                }),
            }
        }
    }
    // Mirror Deno's `BIDSContextDataset.set dataset_description` setter:
    // /dataset_description.json gets a DatasetType default fill so
    // selectors reading `context.json.DatasetType` (memoized via
    // loadJSON in Deno) see a value.
    if bids_path == "/dataset_description.json" {
        if let J::Object(map) = &mut file_json {
            if !map.contains_key("DatasetType") {
                let val = if map.contains_key("GeneratedBy") {
                    "derivative"
                } else {
                    "raw"
                };
                map.insert("DatasetType".into(), J::String(val.into()));
            }
        }
    }

    // Content-derived eval context: NIfTI header, TIFF/OME, gzip,
    // TSV columns, file size. All gated through the content policy.
    let policy = ctx.cache.policy();
    let nifti_header_json = if !ctx.ignore_nifti_headers
        && (parsed.extension == ".nii" || parsed.extension == ".nii.gz")
    {
        policy
            .open(fs_path)
            .and_then(read_header)
            .map(|h| header_to_json(&h))
            .unwrap_or(J::Null)
    } else {
        J::Null
    };
    let (tiff_json, ome_json) =
        if parsed.extension.ends_with(".tif") || parsed.extension.ends_with(".btf") {
            policy
                .open(fs_path)
                .map(|reader| parse_tiff(reader, parsed.extension.starts_with(".ome")))
                .map(|(tiff, ome)| {
                    (
                        tiff.map(|t| tiff_to_json(&t)).unwrap_or(J::Null),
                        ome.map(|o| ome_to_json(&o)).unwrap_or(J::Null),
                    )
                })
                .unwrap_or((J::Null, J::Null))
        } else {
            (J::Null, J::Null)
        };
    let gzip_json = if parsed.extension.ends_with(".gz") {
        policy
            .open(fs_path)
            .and_then(|reader| parse_gzip(reader, 1024))
            .map(|g| gzip_to_json(&g))
            .unwrap_or(J::Null)
    } else {
        J::Null
    };
    // `cache.metadata_size()` wraps the cache policy's metadata(), which skips
    // stat on annex symlinks in parity mode, mirroring the broken-link
    // `stat` failure Deno hits.
    let file_size_opt = ctx.cache.metadata_size(fs_path);
    let file_size = file_size_opt.unwrap_or(0);
    if file_size_opt == Some(0) {
        issues.push(Issue {
            code: "EMPTY_FILE".to_string(),
            sub_code: None,
            severity: Severity::Error,
            location: bids_path.clone(),
            rule: None,
            issue_message: None,
            affects: None,
            line: None,
            code_message: None,
        });
    }

    let (columns, parse_issue) = if parsed.extension == ".tsv" {
        load_tsv(fs_path, &policy)
    } else if parsed.extension == ".tsv.gz" {
        let headers = merged
            .get("Columns")
            .and_then(|v| v.as_array())
            .map(|xs| {
                xs.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if headers.is_empty() || file_size == 0 {
            (TsvColumns::default(), None)
        } else {
            load_tsvgz(fs_path, &headers, &policy)
        }
    } else {
        (TsvColumns::default(), None)
    };
    if let Some(mut issue) = parse_issue {
        issue.location = bids_path.clone();
        issues.push(issue);
    }

    // Associations need a seed context (with empty associations data),
    // then the final context replaces that field.
    let empty_associations = J::Object(serde_json::Map::new());
    let assoc_seed = SidecarContext {
        bids_path,
        datatype,
        parsed,
        merged_sidecar: &merged,
        file_json: &file_json,
        dataset_type: ctx.dataset_type,
        dataset_modalities: &ctx.dataset_modalities,
        sidecar_key_origin: &origin,
        nifti_header: &nifti_header_json,
        gzip: &gzip_json,
        columns: &columns,
        associations: &empty_associations,
        ome: &ome_json,
        tiff: &tiff_json,
        file_size,
        dataset_subjects: &ctx.dataset_subjects,
        dataset_files,
    };
    let mut ctx_value = build_eval_context(&assoc_seed);
    let (associations, assoc_issues) = build_associations(
        &ctx_value,
        &ctx.files,
        file,
        &ctx.index,
        &ctx.cache,
        dataset_files,
    );
    issues.extend(assoc_issues);

    let sctx = SidecarContext {
        associations: &associations,
        ..assoc_seed
    };
    // Build the selector evaluation context once with empty
    // associations, use it for association selectors, then patch in the
    // resolved associations for the remaining check passes.
    set_eval_context_associations(&mut ctx_value, &associations);
    let sidecar_result = check_sidecars(&sctx, &ctx_value);
    issues.extend(sidecar_result.plain);
    dedup_candidates.extend(sidecar_result.dedup);
    issues.extend(check_json(&sctx, &ctx_value));
    issues.extend(check_rules(&sctx, &ctx_value));

    if parsed.extension == ".tsv" || parsed.extension == ".tsv.gz" {
        issues.extend(check_tables(&sctx, &ctx_value, &columns));
    }

    PerFileBundle {
        issues,
        dedup_candidates,
        viewed_sidecars,
    }
}

fn emit_case_collision_issue(
    bids_path: &str,
    source: &DiscoveredFile,
    arena: &[DiscoveredFile],
    siblings: Option<&Vec<usize>>,
    issues: &mut Vec<Issue>,
) {
    let Some(siblings) = siblings else {
        return;
    };
    let source_name = source
        .bids_path
        .rsplit_once('/')
        .map(|(_, n)| n)
        .unwrap_or(&source.bids_path);
    let lowercase = source_name.to_lowercase();
    let mut affects = siblings
        .iter()
        .map(|&i| &arena[i])
        .filter(|other| other.bids_path != source.bids_path)
        .filter_map(|other| {
            let name = other
                .bids_path
                .rsplit_once('/')
                .map(|(_, n)| n)
                .unwrap_or(&other.bids_path);
            (name.to_lowercase() == lowercase).then(|| other.bids_path.clone())
        })
        .collect::<Vec<_>>();
    if affects.is_empty() {
        return;
    }
    affects.sort();
    issues.push(Issue {
        code: "CASE_COLLISION".to_string(),
        sub_code: None,
        severity: Severity::Error,
        location: bids_path.to_string(),
        rule: None,
        issue_message: None,
        affects: Some(affects),
        line: None,
        code_message: None,
    });
}
