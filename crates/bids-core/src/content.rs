//! Per-run content cache. Memoizes small reads that the validator would
//! otherwise repeat: JSON sidecar text and other small text payloads.
//!
//! Lifetime is a single `validate_single_dataset` call. The cache is
//! stored on `DatasetContext`. Recursive derivative validation creates
//! a fresh cache for each derivative root.
//!
//! Key is the path as-passed (no canonicalization inside the cache).
//! Most callers operate on paths already resolved through `DirIndex`,
//! so re-canonicalizing on every lookup would defeat the cache.
//!
//! The cache distinguishes three outcomes per key — `Ok`,
//! `MissingOrUnreadable`, `PolicyRefused` — because parity vs. thorough
//! behavior downstream depends on the policy-refused case staying
//! visible. `read_json` caches only the underlying text; parse failures
//! remain per-call `None` results.
//!
//! Send + Sync: the cache uses `RwLock` internally so M4 can
//! share it across rayon workers without further refactor.

use std::env;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use indexmap::IndexMap;
use serde_json::Value as J;

use crate::policy::ContentPolicy;

/// Default LRU cap on the text cache. The inheritance hot path rereads
/// root/subject/session sidecars often, so refreshing entries on hit
/// keeps shared sidecars resident while one-off file-specific sidecars
/// rotate out. Capping at 512 entries holds the highest-value shared
/// sidecars (root + dataset-level + per-subject) without unbounded
/// growth on large datasets. Picked empirically against ds005016
/// (M1 acceptance gate is +10% RSS).
/// Override with `BIDS_VALIDATOR_CACHE_MAX`.
const DEFAULT_TEXT_CACHE_MAX: usize = 512;

fn text_cache_max() -> usize {
    env::var("BIDS_VALIDATOR_CACHE_MAX")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_TEXT_CACHE_MAX)
}

#[derive(Debug, Clone)]
pub enum ReadOutcome<T> {
    Ok(T),
    MissingOrUnreadable,
    PolicyRefused,
}

impl<T> ReadOutcome<T> {
    fn ok_ref(&self) -> Option<&T> {
        match self {
            Self::Ok(v) => Some(v),
            _ => None,
        }
    }
}

/// Insertion-ordered text cache with LRU refresh. `IndexMap` keeps
/// insertion order; hits are moved to the back, and overflow evicts the
/// front entry.
#[derive(Debug)]
struct LruTextCache {
    map: IndexMap<PathBuf, ReadOutcome<Arc<String>>>,
    max_entries: usize,
}

impl LruTextCache {
    fn new(max_entries: usize) -> Self {
        Self {
            map: IndexMap::with_capacity(max_entries.min(4096)),
            max_entries,
        }
    }

    fn get_refresh(&mut self, k: &Path) -> Option<ReadOutcome<Arc<String>>> {
        let value = self.map.shift_remove(k)?;
        self.map.insert(k.to_path_buf(), value.clone());
        Some(value)
    }

    /// Read-only view of the cached outcome, *without* refreshing
    /// recency. Used by tests that want to assert the discriminated
    /// outcome kind (`PolicyRefused` vs `MissingOrUnreadable`)
    /// without coupling to `LruTextCache`'s internal `IndexMap`.
    #[cfg(test)]
    fn peek(&self, k: &Path) -> Option<ReadOutcome<Arc<String>>> {
        self.map.get(k).cloned()
    }

    fn insert(&mut self, k: PathBuf, v: ReadOutcome<Arc<String>>) {
        if self.max_entries == 0 {
            return;
        }
        if !self.map.contains_key(&k) && self.map.len() >= self.max_entries {
            // LRU eviction: drop the least recently used entry.
            self.map.shift_remove_index(0);
        }
        self.map.insert(k, v);
    }

    fn len(&self) -> usize {
        self.map.len()
    }
}

#[derive(Debug)]
pub struct ContentCache {
    policy: ContentPolicy,
    text: RwLock<LruTextCache>,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl ContentCache {
    pub fn new(policy: ContentPolicy) -> Self {
        Self::with_capacity(policy, text_cache_max())
    }

    pub fn with_capacity(policy: ContentPolicy, text_cache_max: usize) -> Self {
        Self {
            policy,
            text: RwLock::new(LruTextCache::new(text_cache_max)),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    pub fn policy(&self) -> ContentPolicy {
        self.policy
    }

    /// Cached `read_to_string`. Returns `None` on policy refusal or I/O
    /// failure. The successful value is shared via `Arc<String>`; cloning
    /// the return value is a refcount bump, not a string copy.
    pub fn read_to_string(&self, path: &Path) -> Option<Arc<String>> {
        {
            let mut text = self.text.write().unwrap();
            if let Some(outcome) = text.get_refresh(path) {
                self.hits.fetch_add(1, Ordering::Relaxed);
                return outcome.ok_ref().cloned();
            }
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        let outcome = compute_text(&self.policy, path);
        let result = outcome.ok_ref().cloned();
        let mut text = self.text.write().unwrap();
        // Another worker may have filled the same key while this
        // thread was doing the blocking read. Prefer the existing
        // cached value so eviction order stays coherent. We don't
        // bump `hits` here — this thread already counted a miss and
        // already paid the disk-read cost, so observability-wise this
        // is still a miss; the duplicate I/O is wasted but rare.
        if let Some(existing) = text.get_refresh(path) {
            return existing.ok_ref().cloned();
        }
        text.insert(path.to_path_buf(), outcome);
        result
    }

    /// `read_to_string` + `serde_json::from_str`, with the *text* cached
    /// but the parsed value re-parsed on each call. Returns `None` on
    /// policy refusal, I/O failure, or parse failure.
    ///
    /// Why not cache `Arc<Value>`? Storing parsed JSON for every sidecar
    /// in a large dataset (e.g. ds005016 with thousands of sidecars)
    /// pushes peak RSS by ~70%, far beyond M1's 10% gate. Re-parsing
    /// from cached text keeps the I/O savings (the actual hot cost on
    /// slow storage) at much lower memory pressure. Parse cost is
    /// small compared to disk I/O on a cold cache, and serde_json is
    /// fast on already-resident bytes.
    pub fn read_json(&self, path: &Path) -> Option<Arc<J>> {
        let text = self.read_to_string(path)?;
        serde_json::from_str::<J>(&text).ok().map(Arc::new)
    }

    /// File size via `policy.metadata().len()`. This is intentionally
    /// not cached: production validation asks for each file's size once,
    /// so storing every path would add memory pressure without reuse.
    /// Returns `None` when the policy refuses or `metadata()` fails.
    /// Distinguishes `None` (no info) from `Some(0)` (empty file).
    pub fn metadata_size(&self, path: &Path) -> Option<u64> {
        self.policy.metadata(path).map(|m| m.len())
    }

    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    pub fn misses(&self) -> u64 {
        self.misses.load(Ordering::Relaxed)
    }

    pub fn entry_count(&self) -> usize {
        self.text.read().unwrap().len()
    }

    /// Read-only view of the cached outcome for `path`. Doesn't touch
    /// recency, doesn't update hit/miss counters. Exposed only for
    /// tests that need to assert which discriminated `ReadOutcome`
    /// variant is stored (e.g. distinguishing `PolicyRefused` from
    /// `MissingOrUnreadable` without coupling to the internal map
    /// representation).
    #[cfg(test)]
    pub(crate) fn peek_outcome(&self, path: &Path) -> Option<ReadOutcome<Arc<String>>> {
        self.text.read().unwrap().peek(path)
    }
}

fn compute_text(policy: &ContentPolicy, path: &Path) -> ReadOutcome<Arc<String>> {
    if !policy.allows(path) {
        return ReadOutcome::PolicyRefused;
    }
    match std::fs::read_to_string(path) {
        Ok(s) => ReadOutcome::Ok(Arc::new(s)),
        Err(_) => ReadOutcome::MissingOrUnreadable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::ContentMode;
    use std::fs;
    use tempfile::TempDir;

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    fn cache(mode: ContentMode) -> ContentCache {
        ContentCache::new(ContentPolicy::new(mode))
    }

    #[test]
    fn json_sidecar_reuses_cached_text() {
        // `read_json` is text-cache + per-call parse. The second call
        // must hit the text cache (not re-read from disk) but parse a
        // fresh Arc.
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("root.json");
        fs::write(&p, br#"{"RepetitionTime": 2.0}"#).unwrap();
        let c = cache(ContentMode::Parity);

        let a = c.read_json(&p).unwrap();
        let b = c.read_json(&p).unwrap();
        assert_eq!(c.misses(), 1, "text cache miss on first call only");
        assert_eq!(c.hits(), 1, "text cache hit on second call");
        assert_eq!(a["RepetitionTime"], 2.0);
        assert_eq!(b["RepetitionTime"], 2.0);
    }

    #[test]
    fn text_cache_returns_shared_arc() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("dataset_description.json");
        fs::write(&p, br#"{"Name": "ds"}"#).unwrap();
        let c = cache(ContentMode::Parity);

        let a = c.read_to_string(&p).unwrap();
        let b = c.read_to_string(&p).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(&**a, r#"{"Name": "ds"}"#);
    }

    #[test]
    fn missing_file_caches_failure_without_reattempt() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("never_existed.json");
        let c = cache(ContentMode::Parity);

        assert!(c.read_to_string(&p).is_none());
        assert!(c.read_to_string(&p).is_none());
        assert_eq!(c.misses(), 1, "disk read happens only on the first call");
        assert_eq!(c.hits(), 1, "second call serves the cached failure");
    }

    #[test]
    fn invalid_json_text_is_cached_even_though_parse_fails() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("bad.json");
        fs::write(&p, b"not json").unwrap();
        let c = cache(ContentMode::Parity);

        assert!(c.read_json(&p).is_none());
        assert!(c.read_json(&p).is_none());
        // Text cache hit on second call; both parses fail but the
        // unreadable file isn't re-opened.
        assert_eq!(c.misses(), 1, "text read happens once");
        assert_eq!(c.hits(), 1, "second call reuses cached text");
    }

    #[test]
    #[cfg(unix)]
    fn parity_mode_refuses_annex_symlink_and_caches_it() {
        let tmp = TempDir::new().unwrap();
        let target = tmp
            .path()
            .join("../../.git/annex/objects/SHA256E-s100--abc/abc");
        let link = tmp.path().join("sub-01_T1w.nii.gz");
        symlink(&target, &link).unwrap();
        let c = cache(ContentMode::Parity);

        assert!(c.read_to_string(&link).is_none());
        assert!(c.read_to_string(&link).is_none());
        assert_eq!(c.misses(), 1);
        assert_eq!(c.hits(), 1);

        // Confirm the cached outcome is discriminated as PolicyRefused,
        // not MissingOrUnreadable. (Important for downstream parity vs
        // thorough decisions.)
        let stored = c.peek_outcome(&link).unwrap();
        assert!(matches!(stored, ReadOutcome::PolicyRefused));
    }

    #[test]
    #[cfg(unix)]
    fn thorough_mode_reads_annex_symlink_target() {
        let tmp = TempDir::new().unwrap();
        let target_dir = tmp.path().join(".git/annex/objects/SHA256E-s100--abc");
        fs::create_dir_all(&target_dir).unwrap();
        let target = target_dir.join("abc");
        fs::write(&target, b"{}").unwrap();
        let link = tmp.path().join("sub-01_T1w.json");
        // Use a relative annex-shaped target so the parity check fires.
        symlink(
            std::path::Path::new(".git/annex/objects/SHA256E-s100--abc/abc"),
            &link,
        )
        .unwrap();
        let c = cache(ContentMode::Thorough);

        let v = c.read_json(&link).unwrap();
        assert!(v.is_object());
    }

    #[test]
    fn metadata_size_distinguishes_empty_and_present() {
        let tmp = TempDir::new().unwrap();
        let empty = tmp.path().join("empty.nii");
        fs::write(&empty, b"").unwrap();
        let nonempty = tmp.path().join("nonempty.nii");
        fs::write(&nonempty, b"abcdef").unwrap();
        let missing = tmp.path().join("missing.nii");
        let c = cache(ContentMode::Parity);

        assert_eq!(c.metadata_size(&empty), Some(0));
        assert_eq!(c.metadata_size(&nonempty), Some(6));
        assert_eq!(c.metadata_size(&missing), None);
    }

    #[test]
    fn lru_eviction_drops_least_recently_used_entry_when_full() {
        let tmp = TempDir::new().unwrap();
        let files: Vec<_> = (0..5)
            .map(|i| {
                let p = tmp.path().join(format!("f{i}.json"));
                fs::write(&p, format!(r#"{{"i": {i}}}"#)).unwrap();
                p
            })
            .collect();
        let c = ContentCache::with_capacity(ContentPolicy::new(ContentMode::Parity), 3);

        // Fill cache to capacity.
        for p in &files[..3] {
            let _ = c.read_to_string(p);
        }
        assert_eq!(c.text.read().unwrap().len(), 3);

        // Refresh files[0], then insert one more. The least-recently
        // used entry is now files[1].
        let _ = c.read_to_string(&files[0]);
        let _ = c.read_to_string(&files[3]);
        assert_eq!(c.text.read().unwrap().len(), 3);
        // Use `peek_outcome` so the existence checks don't themselves
        // refresh recency and contaminate the next assertion.
        assert!(c.peek_outcome(&files[0]).is_some());
        assert!(c.peek_outcome(&files[1]).is_none());

        // Re-reading the evicted file is a miss but doesn't crash.
        let pre_miss = c.misses();
        let _ = c.read_to_string(&files[1]);
        assert_eq!(c.misses(), pre_miss + 1);
    }

    #[test]
    fn zero_capacity_disables_text_cache() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("root.json");
        fs::write(&p, br#"{"RepetitionTime": 2.0}"#).unwrap();
        let c = ContentCache::with_capacity(ContentPolicy::new(ContentMode::Parity), 0);

        let _ = c.read_to_string(&p);
        let _ = c.read_to_string(&p);
        assert_eq!(c.entry_count(), 0);
        assert_eq!(c.hits(), 0);
        assert_eq!(c.misses(), 2);
    }

    #[test]
    fn hot_inheritance_pattern_amortizes_to_one_miss() {
        // Simulate the inheritance hot loop: 100 data files all "read"
        // the same root sidecar text. The cache should produce 1 miss
        // and 99 hits.
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("root.json");
        fs::write(&p, br#"{"RepetitionTime": 2.0}"#).unwrap();
        let c = cache(ContentMode::Parity);

        for _ in 0..100 {
            let _ = c.read_to_string(&p);
        }
        assert_eq!(c.misses(), 1);
        assert_eq!(c.hits(), 99);
    }
}
