# Re-ingest Version Gate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Re-parse and re-embed every file when the parser schema or parser logic changes, while unchanged files under the same version skip re-parsing exactly as they do today.

**Architecture:** File-level `file_sha256` reuse already exists in `src/parser/ingest.rs` (producer skips files whose hash matches the previous active generation, consumer carries forward their rows and embeddings). This plan adds a deterministic `ingest_version` stamp — `sha256(PARSER_LOGIC_VERSION + canonical schema fingerprint)` — written into every ingested node's metadata and compared against the previous generation's stamp. When it differs (or is absent), the producer disables the skip path for that run, so all files re-parse and re-embed; the observer records the trip as a warning, never an error.

**Tech Stack:** Rust, `sha2 = "0.10"` (already a dependency), `tree-sitter` parser stack (unchanged), `serde_json` (already a dependency).

**Spec:** `docs/superpowers/specs/2026-10-27-reingest-hash-reuse-design.md`

## Global Constraints

- Hash is content-based, never timestamp-based.
- No LLM anywhere in the hash/reuse path.
- Library hygiene: `sha2` only; no `oxirs-core` / `scirs2` / `horned-owl`; no new dependencies.
- Backward compatible: pre-migration rows without an `ingest_version` field are re-parsed, never skipped.
- Warn-first: version change or missing stamp is reported (observer + `tracing::warn!`), never fatal.
- Per-namespace isolation: the stamp comparison reads only the same namespace + source generation.
- Existing `file_sha256` skip semantics are preserved when the version matches.

## Graph Pre-flight (NeuroStrata memory)

Read before implementation; these govern the files this plan touches:

- **Staged generations are mandatory** (fact `ca4efa81`): ids are `namespace::generation::path`; ingest calls `begin_generation` → `publish_generation`, abort on failure. Nothing is deleted in place. The version stamp rides in per-node metadata of the new generation; no store API changes.
- **Symbol identity** is `span_sha256` of the definition text, not the start line — unchanged files keep identical symbol rows when carried forward.
- **Test DB opens go through `LadybugStore::for_testing`** (rule `8c85f445`), never `new` or a default `SystemConfig` — the 8 TiB mmap reservation trips CI. The Task 2 test harness already follows this.
- **Identifiers in commits/reports only echo tool output** (rule `494a50ec`); do not pre-write hashes.
- The ingested AST graph may lag the source (an old `ingest_directory` node still shows the pre-generation `clear_ingested` flow). Treat current source as ground truth for structure; use the graph for rules/facts.

## Review Focus

- **Pre-migration rows (no `ingest_version`):** must trigger a full re-parse with a warning; silently skipping would ship stale symbols forever.
- **Schema key insertion order:** two equivalent schemas built in different orders must fingerprint identically, or every single ingest re-parses everything.
- **Equivalent extension spellings (`"rs"` vs `".rs"`):** `--schema-path` may pass either; normalized before hashing so the same grammar set fingerprints the same.
- **Empty/one-language schema:** fingerprint stays stable and ingest still runs.
- **File deleted between runs:** its node must not be resurrected by carry-forward; the skip map only covers paths the current walk visits.

---

### Task 1: Deterministic schema fingerprint and ingest version helpers

**Files:**
- Modify: `src/parser/schema.rs`
- Modify: `src/parser/generation.rs`
- Create: `tests/ingest/version_gate_test.rs`
- Modify: `Cargo.toml` (add `[[test]]` entry near the existing `staged_publication_test` entry)

**Interfaces:**
- Consumes: nothing (first task).
- Produces:
  - `impl ParserSchema { pub fn fingerprint(&self) -> String }` — lowercase hex sha256 over a canonical JSON form: languages sorted by name, each `extensions` normalized (`trim_start_matches('.')`, lowercase) and sorted, `queries` sorted by key.
  - `pub const PARSER_LOGIC_VERSION: u32 = 1;` in `src/parser/generation.rs`
  - `pub fn ingest_version(schema_fingerprint: &str) -> String` in `src/parser/generation.rs` — `sha256_hex(format!("v{PARSER_LOGIC_VERSION}:{schema_fingerprint}").as_bytes())`
  - `pub fn version_gate_tripped(prev: Option<&str>, current: &str) -> bool` in `src/parser/generation.rs` — `prev != Some(current)`

- [ ] **Step 1: Write the failing tests**

Create `tests/ingest/version_gate_test.rs`. Both modules are crate-free by design, so compile them from their real paths (same pattern as `staged_publication_test.rs`):

```rust
#![allow(dead_code)]

#[path = "../../src/parser/schema.rs"]
mod schema;
#[path = "../../src/parser/generation.rs"]
mod generation;

use schema::ParserSchema;
use generation::{ingest_version, version_gate_tripped, PARSER_LOGIC_VERSION};

fn schema_json(a_first: bool) -> String {
    // Same semantic schema; key insertion order differs.
    if a_first {
        r#"{"languages":{"rust":{"extensions":[".rs","rs"],"queries":{"functions":"(function_item) @fn"}},"python":{"extensions":["py"],"queries":{"functions":"(function_definition) @fn"}}}}"#
    } else {
        r#"{"languages":{"python":{"extensions":["py"],"queries":{"functions":"(function_definition) @fn"}},"rust":{"extensions":["rs",".rs"],"queries":{"functions":"(function_item) @fn"}}}}"#
    }
    .to_string()
}

#[test]
fn fingerprint_is_stable_across_key_and_extension_order() {
    let a = ParserSchema::load(&schema_json(true)).expect("parses");
    let b = ParserSchema::load(&schema_json(false)).expect("parses");
    assert_eq!(a.fingerprint(), b.fingerprint());
}

#[test]
fn fingerprint_changes_when_a_query_changes() {
    let a = ParserSchema::load(&schema_json(true)).expect("parses");
    let changed = schema_json(true).replace("(function_item) @fn", "(function_item) @fn2");
    let b = ParserSchema::load(&changed).expect("parses");
    assert_ne!(a.fingerprint(), b.fingerprint());
}

#[test]
fn ingest_version_changes_with_logic_version_or_fingerprint() {
    let fp = "abc123";
    let v = ingest_version(fp);
    assert_eq!(v, ingest_version("abc123"));
    assert_ne!(v, ingest_version("def456"));
    assert!(v.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(PARSER_LOGIC_VERSION, 1);
}

#[test]
fn version_gate_trips_only_when_stamp_is_missing_or_different() {
    assert!(!version_gate_tripped(Some("v1"), "v1"));
    assert!(version_gate_tripped(Some("v1"), "v2"));
    assert!(version_gate_tripped(None, "v1"), "pre-migration rows must re-parse");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test version_gate_test`
Expected: compile error — `fingerprint`, `ingest_version`, `version_gate_tripped`, `PARSER_LOGIC_VERSION` not found.

- [ ] **Step 3: Implement the helpers**

In `src/parser/schema.rs`, add a canonical-form fingerprint. Build a `serde_json::Value` with sorted keys (`BTreeMap` or manual insertion in sorted order), normalize extensions, then `sha256_hex` the serialized bytes. Reuse `sha2` directly (`use sha2::{Digest, Sha256};`) — `sha256_hex` from `generation.rs` is not importable here without a `crate::` dependency, and `schema.rs` must stay crate-free for the integration test.

In `src/parser/generation.rs`, add the const, `ingest_version`, and `version_gate_tripped` exactly as specified in Interfaces. `sha256_hex` already exists in that module.

- [ ] **Step 4: Add the Cargo test target**

Add under the existing `staged_publication_test` entry:

```toml
# emrz: version-gated re-parse contract. Compiles the crate-free schema and
# generation helpers from their real paths.
[[test]]
name = "version_gate_test"
path = "tests/ingest/version_gate_test.rs"
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --test version_gate_test`
Expected: 4 passed.

- [ ] **Step 6: Commit**

```bash
git add src/parser/schema.rs src/parser/generation.rs tests/ingest/version_gate_test.rs Cargo.toml
git commit -m "feat(ingest): deterministic schema fingerprint and ingest version helpers"
```

---

### Task 2: Version-gated skip in the ingest producer

**Files:**
- Modify: `src/parser/ingest.rs` (observer trait ~line 296; producer skip ~line 479; prev-generation scan ~line 873; item payloads ~line 496, ~line 514, ~line 583; metadata stamps ~line 652, ~line 728, ~line 799)
- Modify: `src/ingest_jobs.rs` (`ProgressReporter` observer impl, ~line 80)
- Test: `#[cfg(test)] mod tests` in `src/parser/ingest.rs` (store-driven `#[tokio::test]`, same harness as `producer_consumer_streams_items_to_store`)

**Interfaces:**
- Consumes: `generation::{ingest_version, version_gate_tripped}` from Task 1.
- Produces:
  - Metadata key `"ingest_version"` on every file, directory, symbol, and doc-section node.
  - `IngestObserver::version_gate_tripped(&self, prev: &str, current: &str)` with a default no-op body (backward compatible for existing implementors).
  - `ingest_directory` behavior: `force_reparse = prev_gen > 0 && version_gate_tripped(prev_version.as_deref(), &current_version)`; when true, the `prev_file_map` skip at line ~479 is bypassed for the whole run.

- [ ] **Step 1: Write the failing test**

Add to the existing `#[cfg(test)] mod tests` in `src/parser/ingest.rs`, following the `producer_consumer_streams_items_to_store` harness (temp dir, `LadybugStore::for_testing`, `ZeroEmbedder`, `ingest_directory`):

```rust
#[tokio::test]
async fn unchanged_files_skip_but_a_schema_change_forces_reparse() {
    struct Recorder(std::sync::Mutex<(usize, usize)>); // (skipped, gate_trips)
    impl IngestObserver for Recorder {
        fn file_skipped(&self, _path: &str) {
            self.0.lock().unwrap().0 += 1;
        }
        fn version_gate_tripped(&self, _prev: &str, _current: &str) {
            self.0.lock().unwrap().1 += 1;
        }
    }

    let tmp = std::env::temp_dir().join(format!("ns-version-gate-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(tmp.join("hello.rs"), "fn main() {}").unwrap();
    let db = std::env::temp_dir().join(format!("ns-version-gate-db-{}", uuid::Uuid::new_v4()));
    let store: Arc<dyn VectorStore> =
        Arc::new(crate::store::LadybugStore::for_testing(&db, 4).unwrap());
    store.init("probe").await.unwrap();

    let schema = ParserSchema::load(include_str!("../schema.json")).unwrap();
    let rec1 = Arc::new(Recorder(std::sync::Mutex::new((0, 0))));
    ingest_directory(&tmp, &schema, Arc::new(ZeroEmbedder), store.clone(), "probe", Some(rec1.clone()) as Option<Arc<dyn IngestObserver>>).await.unwrap();
    assert_eq!(rec1.0.lock().unwrap().0, 0, "first ingest has nothing to skip");

    // Same schema: unchanged file is skipped and the gate does not trip.
    let rec2 = Arc::new(Recorder(std::sync::Mutex::new((0, 0))));
    ingest_directory(&tmp, &schema, Arc::new(ZeroEmbedder), store.clone(), "probe", Some(rec2.clone()) as Option<Arc<dyn IngestObserver>>).await.unwrap();
    assert_eq!(rec2.0.lock().unwrap().0, 1, "unchanged file skipped");
    assert_eq!(rec2.0.lock().unwrap().1, 0, "version gate quiet");

    // Changed schema (fingerprint differs): gate trips, nothing is skipped.
    // Mutating the struct avoids brittle JSON surgery and keeps the rust
    // grammar mapping intact, so hello.rs still parses.
    let mut altered = schema.clone();
    altered
        .languages
        .get_mut("rust")
        .expect("shipped schema has rust")
        .extensions
        .push("rs2".to_string());
    let rec3 = Arc::new(Recorder(std::sync::Mutex::new((0, 0))));
    ingest_directory(&tmp, &altered, Arc::new(ZeroEmbedder), store.clone(), "probe", Some(rec3.clone()) as Option<Arc<dyn IngestObserver>>).await.unwrap();
    assert_eq!(rec3.0.lock().unwrap().1, 1, "version gate tripped once");
    assert_eq!(rec3.0.lock().unwrap().0, 0, "nothing skipped after a schema change");
}
```

Also add a deletion pin to the same test (or a sibling test): after the third ingest, delete `hello.rs`, ingest again with the same schema, and assert `store.get` for the generation-scoped file id of the newest generation returns `None` — a deleted file must not be resurrected by carry-forward.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib parser::ingest::tests::unchanged_files_skip_but_a_schema_change_forces_reparse -- --nocapture`
Expected: FAIL — `version_gate_tripped` method does not exist on `IngestObserver` (compile error).

- [ ] **Step 3: Implement the wiring**

In `src/parser/ingest.rs`:

1. Add to `IngestObserver` (near line 296), after `file_skipped`:
```rust
/// Called once per run when the previous generation's `ingest_version`
/// differs from the current one (or is absent). Implementors surface this
/// as a warning; the ingest continues with a full re-parse.
fn version_gate_tripped(&self, prev: &str, current: &str) {
    let _ = (prev, current);
}
```
2. Add `ingest_version: String` to the three producer item payloads (`NodeUpsert`, `ParsedSymbols`, `DocSections`) and stamp `metadata.insert("ingest_version", json!(ingest_version))` beside every existing `file_sha256` insertion.
3. In `ingest_directory`, while scanning the previous generation's rows (the `prev_file_map` loop near line 873), capture the first non-empty `metadata["ingest_version"]` as `prev_version: Option<String>`.
4. After the schema is available, compute `let current_version = ingest_version(&schema.fingerprint());` and `let force_reparse = prev_gen > 0 && version_gate_tripped(prev_version.as_deref(), &current_version);`. When `force_reparse && prev_gen > 0`, call `observer.version_gate_tripped(prev_version.as_deref().unwrap_or("(none)"), &current_version)` exactly once before the walk starts.
5. Pass `force_reparse` and `current_version` into the blocking walk closure. Change the skip guard at line ~479 to `if is_file && !force_reparse { ... }`, and thread `current_version` into every item the producer sends.
6. In `src/ingest_jobs.rs`, implement `version_gate_tripped` on `ProgressReporter` with `tracing::warn!(namespace = %self.namespace, prev, current, "ingest version changed; re-parsing all files")`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib parser::ingest::tests`
Expected: all pass, including the new test.

- [ ] **Step 5: Run the full gate**

Run: `cargo test --lib && cargo test --test version_gate_test && cargo test --test staged_publication_test && cargo clippy --all-targets -- -D warnings`
Expected: PASS, no clippy warnings.

- [ ] **Step 6: Commit**

```bash
git add src/parser/ingest.rs src/ingest_jobs.rs
git commit -m "feat(ingest): version-gated re-parse when parser schema changes"
```

---

## Self-Review

- **Spec coverage:** file-level hash reuse already exists (verified in `src/parser/ingest.rs`); the missing spec requirement — version-gated re-parse — is Task 1 (fingerprint/version helpers) + Task 2 (producer wiring), including stamping and warn-first reporting. Reuse of unchanged nodes/embeddings is preserved by the untouched carry-forward path; per-namespace isolation holds because the scan is already namespace + source scoped.
- **Type consistency:** `fingerprint(&self) -> String`, `ingest_version(&str) -> String`, `version_gate_tripped(Option<&str>, &str) -> bool`, and the `"ingest_version"` metadata key are used identically across tasks.
- **Proportion:** 2 tasks, no implementation transcripts; signatures and test assertions only.
