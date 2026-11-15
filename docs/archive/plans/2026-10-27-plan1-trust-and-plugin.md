# Plan 1: Trust Bugs and OpenCode Plugin Shim Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix supply-chain trust bugs (empty SHA-256 pins, fake Laya 0.75 score, broken checksum verifier) and make the OpenCode plugin shim build against `@opencode/plugin` 2.0.32, restoring the Zero-Action Start guard before writes.

**Architecture:** Keep the existing FastEmbedder/LayaOnnxProvider/model_downloader code paths but make them honest: real pins, fail-closed verification, explicit unavailable state. Rewrite the plugin entry point to the V2 `Plugin.define` API while preserving all wrapper and hook behavior.

**Tech Stack:** Rust (cargo test), TypeScript (tsc), `@opencode/plugin` 2.0.32.

**Spec:** docs/superpowers/specs/2026-10-27-neurostrata-feature-specs-design.md (§4 Bug B1, F4)

## Global Constraints

- No LLM in read path; deterministic only.
- Single store; no second database.
- Supply-chain pins must be real SHA-256 or absent with a logged warning; never empty-string hashes.
- `verify_model_checksum` must fail closed when a pin is set and no matching file exists.
- Laya must never report 0.75 unless a real ONNX inference produced it.
- Plugin must build with `npm run build` and load in OpenCode 2.0.32.
- TDD: write failing test first, then implement.

## Review Focus

- A tampered model file (wrong SHA) must cause `FastEmbedder::new` to return `Err`, not a warning.
- A missing model file with a pin set must return `Err`, not `Ok`.
- `LayaOnnxProvider::judge_duplicates` must never return `confidence: 0.75`.
- `ensure_ready` must not claim `Ready` without a real runtime.
- The plugin shim must export a default `Plugin` and type-check with `tsc`.

---

### Task 1: Pin Nomic embedder SHA-256 and make checksum verification fail-closed

**Files:**
- Modify: `src/embed.rs:54-72` (default model pins)
- Modify: `src/embed.rs:136-167` (verify_model_checksum)
- Test: `src/embed.rs` (tests module)

**Interfaces:**
- Produces: `verify_model_checksum(cache_dir: &PathBuf, model_name: &str, pinned_sha: &str) -> Result<()>` that returns `Err` on mismatch or missing file.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn verify_model_checksum_fails_on_mismatch() {
    let dir = std::env::temp_dir().join("ns-checksum-mismatch");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let model_path = dir.join("model.onnx");
    std::fs::write(&model_path, b"not-the-model").unwrap();
    let err = verify_model_checksum(&dir, "NomicEmbedTextV15", "deadbeef").unwrap_err();
    assert!(err.to_string().contains("checksum mismatch"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn verify_model_checksum_fails_when_no_file() {
    let dir = std::env::temp_dir().join("ns-checksum-missing");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let err = verify_model_checksum(&dir, "NomicEmbedTextV15", "deadbeef").unwrap_err();
    assert!(err.to_string().contains("model file not found"));
    let _ = std::fs::remove_dir_all(&dir);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib embed::tests::verify_model_checksum 2>&1 | tail -20`
Expected: FAIL (function currently returns Ok on missing file).

- [ ] **Step 3: Implement**

In `src/embed.rs`:
1. Set the Nomic pin to `147d5aa88c2101237358e17796cf3a227cead1ec304ec34b465bb08e9d952965`.
2. Leave BGE pin as `None` with a comment `// BGE pin pending a verified download; verification skipped when None.`
3. Rewrite `verify_model_checksum` to walk `cache_dir/models--{name}/snapshots/*/onnx/model.onnx` (and a flat fallback), compute SHA-256 of the first existing file, return `Err` on mismatch, and return `Err` with "model file not found" when no candidate exists.

- [ ] **Step 4: Run tests**

Run: `cargo test --lib embed 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/embed.rs
git commit -m "fix(embed): pin Nomic SHA-256 and fail-closed checksum verification"
```

---

### Task 2: Retire Laya fake 0.75 stub

**Files:**
- Modify: `src/judgment/laya_onnx.rs:94-121,185-190`
- Test: `src/judgment/laya_onnx.rs` (tests module or new tests)

**Interfaces:**
- Produces: `judge_duplicates` returns `Ok(vec![])` (no fake scores) or `Err` stating inference is unavailable; `ensure_ready` returns `RuntimeState::Unavailable`; `health_check` returns `Ok(false)`.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn laya_does_not_return_fake_confidence() {
    let provider = LayaOnnxProvider::load(
        std::path::Path::new("/tmp/nonexistent-bundle"),
        std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
    );
    // load may fail on missing bundle; if it succeeds, judge must not fake 0.75
    if let Ok(p) = provider {
        let res = p.judge_duplicates(&crate::traits::MemoryPayload::default(), &[]).await;
        match res {
            Ok(v) => assert!(v.is_empty(), "no fake 0.75 judgments allowed"),
            Err(e) => assert!(e.to_string().contains("not implemented") || e.to_string().contains("unavailable")),
        }
    }
}

#[tokio::test]
async fn laya_ensure_ready_reports_unavailable() {
    let mgr = LearningRuntimeManager { paths: LearningPaths { root: std::path::PathBuf::from("/tmp") } };
    assert_eq!(mgr.ensure_ready().await, RuntimeState::Unavailable);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib judgment::laya 2>&1 | tail -20`
Expected: FAIL (ensure_ready returns Ready; judge may return 0.75).

- [ ] **Step 3: Implement**

In `src/judgment/laya_onnx.rs`:
1. In `judge_duplicates`, remove the `confidence: 0.75` branch. If `model.onnx` exists, return `Err(anyhow!("Laya ONNX inference not implemented"))`. Otherwise `Ok(vec![])`.
2. `health_check` returns `Ok(false)`.
3. `ensure_ready` returns `RuntimeState::Unavailable`.

- [ ] **Step 4: Run tests**

Run: `cargo test --lib judgment 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/judgment/laya_onnx.rs
git commit -m "fix(laya): retire fake 0.75 duplicate confidence stub"
```

---

### Task 3: ModelRegistry multi-file bundle support (minimal)

**Files:**
- Modify: `src/model_downloader.rs:14-32` (ModelEntry + DEFAULT_REGISTRY)

**Interfaces:**
- Produces: `ModelEntry.files: Option<Vec<String>>` with serde default; registry JSON accepts the new field.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn model_entry_accepts_files_field() {
    let json = r#"{"name":"x","url":"u","sha256":"0","files":["a.onnx","b.json"]}"#;
    let e: ModelEntry = serde_json::from_str(json).unwrap();
    assert_eq!(e.files.unwrap(), vec!["a.onnx","b.json"]);
}

#[test]
fn model_entry_defaults_files_to_none() {
    let json = r#"{"name":"x","url":"u","sha256":"0"}"#;
    let e: ModelEntry = serde_json::from_str(json).unwrap();
    assert!(e.files.is_none());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib model_downloader 2>&1 | tail -20`
Expected: FAIL (unknown field `files`).

- [ ] **Step 3: Implement**

Add `#[serde(default)] pub files: Option<Vec<String>>` to `ModelEntry`. Update `DEFAULT_REGISTRY` to include `"files":null`. No download logic change in this plan.

- [ ] **Step 4: Run tests**

Run: `cargo test --lib model_downloader 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/model_downloader.rs
git commit -m "feat(model-downloader): add optional files list for multi-file bundles"
```

---

### Task 4: OpenCode plugin shim build (V2 API)

**Files:**
- Modify: `plugins/opencode/index.ts:1` (imports)
- Modify: `plugins/opencode/index.ts:479-500` (PluginContext type usage)
- Modify: `plugins/opencode/index.ts` (add default export)
- Modify: `plugins/opencode/package.json` (peer/dev deps)
- Test: `plugins/opencode` (npm run build)

**Interfaces:**
- Produces: default export `Plugin.define({ id, setup })`; `npm run build` succeeds.

- [ ] **Step 1: Update package.json**

Set `@opencode/plugin` to `^2.0.0` in both `peerDependencies` and `devDependencies`.

- [ ] **Step 2: Remove type-only imports**

Replace:
```ts
import type { ToolCall, ToolResult, PluginContext } from "@opencode/plugin";
```
with:
```ts
import { Plugin } from "@opencode/plugin";
```
Change `PluginContext`/`ToolCall`/`ToolResult` annotations to `any`.

- [ ] **Step 3: Add default export**

Append:
```ts
export default Plugin.define({
  id: "neurostrata",
  async setup(ctx: any) {
    await initialize(ctx);
    return {};
  },
});
```

- [ ] **Step 4: Build**

Run: `cd plugins/opencode && npm install && npm run build`
Expected: no type errors; `dist/index.js` and `dist/index.d.ts` produced.

- [ ] **Step 5: Commit**

```bash
git add plugins/opencode/index.ts plugins/opencode/package.json
git commit -m "fix(plugin): build shim against @opencode/plugin 2.0.32"
```

---

## Self-Review

1. Spec coverage: F4 and B1 covered; multi-file bundle field is minimal prep for F5.
2. Step scan: each task has failing test first, run, implement, run, commit.
3. Type consistency: `verify_model_checksum`, `judge_duplicates`, `ensure_ready`, default export all named consistently.
4. Review Focus: all five items have corresponding tests (checksum mismatch/missing, fake confidence, ensure_ready, plugin build).
5. Proportion: plan is shorter than spec; no full code bodies except signatures.

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-10-27-plan1-trust-and-plugin.md`. Please review the plan. Which execution approach would you prefer?

- **Subagent-driven** - fresh subagent per task with reviewer gate.
- **Native** - I implement every task in this session, then one reviewer.

For this plan I recommend **Native**, because the tasks are small, mostly independent, and the plan carries exact signatures and tests. Does the plan capture what you want, and which approach should we use?
