//! Content-read policy. Single source of truth for whether the validator
//! is allowed to read a file's bytes during validation.
//!
//! Mirrors the TS validator's implicit policy: Deno's `readBytes` returns
//! `null`/raises when a file isn't locally available (e.g. a broken
//! git-annex symlink), and downstream checks treat the resulting empty
//! state as "no content-derived facts." Before this module existed,
//! Rust had three independent `content_reads_allowed` definitions
//! (`bids-cli/src/main.rs`, `bids-core/src/associations.rs`) and two
//! `is_git_annex_symlink` definitions, all of which substring-matched
//! `.git/annex/objects` against the link target — a path component
//! containing that literal string would be falsely skipped.
//!
//! All content-derived filesystem reads in `inheritance`,
//! `associations`, `tsv`, `nifti`, `gzip`, `tiff`, dataset-type
//! detection, and the per-file loop route through a single
//! [`ContentPolicy`].

use std::fs;
use std::path::{Component, Path};

/// What content-mode the validator is running in. `Parity` mirrors
/// Deno's read-or-skip behavior for git-annex symlinks (so the
/// implemented-code multiset stays comparable); `Thorough` reads
/// whatever is locally accessible.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentMode {
    Parity,
    Thorough,
}

/// Decides whether a given path's bytes should be read, and centralizes
/// the I/O so call sites don't have to. Cheap to clone / pass by value.
#[derive(Clone, Copy, Debug)]
pub struct ContentPolicy {
    mode: ContentMode,
}

impl ContentPolicy {
    pub fn new(mode: ContentMode) -> Self {
        Self { mode }
    }

    pub fn mode(&self) -> ContentMode {
        self.mode
    }

    /// `true` if reading `path`'s content is permitted under this
    /// policy. Prefer the convenience methods below so I/O remains
    /// centralized.
    pub fn allows(&self, path: &Path) -> bool {
        match self.mode {
            ContentMode::Thorough => true,
            ContentMode::Parity => !is_git_annex_symlink(path),
        }
    }

    /// Read the file to a `String`. `None` if disallowed by policy
    /// **or** if the read fails (broken symlink, permission, etc.) —
    /// matching the swallowed-error behavior of the pre-refactor
    /// call sites.
    pub fn read_to_string(&self, path: &Path) -> Option<String> {
        if !self.allows(path) {
            return None;
        }
        fs::read_to_string(path).ok()
    }

    /// Stat the file. `None` when disallowed by policy or stat fails.
    /// Used for `file_size` in the eval context — without policy
    /// gating, `metadata()` follows symlinks and returns the linked
    /// object's size, defeating parity mode.
    pub fn metadata(&self, path: &Path) -> Option<fs::Metadata> {
        if !self.allows(path) {
            return None;
        }
        fs::metadata(path).ok()
    }

    /// Open the file for incremental reads (NIfTI header peek, gzip
    /// header scan, TIFF prelude). `None` when disallowed or open
    /// fails.
    pub fn open(&self, path: &Path) -> Option<fs::File> {
        if !self.allows(path) {
            return None;
        }
        fs::File::open(path).ok()
    }
}

/// Tighter replacement for the previous substring-based check. A path
/// like `…/some-..git-annex-objects.../foo` is **not** an annex
/// symlink even though its target contains the literal substring;
/// matching by [`Path::components`] is component-aware.
fn is_git_annex_symlink(path: &Path) -> bool {
    let Ok(target) = fs::read_link(path) else {
        return false;
    };
    let mut comps = target.components();
    // Walk components; look for `.git` followed by `annex` followed by
    // `objects` as adjacent path segments. Targets are typically
    // relative like `../../.git/annex/objects/SHA-.../...`.
    // We intentionally do not normalize `..` here. This predicate only
    // decides whether parity mode should skip a content read; treating
    // suspicious relative annex-shaped links as unreadable is the safe
    // direction and does not authorize filesystem access.
    while let Some(c) = comps.next() {
        if matches!(c, Component::Normal(s) if s == ".git")
            && matches!(comps.next(), Some(Component::Normal(s)) if s == "annex")
            && matches!(comps.next(), Some(Component::Normal(s)) if s == "objects")
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    #[test]
    fn parity_allows_regular_file() {
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join("foo.json");
        fs::write(&f, b"{}").unwrap();
        let p = ContentPolicy::new(ContentMode::Parity);
        assert!(p.allows(&f));
        assert_eq!(p.read_to_string(&f).as_deref(), Some("{}"));
    }

    #[test]
    #[cfg(unix)]
    fn parity_skips_annex_symlink() {
        let tmp = TempDir::new().unwrap();
        let target = tmp
            .path()
            .join("../../.git/annex/objects/SHA256E-s100--abc/abc");
        let link = tmp.path().join("sub-01_T1w.nii.gz");
        symlink(&target, &link).unwrap();
        let p = ContentPolicy::new(ContentMode::Parity);
        assert!(
            !p.allows(&link),
            "annex symlink must be skipped in parity mode"
        );
        assert!(p.read_to_string(&link).is_none());
    }

    #[test]
    #[cfg(unix)]
    fn thorough_allows_annex_symlink() {
        let tmp = TempDir::new().unwrap();
        let target = tmp
            .path()
            .join("../../.git/annex/objects/SHA256E-s100--abc/abc");
        let link = tmp.path().join("sub-01_T1w.nii.gz");
        symlink(&target, &link).unwrap();
        let p = ContentPolicy::new(ContentMode::Thorough);
        assert!(p.allows(&link));
    }

    #[test]
    #[cfg(unix)]
    fn substring_decoy_is_not_annex() {
        // A path containing the literal `.git/annex/objects` as part of
        // a single component (not adjacent segments) must not be
        // detected as annex. Previous substring check failed this.
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("decoy_.git_annex_objects_dir/payload");
        let link = tmp.path().join("sub-01_T1w.nii.gz");
        symlink(&target, &link).unwrap();
        let p = ContentPolicy::new(ContentMode::Parity);
        assert!(p.allows(&link), "decoy substring path must not be skipped");
    }

    #[test]
    fn missing_file_returns_none_not_panic() {
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join("nope.json");
        let p = ContentPolicy::new(ContentMode::Thorough);
        assert!(p.read_to_string(&f).is_none());
        assert!(p.metadata(&f).is_none());
        assert!(p.open(&f).is_none());
    }
}
