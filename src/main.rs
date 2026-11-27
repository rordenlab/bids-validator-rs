//! `bids-validator` Rust CLI — MVP slice.
//!
//! Walks a dataset, runs filename-level validation, emits issue JSON in a
//! shape comparable to the Deno validator's `--json` output.
#![forbid(unsafe_code)]

use anyhow::Result;
use clap::Parser;

use bids_schema_codegen::load_schema_context;
use bids_validator_rs::validate_path;

mod cli;
mod output;

use cli::{reject_unsupported_options, resolve_output_format, Cli};
use output::write_output;

fn main() -> Result<()> {
    let cli = Cli::parse();
    let output_format = resolve_output_format(&cli)?;
    reject_unsupported_options(&cli)?;

    load_schema_context(cli.schema.as_deref())?;
    let result = validate_path(&cli.dataset, &cli.validation_options())?;
    write_output(&result, output_format, cli.outfile.as_deref())?;
    if result.has_errors() {
        std::process::exit(1);
    }

    Ok(())
}
