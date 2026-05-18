//! Core BIDS validation primitives: filename parsing, rule matching, issues.
#![forbid(unsafe_code)]

pub mod associations;
pub mod checks;
pub mod compile;
pub mod content;
pub mod filename;
pub mod gzip;
pub mod inheritance;
pub mod issue;
pub mod metadata;
pub mod nifti;
pub mod policy;
pub mod rules;
pub mod run;
pub mod tables;
pub mod tiff;
pub mod tsv;
