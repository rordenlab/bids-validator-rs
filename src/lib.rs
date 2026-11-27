//! Library facade for the native Rust BIDS validator split repository.
//!
//! Validate a BIDS dataset on disk with [`validate_path`].
#![forbid(unsafe_code)]

mod discover;
mod validate;

pub use bids_core as core;
pub use bids_core::policy::ContentMode;
pub use bids_expr as expr;
pub use bids_schema_codegen as schema;
pub use validate::{validate_path, ValidationOptions, ValidationReport};
