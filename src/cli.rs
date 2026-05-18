//! CLI surface: flag parsing, version string, mode enums, and the small
//! helpers that translate raw clap output into accept/reject decisions.

use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::{bail, Result};
use clap::{ArgAction, Parser, ValueEnum};

pub(crate) fn version_string() -> &'static str {
    static CELL: OnceLock<String> = OnceLock::new();
    CELL.get_or_init(|| {
        let s = bids_schema_codegen::schema();
        let upstream_version = option_env!("BIDS_VALIDATOR_UPSTREAM_VERSION").unwrap_or("2.4.1");
        let upstream_commit_full = option_env!("BIDS_VALIDATOR_UPSTREAM_COMMIT")
            .unwrap_or("94ea9719efbbac7531fed8ad8a2a7bf764cee51f");
        let upstream_commit = upstream_commit_full
            .get(..7)
            .unwrap_or(upstream_commit_full);
        let rust_commit = option_env!("BIDS_VALIDATOR_GIT_COMMIT").unwrap_or("unknown");
        format!(
            "{upstream_version} commit {upstream_commit} rust {pkg}\n\
             rust git commit {rust_commit}\n\
             beta label     rust-beta parity-slice\n\
             schema source  bundled\n\
             @bids/schema   {schema}\n\
             bids version   {bids}\n\
             content mode   parity (default), thorough\n\
             link mode      parity (default), follow, no-follow\n\
             nifti headers  enabled by default; --ignore-nifti-headers available",
            pkg = env!("CARGO_PKG_VERSION"),
            schema = s.schema_version,
            bids = s.bids_version,
        )
    })
}

#[derive(Parser, Debug)]
#[command(
    name = "bids-validator",
    version = version_string(),
    about = "Rust port of the BIDS validator (parity slice)."
)]
pub(crate) struct Cli {
    /// Path to the dataset to validate.
    pub(crate) dataset: PathBuf,
    /// Deprecated Deno-compatible alias for `--format json`.
    #[arg(long, default_value_t = false)]
    pub(crate) json: bool,
    /// Output format. Rust beta supports `json` and `json_pp`; `text`
    /// is parsed but rejected with an explicit message.
    #[arg(long, value_enum)]
    pub(crate) format: Option<OutputFormat>,
    /// Specify a schema version/path/URL. Local schema files are loaded
    /// when they are structurally identical to the bundled schema.
    /// Tags matching the bundled schema version are accepted as the
    /// bundled schema. URL schemas are parsed but rejected for beta.
    #[arg(short = 's', long)]
    pub(crate) schema: Option<String>,
    /// Path to a Deno-style severity override config. Parsed but
    /// rejected until config filtering is ported.
    #[arg(short = 'c', long)]
    pub(crate) config: Option<PathBuf>,
    /// Maximum TSV rows. Parsed but rejected because the Rust beta uses
    /// bounded full-file TSV reads today.
    #[arg(long = "max-rows")]
    pub(crate) max_rows: Option<i64>,
    /// Accepted for JSON-mode compatibility. Text output is not part of
    /// the beta surface yet, so this is currently a no-op.
    #[arg(short = 'v', long, default_value_t = false)]
    pub(crate) verbose: bool,
    /// Drop warning-severity issues from JSON output and exit status.
    #[arg(long = "ignoreWarnings", default_value_t = false)]
    pub(crate) ignore_warnings: bool,
    /// Filename-only stdin mode. Parsed but rejected until the stdin
    /// frontend is implemented.
    #[arg(long = "filenameMode", default_value_t = false)]
    pub(crate) filename_mode: bool,
    /// Permitted dataset types. Accepts repeated or comma-separated
    /// values (`raw`, `derivative`, `study`).
    #[arg(long = "datasetTypes", action = ArgAction::Append, value_delimiter = ',')]
    pub(crate) dataset_types: Vec<String>,
    /// Modalities to reject if detected. Accepts repeated or
    /// comma-separated values (`MRI`, `PET`, `MEG`, etc.; case-insensitive).
    #[arg(long = "blacklistModalities", action = ArgAction::Append, value_delimiter = ',')]
    pub(crate) blacklist_modalities: Vec<String>,
    /// Validate BIDS derivatives found under `<dataset>/derivatives/*`
    /// that contain their own dataset_description.json.
    #[arg(short = 'r', long, default_value_t = false)]
    pub(crate) recursive: bool,
    /// Prune derivatives/sourcedata/code during loading. Root pruning is
    /// the Rust default; when combined with `--recursive`, this disables
    /// derivative validation.
    #[arg(short = 'p', long, default_value_t = false)]
    pub(crate) prune: bool,
    /// Write formatted output to a file instead of stdout.
    #[arg(short = 'o', long)]
    pub(crate) outfile: Option<PathBuf>,
    /// Parsed but rejected until git-annex remote content access is
    /// implemented for the Rust CLI.
    #[arg(long = "preferredRemote")]
    pub(crate) preferred_remote: Option<String>,
    /// Validate files from a git tree. Parsed but rejected until the
    /// Rust git frontend exists. Bare `--git-ref` maps to `HEAD`.
    #[arg(long = "git-ref", num_args = 0..=1, default_missing_value = "HEAD")]
    pub(crate) git_ref: Option<String>,
    /// Accepted for Deno CLI compatibility. JSON output has no color.
    #[arg(long, default_value_t = false)]
    pub(crate) color: bool,
    /// Accepted for Deno CLI compatibility. JSON output has no color.
    #[arg(long = "no-color", default_value_t = false)]
    pub(crate) no_color: bool,
    /// Accepted for Deno CLI compatibility. The Rust beta does not yet
    /// emit debug logs.
    #[arg(long, default_value = "ERROR")]
    pub(crate) debug: String,
    /// Controls whether content-derived checks mimic Deno read failures on git-annex symlinks
    /// or read local annex targets when they are present.
    #[arg(long, value_enum, default_value_t = ContentMode::Parity)]
    pub(crate) content_mode: ContentMode,
    /// Controls whether dataset discovery follows symlinked directories.
    ///
    /// `parity` keeps the current Deno-compatible behavior: symlinked
    /// directories are registered as entries but not traversed. `follow`
    /// traverses symlinked directories for stricter local checks.
    /// `no-follow` skips symlinked directories entirely while still
    /// registering regular file symlinks, including broken symlinks, for
    /// filename validation.
    #[arg(long, value_enum, default_value_t = LinkMode::Parity)]
    pub(crate) link_mode: LinkMode,
    /// Skip NIfTI header parsing. The `nifti_header` context is set to
    /// `null` for every file; `rules.checks` selectors that gate on
    /// `nifti_header != null` therefore evaluate falsy and no NIfTI-
    /// dependent issues fire. Mirrors Deno's `--ignoreNiftiHeaders`.
    /// Useful when comparing against a Deno run that had the same flag,
    /// or for fast filename-only sweeps.
    #[arg(long, alias = "ignoreNiftiHeaders", default_value_t = false)]
    pub(crate) ignore_nifti_headers: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum OutputFormat {
    Text,
    Json,
    #[value(name = "json_pp")]
    JsonPp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum ContentMode {
    /// Match Deno behavior for git-annex symlink content where possible.
    Parity,
    /// Read local symlink/annex targets when the bytes are available.
    Thorough,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum LinkMode {
    /// Match Deno-compatible discovery by not traversing symlinked directories.
    Parity,
    /// Traverse symlinked directories during discovery.
    Follow,
    /// Do not traverse symlinked directories during discovery.
    NoFollow,
}

impl LinkMode {
    pub(crate) fn follows_dirs(self) -> bool {
        matches!(self, LinkMode::Follow)
    }

    pub(crate) fn registers_symlinked_dirs(self) -> bool {
        matches!(self, LinkMode::Parity | LinkMode::Follow)
    }
}

pub(crate) fn resolve_output_format(cli: &Cli) -> Result<OutputFormat> {
    let format = if cli.json {
        OutputFormat::Json
    } else {
        cli.format.unwrap_or(OutputFormat::Json)
    };
    if format == OutputFormat::Text {
        bail!("--format text is not supported by the Rust beta; use --json or --format json");
    }
    Ok(format)
}

pub(crate) fn reject_unsupported_options(cli: &Cli) -> Result<()> {
    if cli.config.is_some() {
        bail!("--config is not implemented in the Rust beta yet; run without it or use the Deno validator");
    }
    if cli.max_rows.is_some() {
        bail!("--max-rows is not implemented in the Rust beta yet; TSV reads are bounded but all rows are validated");
    }
    if cli.filename_mode {
        bail!("--filenameMode is not implemented in the Rust beta yet");
    }
    if cli.preferred_remote.is_some() {
        bail!("--preferredRemote requires the Deno/git-annex frontend and is not implemented in the Rust beta yet");
    }
    if cli.git_ref.is_some() {
        bail!(
            "--git-ref requires the Deno/git frontend and is not implemented in the Rust beta yet"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bids_schema_codegen::load_schema_context;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    #[test]
    fn parses_link_mode_cli() {
        let default = Cli::try_parse_from(["bids-validator", "/tmp/ds"]).unwrap();
        assert_eq!(default.link_mode, LinkMode::Parity);

        let no_follow =
            Cli::try_parse_from(["bids-validator", "--link-mode", "no-follow", "/tmp/ds"]).unwrap();
        assert_eq!(no_follow.link_mode, LinkMode::NoFollow);

        let follow =
            Cli::try_parse_from(["bids-validator", "--link-mode", "follow", "/tmp/ds"]).unwrap();
        assert_eq!(follow.link_mode, LinkMode::Follow);
    }

    #[test]
    fn parses_deno_cli_compatibility_flags() {
        let cli = Cli::try_parse_from([
            "bids-validator",
            "--json",
            "--format",
            "json_pp",
            "--ignoreWarnings",
            "--ignoreNiftiHeaders",
            "--datasetTypes",
            "raw,derivative",
            "--blacklistModalities",
            "MRI,PET",
            "--recursive",
            "/tmp/ds",
        ])
        .unwrap();

        assert!(cli.json);
        assert_eq!(cli.format, Some(OutputFormat::JsonPp));
        assert!(cli.ignore_warnings);
        assert!(cli.ignore_nifti_headers);
        assert_eq!(cli.dataset_types, vec!["raw", "derivative"]);
        assert_eq!(cli.blacklist_modalities, vec!["MRI", "PET"]);
        assert!(cli.recursive);
    }

    #[test]
    fn version_string_reports_beta_metadata() {
        let version = version_string();
        for expected in [
            "rust 0.1",
            "rust git commit",
            "beta label     rust-beta parity-slice",
            "schema source  bundled",
            "@bids/schema",
            "bids version",
            "content mode   parity (default), thorough",
            "link mode      parity (default), follow, no-follow",
            "nifti headers  enabled by default",
        ] {
            assert!(
                version.contains(expected),
                "missing version metadata: {expected}\n{version}"
            );
        }
    }

    #[test]
    fn rejects_text_and_unsupported_schema_sources_explicitly() {
        let text = Cli::try_parse_from(["bids-validator", "--format", "text", "/tmp/ds"]).unwrap();
        assert!(resolve_output_format(&text)
            .unwrap_err()
            .to_string()
            .contains("--format text"));

        let schema = Cli::try_parse_from([
            "bids-validator",
            "--schema",
            "https://example.test/schema.json",
            "/tmp/ds",
        ])
        .unwrap();
        assert!(load_schema_context(schema.schema.as_deref())
            .unwrap_err()
            .to_string()
            .contains("remote schema loading"));
    }

    #[test]
    fn accepts_bundled_schema_path_and_matching_tags() {
        let schema_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("schemas/schema-1.2.1.json")
            .canonicalize()
            .unwrap();
        let path_ctx = load_schema_context(Some(schema_path.to_str().unwrap())).unwrap();
        assert_eq!(
            path_ctx.schema_version,
            bids_schema_codegen::schema().schema_version
        );
        assert_eq!(
            path_ctx.bids_version,
            bids_schema_codegen::schema().bids_version
        );

        let schema_version_ctx =
            load_schema_context(Some(&bids_schema_codegen::schema().schema_version)).unwrap();
        assert_eq!(
            schema_version_ctx,
            bids_schema_codegen::bundled_schema_context()
        );

        let v_tag = format!("v{}", bids_schema_codegen::schema().schema_version);
        let v_tag_ctx = load_schema_context(Some(&v_tag)).unwrap();
        assert_eq!(v_tag_ctx, bids_schema_codegen::bundled_schema_context());

        let bids_version_ctx =
            load_schema_context(Some(&bids_schema_codegen::schema().bids_version)).unwrap();
        assert_eq!(
            bids_version_ctx,
            bids_schema_codegen::bundled_schema_context()
        );
    }

    #[test]
    fn rejects_invalid_and_incompatible_schema_files() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("missing-schema.json");
        assert!(load_schema_context(Some(missing.to_str().unwrap()))
            .unwrap_err()
            .to_string()
            .contains("failed to read schema"));

        let invalid = tmp.path().join("invalid-schema.json");
        fs::write(&invalid, b"{not json").unwrap();
        assert!(load_schema_context(Some(invalid.to_str().unwrap()))
            .unwrap_err()
            .to_string()
            .contains("failed to parse schema"));

        let incompatible = tmp.path().join("schema-9.9.9.json");
        let mut value: serde_json::Value =
            serde_json::from_str(bids_schema_codegen::BUNDLED_SCHEMA_JSON).unwrap();
        value["schema_version"] = serde_json::Value::String("9.9.9".to_string());
        fs::write(&incompatible, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(load_schema_context(Some(incompatible.to_str().unwrap()))
            .unwrap_err()
            .to_string()
            .contains("not compatible"));
    }
}
