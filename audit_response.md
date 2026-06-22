# Audit response & parity-divergence rationale

This file is the durable rationale store that `tests/parity/run.py` points to from its `KNOWN_DIVERGENCES` / `KNOWN_DIVERGENCE_ISSUES` comments, plus the response to the external code reviews captured (and deleted) in `audit_temp.md`.

## Accepted parity divergences (referenced by run.py)

### `ds000003` — `GZIP_HEADER_FILENAME` / `GZIP_HEADER_MTIME`

35 of 47 `.nii.gz` files are broken git-annex symlinks whose object isn't checked out locally. Deno's `readBytes` resolves them via the git-annex layer and parses the gzip header anyway; Rust's `parse_gzip` opens the symlink directly, gets `ENOENT`, returns `None`, and under-fires both codes by 39 each. Fixing needs a git-annex-aware loader (plan.md Phase 2). Accepted divergence.

### `ds002606` — `SIDECAR_WITHOUT_DATAFILE`

`/prefs.json` is a non-standalone JSON that both validators mark `NOT_INCLUDED`. Deno additionally runs `walkBack` for the `.json` itself, where the source matches its own walk (self-self subset) and sets `source.viewed = true` — so Deno's orphan check is effectively dead code that never fires on a well-formed suffix. Rust deliberately diverges: it skips *self* in the viewed-set accumulator (only ancestor sidecars count as inheriting), so genuine orphans actually fire. Tracked as an intentional Rust-only signal, not a parity bug.

## Response to the external reviews (`audit_temp.md`, now deleted)

Two successive reviews were considered. Each point is **Fixed**, **Discuss**, or **Won't fix (now)**.

### `.github/ISSUE_TEMPLATE/rust_beta.md` frontmatter collapsed — **Fixed**

The markdown reflow joined the YAML frontmatter onto one line, which would break the GitHub issue template. Restored to one key per line. (The reflow was an explicit request; this was the one real break it caused.)

### ds005016 parity sweep red — **Discuss (pre-existing, not a regression)**

Verified by stashing the WIP and re-running: the failure multiset is byte-identical with and without the changes (`rust=15996 deno=15998`). The WIP only *adds* `viewed` marks, so it can only remove `SIDECAR_WITHOUT_DATAFILE`, never add the `/scans.json` one. The gaps (`MULTIPLE_README_FILES`/`README_FILE_SMALL` out-of-contract, `/scans.json` orphan via the *inheritance* walk, `GeneratedBy`/`SourceDatasets` recommended keys, `participants.group` column) are unimplemented-slice divergences on a large real dataset. `ds005016`/`ds000003`/`ds002606` are also wired to machine-specific absolute paths and skipped on checkouts where they're absent. Not a blocker for this WIP; tracked in `CLAUDE.md`.

### Electrophysiology corpus gap — **Fixed**

Root cause of why the coordsystem bug was never caught: the parity suite had zero MEG/EEG/iEEG datasets. Added `eeg_face13` (EEG) and `ieeg_epilepsy` (iEEG, multi-target `space-*` coordsystem) via `scripts/fetch_bench_data.py` into gitignored `data/` — zero repo bulk. Both are fully green and exercise the coordsystem association/`viewed` walk on real data. Adding them surfaced two real Rust-emitted codes Deno also emits (`EOG_CHANNEL_COUNT_MISMATCH`, `MISC_CHANNEL_COUNT_MISMATCH`), now added to the `RUST_CODES` contract (electrophysiology-only, so MRI/PET caches are unaffected). CTF-MEG (`ds000246`) was excluded: Deno treats the `.ds/` recording as one unit, Rust walks its internal files — a separate unimplemented divergence, out of scope.

**Durability (raised by a later review):** `data/` and `tests/parity/cache/*.json` are gitignored, so the fetched datasets are skipped on a fresh checkout/CI. This is the existing parity-suite design (the whole sweep is a local developer tool — CI runs `cargo test`, not the parity sweep). The *durable, CI-gated* regression guard for the coordsystem fix is the unit tests in `crates/bids-core/tests/sidecar_without_datafile.rs`; the fetched datasets are an extra local cross-check. We do **not** vendor external datasets into the repo (bulk + the user's explicit "no bulk" constraint). An earlier draft of `CLAUDE.md` wrongly said the Deno cache is committed — corrected.

**Parity contract docs (raised by a later review):** adding the two channel codes to `RUST_CODES` made `tests/parity/ISSUE_COVERAGE.md` stale. Updated in the same change: implemented count 50→52, missing 128→126, both codes moved from the missing list to the implemented contract. Also fixed a pre-existing malformed table cell in `README.md` and refreshed the `BETA.md`/`ISSUE_COVERAGE.md` status dates.

**ds005016 default-sweep ambiguity (raised twice):** acknowledged as a workflow wart — a machine-specific, known-red dataset shares the default sweep path with required-green datasets. Pre-existing, not introduced here. Documented follow-up: move machine-specific/known-red datasets behind an explicit opt-in suite. Not fixed in this change to keep scope tight.

**Fingerprint "still too broad" (raised again):** the reviewer wants the exact normalized selector set. Kept the both-substrings check — the `*_patch_still_needed` self-test already fires on *any* change to the selectors, which is the real human-review trigger; an exact-set match is gold-plating for a one-rule patch. Noted as the stronger option if more patches accrue.

### Schema patch fingerprint too weak — **Fixed**

`spoiling_type_bug_present` now requires BOTH a `modality` selector and an `extension` selector before treating upstream as fixed, so a partial upstream fix keeps the patch active and keeps the self-test flagging for human review.

### Schema patch governance not enforced — **Fixed**

Added `patches_do_not_touch_statically_emitted_subtrees`: applies all patches and asserts `objects.formats`, `objects.entities`, and `rules.modalities` are unchanged (those are emitted by `build.rs` from the raw schema). A future patch touching them fails the test, forcing build-time support. General guard, no per-patch metadata.

### Schema accessor comment disagreed with implementation — **Fixed**

`schema()`'s doc comment now describes parse→Value→patch→deserialize and acknowledges the one-time `Value` materialization cost (the price of keeping the on-disk JSON byte-verbatim).

### Sidecar-orphan / build.rs / association docs stale — **Fixed**

Rewrote the stale `run.rs` and `sidecar_without_datafile.rs` headers (EMG carve-out removed), the `build.rs` header (now lists formats+entities+modalities plus the raw/patched invariant), and narrowed the `associations.rs` "mirrors Deno exactly" wording to "matches Deno's association side-effect" (it does not claim full orphan-sidecar parity).

### Cache write-lock on hot hits — **Won't fix (now)**

Pre-existing in `content.rs`, unrelated to this WIP, bounded (LRU recency refresh on a write lock). A throughput ceiling on huge datasets, not a leak. Perf follow-up; tracked in `CLAUDE.md`.

### Association `viewed` returns `Vec<PathBuf>` (incl. non-JSON, dups) — **Won't fix**

Minimal correct form. The consumer already does a linear `viewed.contains` over a `Vec`; switching to a `HashSet` or pre-filtering to `.json` would couple `associations.rs` to the orphan pass's "only `.json` matters" assumption to save a few `PathBuf` clones per file.

### Test fixture builders duplicated — **Discuss (optional)**

`build_ctx`/`discover`/`infer_datatype` are triplicated across three integration-test files. A `tests/common/mod.rs` would dedupe ~50 lines. Worth doing before the next behavioral test; not blocking.

### patches.rs framework for one patch / lacks concrete upstream ref — **Discuss**

The `SchemaPatch` table is generic machinery for one entry; it could collapse to two free functions. Kept because the self-tests (`*_patch_still_needed`, `*_patched_at_runtime`, the governance guard) are the real value and read cleanly against the table. `upstream_ref` is a search hint until an upstream issue is filed.

## Round 3 (crafted-fixture PR #3 audit)

A later review of the crafted-fixture work (DWI/fmap/NIfTI families). Resolutions:

- **`.tsv.gz` missed the BOM fix — Fixed.** The earlier BOM strip was only in `parse_tsv`; `parse_tsv_body_with_headers` (the `.tsv.gz` path) now strips it too, with a regression test.
- **Default parity sweep ambiguous — Fixed.** Machine-specific / known-red datasets (`ds000003`, `ds002606`, `ds005016`) moved to an opt-in `OPT_IN_DATASETS` dict in `run.py`. The no-argument sweep now runs only the expected-green set (vendor + fetched bids-examples) and is a clean pass/fail signal; opt-in datasets run when named explicitly.
- **`ISSUE_COVERAGE.md` count drift — Fixed.** `SUBJECT_FOLDERS` was in `RUST_CODES` but listed as missing; moved to implemented, counts corrected (now 69 implemented / 109 missing; status table sums to 202).
- **Evidence levels conflated — Fixed (documented).** `ISSUE_COVERAGE.md` now distinguishes *dataset parity* (continuously Deno-checked) from *crafted-fixture* codes (CI-gated Rust assertions; Deno parity confirmed once during development).
- **Crafted NIfTI builder triplicated — Fixed.** Moved the minimal NIfTI header builder into `tests/common/mod.rs`; the three check files share it.
- **Fixture builder doesn't populate modalities/subjects — Documented.** Added a note on `build_ctx` that it's a narrow fixture builder (empty `dataset_modalities`/`dataset_subjects`); tests depending on those must set them explicitly.
- **Clean-checkout coverage / TSV buffering / cache lock / association alloc — Discuss / Won't fix.** Pre-existing and by-design (parity sweep + oracle are local dev tools, CI runs `cargo test`); tracked in `CLAUDE.md`. The crafted-fixture integration tests are the durable, CI-gated guard.

## Verdict

Committable. `cargo fmt`, `clippy -D warnings`, and `cargo test --workspace` are green. The **default** parity sweep is green on all 14 present datasets (vendor + fetched bids-examples); `ds005016` (opt-in) carries documented pre-existing divergences; `ds000003`/`ds002606` (opt-in) skip when their machine-specific paths are absent.
