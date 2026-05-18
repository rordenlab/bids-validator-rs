use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const DEFAULT_UPSTREAM_VERSION: &str = "2.4.1";
const DEFAULT_UPSTREAM_COMMIT: &str = "94ea9719efbbac7531fed8ad8a2a7bf764cee51f";

fn main() {
    println!("cargo:rerun-if-env-changed=BIDS_VALIDATOR_GIT_COMMIT");
    println!("cargo:rerun-if-env-changed=BIDS_VALIDATOR_UPSTREAM_VERSION");
    println!("cargo:rerun-if-env-changed=BIDS_VALIDATOR_UPSTREAM_COMMIT");
    emit_git_rerun_paths();

    let commit = env::var("BIDS_VALIDATOR_GIT_COMMIT")
        .ok()
        .and_then(validated_commit)
        .or_else(git_commit)
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=BIDS_VALIDATOR_GIT_COMMIT={commit}");

    let upstream_version = env::var("BIDS_VALIDATOR_UPSTREAM_VERSION")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_UPSTREAM_VERSION.to_string());
    let upstream_commit = env::var("BIDS_VALIDATOR_UPSTREAM_COMMIT")
        .ok()
        .and_then(validated_commit)
        .unwrap_or_else(|| DEFAULT_UPSTREAM_COMMIT.to_string());
    println!("cargo:rustc-env=BIDS_VALIDATOR_UPSTREAM_VERSION={upstream_version}");
    println!("cargo:rustc-env=BIDS_VALIDATOR_UPSTREAM_COMMIT={upstream_commit}");
}

fn emit_git_rerun_paths() {
    let Ok(manifest_dir) = env::var("CARGO_MANIFEST_DIR") else {
        return;
    };
    let git_dir = PathBuf::from(manifest_dir).join(".git");
    let head = git_dir.join("HEAD");
    println!("cargo:rerun-if-changed={}", head.display());
    if let Ok(text) = fs::read_to_string(&head) {
        if let Some(reference) = text.trim().strip_prefix("ref: ") {
            println!(
                "cargo:rerun-if-changed={}",
                git_dir.join(reference).display()
            );
        }
    }
}

fn git_commit() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let commit = String::from_utf8(output.stdout).ok()?;
    validated_commit(commit)
}

fn validated_commit(commit: String) -> Option<String> {
    let commit = commit.trim();
    if (7..=40).contains(&commit.len()) && commit.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(commit.to_ascii_lowercase())
    } else {
        None
    }
}
