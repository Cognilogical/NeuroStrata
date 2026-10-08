# Verdict: Dedicated Memory Writer/Reader Agents

**Status:** Rejected as proposed. Deterministic subset adopted instead.
**Date:** 2026-10-08
**Scope:** Proposal to add two background LLM agents (memory writer, memory reader) to make memory operations transparent to the main agent.

---

## 1. Verdict

**REJECT** the two-agent LLM architecture. **ADOPT** the deterministic middle ground: event-driven retrieval prefetch over existing graph edges, plus ergonomic improvements to the explicit write path the main agent already owns.

## 2. Rationale (laws of physics)

### 2.1 The write judgment is irreducible and already optimally placed

Deciding "is this fact worth persisting?" requires two inputs:

1. The full conversational/situational context (why a fact matters, whether it's ephemeral).
2. Knowledge of what is already stored.

The main agent is the **only** component that already holds input (1) at zero marginal cost. A background writer agent must re-derive that context. There are exactly two options, both dominated:

- **Full transcript access** → the writer re-processes every token the main model processed. Cost ≈ 1 extra full-model pass per turn. For a solo developer this roughly doubles token spend to hide one `add_memory` call (~50 tokens when it fires, and it fires rarely).
- **Summary/lossy access** → the writer judges with degraded context → low-precision writes → retrieval pollution for every future session. False-positive writes compound (they enter snapshots, RRF results, neural-gain rankings); false negatives cost one missed fact. An autonomous writer tuned for recall monotonically degrades the DB.

You cannot compress the main agent's situational awareness into a side channel without loss. The judgment complexity is not removed by moving it — it is duplicated, with worse inputs. **This is irreducible complexity; the proposal adds an agent to hide a tool call.**

Evidence the current design already adjudicated this: `.agents/skills/neurostrata/SKILL.md` states *"Instead of relying on a background process, YOU are responsible for continuously monitoring the chat stream for the following 8 categories of structural facts."* Write triggers are already event-based (bead close → `add_memory`, pre-push hook, the "breath" prompt for Tier-3 commits).

### 2.2 The reader half is already built — deterministically, at the cost floor

Verified in-repo:

- `neurostrata_get_snapshot` — pre-computed top-N weighted active rules per namespace (zero-shot grounding, `src/store`, documented in `docs/COGNITIVE_ARCHITECTURE.md` §5).
- `neurostrata_search_memory` — hybrid dense-vector + BM25 (tantivy) merged via Reciprocal Rank Fusion, with neural-gain reweighting (§3, §6).
- Embeddings are **local** (`fastembed` in `src/embed.rs`) — a retrieval query costs milliseconds and $0.00.
- Domain isolation shrinks the search space (§4).

The irreducible complexity of retrieval is: embed query → rank → filter. The system is already at that floor. An LLM "reader agent" deciding what to inject inserts a paid, latent, fallible judgment layer on top of a free, deterministic, auditable one. Its only plausible value-add (reranking) is unproven to beat RRF + neural gain on this corpus size, and its failure mode — auto-injecting a stale rule prefixed `[🛑 CRITICAL PROJECT RULE]`, which agents are instructed to follow absolutely — is strictly worse than a missed injection.

### 2.3 Security regression (Guarded Curation)

`README.md` documents Guarded Curation as the LLM08 mitigation: no MCP tool destroys memory; corrections go through `supersede`. The write path's integrity rests on the main agent being a thinking gatekeeper. An autonomous writer that ingests file contents and transcripts creates a **persistent prompt-injection write-path**: malicious text in any read artifact becomes a durable `RULE:` memory that outlives the session and is auto-retrieved into future contexts. That is a materially worse attack surface than explicit writes.

### 2.4 Auditability

Explicit `add_memory` calls appear in the transcript with the agent's stated reason. Background writes are invisible at the point of decision; debugging "why does the DB believe X?" becomes archaeology across two agents' logs. The existing bi-temporal audit trail helps, but it records *what* changed, not the *reasoning context* — which today lives in the main transcript for free.

### 2.5 Cost/latency budget (solo developer constraint)

| Component | Marginal cost/turn |
|---|---|
| Current: explicit writes | ~0 (fires on events, ~1 tool call when salient) |
| Current: deterministic retrieval | ~ms, $0 (local fastembed) |
| Proposed writer agent | ≥1 full-model pass over transcript (≈2× session token spend) |
| Proposed reader agent | +1 LLM call/turn + injected tokens consumed every subsequent turn |

The proposal fails the cost-effectiveness constraint outright.

## 3. Adopted middle ground (deterministic subset)

The legitimate pain point is **cognitive load and missed retrievals**, not write automation. Address it without an LLM in the loop:

### R1. Event-driven retrieval prefetch (replaces "reader agent")

Use the graph edges that already exist (`locations`, `governs`, `related_to` — see metadata usage in `src/secrets.rs`, `src/parser/ingest.rs`):

- **Session start:** `get_snapshot` (already shipped). Client shims (AGENTS.md / hook) should make it the mandatory first action — already the case in this repo's AGENTS.md.
- **File-touch trigger:** when the main agent's client reads/edits a file, a shim or MCP middleware issues a deterministic query: active memories whose `governs`/`locations` edges match the path (exact + directory-prefix), union snapshot for the namespace. Inject as a compact, capped block (≤512 tokens, top-K=5, deduped against what is already in context).
- **No LLM anywhere in this path.** Ranking = existing RRF + neural gain. Trigger = path match. Budget = hard token cap with eviction of stale injections.

Acceptance criteria: prefetch must never exceed cap; every injected memory carries its id; a config flag disables injection entirely.

### R2. Write-path ergonomics (replaces "writer agent")

Keep the main agent as the write filter; lower its cost:

- **Batch write tool** (`add_memories` accepting an array) so the bead-close/breath-prompt moments cost one call, not N.
- **Inline dedup feedback:** the existing `DeduplicationChecker` verdict (judgment provider, circuit-breaker protected, `src/judgment/`) already returns "potential duplicate" on write — keep it synchronous and human-readable so correction (`supersede`) is one step.
- **Dry-run flag** on `add_memory` returning the dedup verdict without persisting, for agents that want to check before writing.

### R3. Optional, off-by-default: LLM reranker

If future measurement shows retrieval precision (not recall) is the bottleneck, an LLM rerank of the top-K deterministic results may be added behind a config flag. Requires: offline eval harness showing ≥20% precision@5 gain over RRF+neural-gain on a labeled corpus, per-call circuit breaker, and a hard per-session call budget. Not scheduled.

## 4. Answers to the proposal's key questions

1. **Writer trigger:** Rejected at any cadence. Event-based explicit triggers (bead close, pre-push, breath prompt) already exist and are sufficient.
2. **Memory bloat:** Prevented by keeping the informed filter (main agent) + existing judgment-based dedup on write. An autonomous writer has no bloat-boundary that doesn't trade recall for pollution.
3. **Injection strategy:** Deterministic edge-traversal prefetch (R1), top-K=5, ≤512-token cap, session-start snapshot. No LLM.
4. **Black box:** Yes — rejected architecture would be materially harder to audit; adopted subset is fully deterministic and logged.
5. **Cost/latency:** Proposed ≈2× token spend + 2 LLM calls/turn. Adopted ≈ $0, ms-latency, one tool call on events.
6. **Conflict/correction:** Unchanged — `supersede_memory` (retire, never overwrite) remains the single correction path. Bad auto-writes would have been uncorrectable at scale; explicit writes keep the error rate low enough for one-at-a-time curation.
7. **Transparency vs control:** Explicit memory ops are not a bug — the write judgment IS agent reasoning. The fix for cognitive load is better triggers and batching (R2), not a second mind with worse context.

## 5. What would change this verdict

Revisit only if: (a) instrumented data shows the main agent misses ≥30% of high-value writes (measured by post-hoc session review), AND (b) a writer prototype on logged transcripts demonstrates precision ≥0.9 at fixed recall against human-labeled "worth storing" judgments, AND (c) per-turn cost fits a solo-dev budget. Until all three hold, the deterministic subset stands.
