# Verdict: Fold NeuroCortex into NeuroStrata (MERGE)

**Date:** 2026-10-08 · **Author:** claude-fable co-architect · **Status:** proposal

## Verdict

**MERGE.** Port NeuroCortex (1,062 LOC, verified) into NeuroStrata as a `src/guard/` module
behind a cargo feature flag. Retire the standalone `neurocortex` binary, LanceDB, and
llama-cpp-2. Ship one MCP server exposing 7 memory tools + 3 guard tools.

This is not integration-at-MCP-level (that preserves every cost and adds none of the
benefits) and not status quo (the split already failed: the two systems are covertly
coupled — see Evidence §4).

## Evidence (verified against both repos)

1. **NeuroCortex is already a NeuroStrata parasite.** Its vector DB URI is literally
   `.neurostrata` (main.rs:27); its embedder config is `~/.config/neurostrata/embedders.json`
   (semantic.rs:28); its model cache is `~/.cache/neuro/models/fastembed` (semantic.rs:69).
   "Independent deployment" is fictional.
2. **Same embedding model, skewed versions.** Both default to NomicEmbedTextV15 768-dim.
   NeuroCortex pins fastembed 4; NeuroStrata runs fastembed 7. Two processes load the same
   ~100–200 MB ONNX model per agent session.
3. **Duplicated capabilities, worse in NeuroCortex:**
   - Secret scanning: NeuroCortex = 6 hardcoded prefixes (main.rs:68). NeuroStrata
     `secrets.rs` = compiled regex scanner with categories. Strict superset.
   - Semantic judgment: NeuroCortex = embedded llama-cpp-2 running a 7B Q4 GGUF
     (~4.5 GB download, heavy native build). NeuroStrata = `JudgmentProvider` trait
     (model-agnostic, circuit-broken, typesafe_jev provider live). Strict superset.
4. **The data is one domain.** `learn_behavioral_rule(rule_class, trigger_pattern,
   constraint_text)` is `add_memory` with metadata. AGENTS.md currently mandates dual-write
   (teach NeuroCortex AND NeuroStrata) — pure entropy. Meanwhile the guard validates against
   its tiny private rules table and **cannot see** the architectural rules in NeuroStrata
   that are the actual ground truth (pre-flight `get_snapshot` mandate). Merged, the guard
   queries the real corpus with one `search_memory`.
5. **Brief claims corrected:** NeuroCortex fails **closed**, not "fail-open" — semantic
   error → `DeterministicReject` (semantic.rs:357), churn limit → reject (main.rs:61),
   no container engine → reject (sandbox.rs:44). `neurocortex_think` discards the `thought`
   argument entirely (handler reads only step/total/is_correction) — it is a stub, ~10 LOC
   to port.
6. **Scale asymmetry:** NeuroCortex 1,062 LOC, hand-rolled stdio MCP, panics on DB-connect
   failure. NeuroStrata ~10k LOC, daemon + checkpointing + emergency cache. The guard
   inherits resilience by moving in, not by staying out.

## Why merge minimizes entropy

- **⊖ 1 binary, ⊖ 1 MCP registration, ⊖ 1 vector DB (LanceDB + arrow 57 tree),
  ⊖ 1 native LLM stack (llama-cpp-sys), ⊖ fastembed version skew, ⊖ dual-write mandate.**
- **⊕ guard queries ground-truth rules; ⊕ guard inherits daemon resilience +
  emergency cache; ⊕ capability-check matrix in INTEGRATION_NEUROCORTEX.md collapses
  (no more "if X present / if Y missing" agent branching).**

### Adversarial check (why not merge) — and rebuttals

| Risk | Rebuttal |
|---|---|
| Guard latency critical; daemon coupling could block it | Rule lookup via in-process read-through cache; daemon miss → stale cache (existing emergency_cache pattern). Guard hot path (secret scan, churn, sandbox) never touches daemon. |
| 7B LLM inference OOM could kill memory server | Kill llama-cpp-2 entirely. Intent eval → `JudgmentProvider` (external/circuit-broken). No in-process heavyweight inference added. |
| Podman sandbox orthogonal to memory | True, and harmless: 150 LOC shell-out module, no link-time coupling. Feature-gate it. |
| Migration cost | ~1k LOC source, most of it deletable (Arrow/LanceDB plumbing ≈ 230 LOC of semantic.rs). Est. 2–4 focused sessions. |

## Target design

```
src/guard/
  mod.rs        — tool registration: local_guard_validate, learn_behavioral_rule, guard_think
  validate.rs   — pipeline: churn limiter → secrets.rs scan → rule match → judgment → sandbox
  rules.rs      — BehavioralRule as namespaced memory type (`type: behavioral_rule`,
                  metadata: rule_class, trigger_pattern, hit_count, status) in LadybugDB
  cache.rs      — read-through rule cache, stale-fallback on daemon outage
  sandbox.rs    — Podman executor, ported verbatim (feature `guard-sandbox`)
Cargo.toml:     — feature `guard` (default on); DROP lancedb, arrow, llama-cpp-2, fastembed<7
```

**Tool renames:** keep `local_guard_validate` / `learn_behavioral_rule` wire names for
back-compat (AGENTS.md references them); rename `neurocortex_think` → `neurostrata_think`
with alias.

**Judgment wiring:** `evaluate_intent(payload, intent)` becomes a `JudgmentProvider` call
(`judge_action(payload, matched_rules) → {approved, reason}`). TypeSafe Jev provider first;
fail-closed verdict preserved when provider unavailable.

**Data migration:** one-shot `neurostrata-mcp migrate-neurocortex`:
read LanceDB `behavioral_rules` → re-embed (fastembed 7, same model → same vector space,
but re-embed anyway for provenance) → insert as `behavioral_rule` memories in `global`
namespace → write `.neurostrata/MIGRATED` tombstone. Table is tiny (agent-taught rules);
no online migration needed.

## Acceptance criteria

1. `neurostrata-mcp` stdio exposes all 10 tools; old NeuroCortex MCP configs work via
   symlink/shim binary that execs `neurostrata-mcp` (one release cycle), then removed.
2. `local_guard_validate` passes: secret reject, churn reject, sandbox dry-run reject,
   rule-constraint injection — with daemon UP and daemon DOWN (stale cache).
3. `search_memory` surfaces behavioral rules; guard match latency ≤ current LanceDB path.
4. Build graph: no lancedb/arrow/llama-cpp in `cargo tree`.
5. AGENTS.md dual-write mandate deleted; single `learn_behavioral_rule` write path.

## Revisit triggers (would argue to re-split)

- Guard needs sub-50 ms p99 under memory-ingest load (process isolation).
- A second, non-NeuroStrata consumer of the guard emerges (then extract as shared crate,
  not MCP server).
