//! `bids-validator` Rust CLI — MVP slice.
//!
//! Walks a dataset, runs filename-level validation, emits issue JSON in a
//! shape comparable to the Deno validator's `--json` output.
#![forbid(unsafe_code)]

use anyhow::Result;
use clap::Parser;

use bids_schema_codegen::load_schema_context;

mod cli;
mod discover;
mod output;
mod validate;

use cli::{reject_unsupported_options, resolve_output_format, Cli};
use output::write_output;
use validate::{has_errors, validate_cli};

fn main() -> Result<()> {
    let cli = Cli::parse();
    let output_format = resolve_output_format(&cli)?;
    reject_unsupported_options(&cli)?;

    let schema_context = load_schema_context(cli.schema.as_deref())?;
    let result = validate_cli(&cli, &schema_context)?;
    write_output(&result, output_format, cli.outfile.as_deref())?;
    if has_errors(&result) {
        std::process::exit(1);
    }

    Ok(())
}
