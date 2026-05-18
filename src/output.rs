//! Serialize the CLI result to stdout or a file.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

use anyhow::Result;

use crate::cli::OutputFormat;
use crate::validate::CliValidationResult;

pub(crate) fn write_output(
    result: &CliValidationResult,
    format: OutputFormat,
    outfile: Option<&Path>,
) -> Result<()> {
    if let Some(path) = outfile {
        let mut file = fs::File::create(path)?;
        write_json_output(&mut file, result, format)?;
    } else {
        let stdout = io::stdout();
        let mut lock = stdout.lock();
        write_json_output(&mut lock, result, format)?;
        writeln!(lock)?;
    }
    Ok(())
}

fn write_json_output<W: Write>(
    writer: &mut W,
    result: &CliValidationResult,
    format: OutputFormat,
) -> Result<()> {
    match format {
        OutputFormat::Json => serde_json::to_writer(writer, result)?,
        OutputFormat::JsonPp => serde_json::to_writer_pretty(writer, result)?,
        OutputFormat::Text => unreachable!("text output rejected before validation"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bids_core::issue::IssueList;
    use std::collections::BTreeMap;

    fn format_output(result: &CliValidationResult, format: OutputFormat) -> Result<String> {
        Ok(match format {
            OutputFormat::Json => serde_json::to_string(result)?,
            OutputFormat::JsonPp => serde_json::to_string_pretty(result)?,
            OutputFormat::Text => unreachable!("text output rejected before validation"),
        })
    }

    #[test]
    fn json_pp_output_is_pretty_json() {
        let result = CliValidationResult {
            issues: IssueList::default(),
            derivatives_summary: BTreeMap::new(),
        };
        let out = format_output(&result, OutputFormat::JsonPp).unwrap();
        assert!(out.starts_with("{\n"));
        assert!(out.contains("\"issues\""));
    }
}
