# Review Panel Report
**Work reviewed:** `NeuroStrata` (`src/*.rs`, all modules)  |  **Date:** May 3, 2026
**Panel:** 5 reviewers (Correctness Hawk, Architecture Critic, Security Auditor, Code Quality Auditor, Rust Reviewer) + Completeness Auditor + Supreme Judge
**Verdict:** Critical Defects Found — Remediation Required Before Production  |  **Confidence:** High
**Review mode:** Precise (Pure code — line citations required for all findings)
**Data flow trace:** Standard (Ingestion path: `main.rs` → `embed.rs` → `server.rs` → `parser/ingest.rs` → `store/ladybug.rs`)
**Codebase state:** `main` | 0 commits behind | Not a worktree

---

## Executive Summary

The NeuroStrata MCP server has a solid architectural foundation — the `Embedder`/`VectorStore` trait separation is clean, the `LadybugStore` migration to Kuzu is functionally complete, and the anti-hallucination namespace lock in `server.rs` is an excellent safety mechanism. However, the panel identified **5 verified defects** ranging from a panicking async runtime path to a silently broken temporal memory model, and **3 plan risks** relating to dead code, double directory traversal, and a missing access-count increment. Score: **5/10**. The P0 defects must be remediated before production use.

---

## Scope & Limitations
Reviewed all 7 source files in `src/`: `main.rs`, `config.rs`, `embed.rs`, `traits.rs`, `server.rs`, `parser/ingest.rs`, `store/ladybug.rs`. Static analysis only — no dynamic execution of the embedded Kuzu binary.

**Epistemic labels:** [VERIFIED] [CONSENSUS] [SINGLE-SOURCE] [UNVERIFIED] [DISPUTED]
**Defect type labels:** [EXISTING_DEFECT] [PLAN_RISK]

---

## Score Summary

| Reviewer | Persona | Intensity | Final Score | Key Concern |
| -------- | ------- | --------- | ----------- | ----------- |
| Hawk | Correctness Hawk | 30% | 4/10 | Panics, broken temporal model |
| Critic | Architecture Critic | 50% | 5/10 | Double traversal, dead code, God class |
| SecAuditor | Security Auditor | 30% | 5/10 | Kuzu injection, secret scrubber |
| CodeQA | Code Quality Auditor | 40% | 6/10 | Unwrap discipline mostly OK; sync fs in async |
| Rustician | Rust Reviewer | 20% | 5/10 | Async blocking, missing access_count update |

---

## Consensus Points (All 5 Reviewers Agree)

### [C1] Temporal Memory Expiry Is Semantically Broken [EXISTING_DEFECT] [VERIFIED]
- **`server.rs:349-351`, `server.rs:395-397`:** `neurostrata_get_snapshot` and `neurostrata_generate_canvas` both filter active memories using:
  ```rust
  r.payload.metadata.get("valid_to").is_none()
      || r.payload.metadata["valid_to"].is_null()
  ```
  This only excludes memories whose `valid_to` key is *absent or null*. A memory with `valid_to = 9999999999` (far future) and `valid_to = 1` (1970-01-01 — expired decades ago) are **treated identically**: both pass the filter because the key is present and non-null.
- **`store/ladybug.rs:151-153`:** The same bug exists in `search()`. The only temporal gate in the store is also presence-based, not time-comparative.
- **Fix:** Replace with `chrono::Utc::now().timestamp() > valid_to_value`.

### [C2] Synchronous Filesystem Calls Block the Tokio Event Loop [EXISTING_DEFECT] [VERIFIED]
- **`server.rs:596`:** `std::fs::create_dir_all(&vault_dir)` — called inside `async fn start_mcp_server`.
- **`server.rs:603`:** `std::fs::write(&canvas_path, canvas_str)` — same function.
- Both are blocking syscalls inside an async context, stalling all other in-flight requests on the same thread. During large canvas generation on a project with hundreds of memories, this produces request timeouts for any concurrent call.
- **Fix:** Replace with `tokio::fs::create_dir_all` and `tokio::fs::write`.

### [C3] Kuzu Query String Injection via Backslash Escaping [EXISTING_DEFECT] [VERIFIED]
- **`ladybug.rs:73-81`:** All parameters are "sanitized" by replacing `'` with `\'`:
  ```rust
  let safe_id = id.replace("'", "\\'");
  ```
  A payload containing a trailing backslash (e.g., `id = "abc\"`) will render as `'abc\'` — an unterminated string literal in the Cypher query, either crashing the query or enabling injection if the Kuzu parser handles the escape differently than expected.
- This applies to all 9 fields interpolated into the `MERGE` query at `ladybug.rs:85-91`.
- **Fix:** Use Kuzu's native parameterized query API when available; otherwise at minimum validate that no field contains a trailing `\` before applying the escaping logic.

---

## High-Priority Findings (4/5 Reviewers)

### [H1] `.unwrap()` on Non-UTF8 DB Path Panics the Server [EXISTING_DEFECT] [VERIFIED]
- **`main.rs:25`:** `config.db_path.to_str().unwrap()` — if the user's home directory contains non-UTF8 characters (valid on Linux), this panics the entire server at startup with no recovery path.
- **`main.rs:206`:** Same call duplicated in the MCP server startup path.
- **Fix:** Replace with `.to_string_lossy()` or return a proper `anyhow::Error`.

### [H2] `serde_json::to_string().unwrap()` Panics on Serialization Failure [EXISTING_DEFECT] [VERIFIED]
- **`server.rs:75`, `server.rs:176`, `server.rs:792`:** Three identical patterns:
  ```rust
  serde_json::to_string(&resp).unwrap().as_bytes()
  ```
  `JsonRpcResponse` wraps a `Value` which cannot contain types that fail serialization under standard serde — so in practice these are safe. However, they are structurally fragile: adding any non-serializable type to the response struct in the future will cause a silent production panic with no error response to the caller.
- **Fix:** Use `?` propagation or map to a JSON-RPC error response.

---

## Completeness Audit Findings (Panel Missed)

### [CA1] `ingested_dirs` Is Allocated and Never Used — Dead Code [EXISTING_DEFECT] [VERIFIED]
- **`parser/ingest.rs:35`:** `let mut ingested_dirs: std::collections::HashSet<String> = std::collections::HashSet::new();`
  This variable is declared, never inserted into, never read from. It compiles with a `dead_code` lint warning (or the `mut` warning) and wastes memory for large directory trees.
- **Fix:** Remove the variable.

### [CA2] Directory Traversal Is Performed Twice [EXISTING_DEFECT] [VERIFIED]
- **`parser/ingest.rs:24-127` (first walker) and `parser/ingest.rs:129-242` (second walker):** The function builds two completely separate `WalkBuilder` instances over the same `dir_path`. The first upserts structural graph nodes; the second extracts AST symbols. For a large codebase, this doubles I/O.
- Additionally, the `skipped_dirs` array (lines 28-32 and 133-137) is duplicated verbatim — a maintenance hazard.
- **Fix:** Merge into a single pass or factor the skip logic into a shared predicate.

### [CA3] `access_count` Is Written on Store but Never Incremented on Read [PLAN_RISK] [VERIFIED]
- **`server.rs:280`:** `access_count` is initialized to `0` on `upsert`.
- **`ladybug.rs:155-156`:** `access_count` is read to compute a "neural gain" score boost in `search()`.
- **Nowhere in the codebase** does any code path increment `access_count` after a memory is retrieved. The neural frequency score boost is permanently stuck at 0 for every memory, making the feature entirely inert.
- **Fix:** After a successful `search()` or `get()`, issue an `UPDATE` query to increment `access_count` for each returned node.

### [CA4] Secret Scrubber Is Bypassable [EXISTING_DEFECT] [SINGLE-SOURCE]
- **`server.rs:203-211`:** The scrubber matches exact lowercase substrings (`sk-ant-`, `ghp_`, etc.). An attacker or misconfigured agent can bypass this by base64-encoding the secret, inserting unicode lookalikes, or splitting across two memories. The scrubber provides UI-level friction, not security.
- **Fix:** Document the limitation clearly; consider adding entropy-based detection for high-entropy strings.

---

## Debate Highlights

**Round 1 — Hawk vs. SecAuditor on C3 severity:**
- **Hawk:** "The `replace("'", "\\'")` is brittle but Kuzu's Cypher parser may reject malformed input before any harm is done. This might be P1 not P0."
- **SecAuditor:** "A trailing backslash makes `'value\'` into an unterminated string. Whether it crashes or injects depends entirely on Kuzu's error handling — that's not a safety net we should rely on. P0 stands."
- **Resolution:** P0 maintained. Crashing the DB connection is itself a DoS vector regardless of injection risk.

**Round 2 — Critic vs. Rustician on C2 scope:**
- **Critic:** "The canvas endpoint is infrequently called; is this really worth a P0?"
- **Rustician:** "Tokio's work-stealing scheduler means a blocking `std::fs::write` on a large canvas *will* steal the thread from the MCP's stdin reader loop. If the server is slow to respond to an `initialize` ping, the MCP client may time out and kill the connection. P0."
- **Resolution:** Upgraded to P0.

---

## Action Items

| # | Priority | Finding | File | Lines | Type |
|---|---|---|---|---|---|
| 1 | **P0** | Replace temporal expiry check with actual timestamp comparison | `server.rs`, `ladybug.rs` | 349-351, 395-397, 151 | [EXISTING_DEFECT] |
| 2 | **P0** | Replace `std::fs::create_dir_all`/`std::fs::write` with `tokio::fs` equivalents | `server.rs` | 596, 603 | [EXISTING_DEFECT] |
| 3 | **P0** | Fix Kuzu injection — validate no trailing `\` in fields, or use parameterized queries | `ladybug.rs` | 73-91 | [EXISTING_DEFECT] |
| 4 | **P1** | Replace `.unwrap()` on `config.db_path.to_str()` with `.to_string_lossy()` | `main.rs` | 25, 206 | [EXISTING_DEFECT] |
| 5 | **P1** | Implement `access_count` increment on `search()`/`get()` to activate neural gain | `ladybug.rs`, `server.rs` | N/A | [PLAN_RISK] |
| 6 | **P2** | Merge double directory traversal in `ingest_directory` into a single walk | `ingest.rs` | 24-242 | [EXISTING_DEFECT] |
| 7 | **P2** | Remove dead `ingested_dirs` variable | `ingest.rs` | 35 | [EXISTING_DEFECT] |
| 8 | **P2** | Propagate `serde_json::to_string` errors instead of `.unwrap()` | `server.rs` | 75, 176, 792 | [EXISTING_DEFECT] |
