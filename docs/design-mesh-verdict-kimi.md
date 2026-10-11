# Mesh Design — Independent Verdict (Kimi K3, second architect seat)

Review of Concept v0 ("Approach C": git + hybrid snapshot/event tail + Thalamic Bus tee).
Verified against repo: bus event types (`memory_added`, `memory_superseded`, …) exist in
`src/server.rs`; Lock 2 extraction enforcement exists (`src/task/*`); procedure counters
(`remaining_fires`, `performance_count`, `last_performed_at`) are real mutable row state
stamped by `procedure_perform`. No prior mesh doc exists.

---

## 1. The four questions

### Q1 — Tasks in the mesh? **No. Exclude from v1; design the schema so they can be added later.**

Tasks are order-sensitive mutable state (open → in_progress → blocked → done) with
*exclusive claim semantics* backed by session liveness (60-min heartbeat window). Claims
are meaningless under eventual consistency: two offline nodes both claim the same task and
the merge has no correct answer; a stale remote claim would phantom-lock local work. Status
churn is high-frequency, low-durability — exactly what a git ledger is worst at. The durable
cross-machine artifact of a task is the memory Lock 2 forces out of it, and **that already
flows through the mesh**. If shared-backlog visibility is wanted later, ship a read-only
export projection (id, title, status, blocked_by — never claims, never session ids), or an
explicit `shared: true` opt-in flag. Red line below: claim/heartbeat state never leaves
the node, in any version.

### Q2 — Conflict resolution: content-hash dedup is right for adds, but insufficient alone; fix the ordering primitive, skip vector clocks.

Content-hash dedup handles the easy case (same content, different ids). It does **not**
handle the hard case: node A supersedes X→Y while offline node B supersedes X→Z. Y≠Z, so
dedup is silent and you need a lifecycle-conflict policy. Vector clocks only *detect* that
concurrency — you still need the resolution policy, so they buy little at human-scale write
rates. Recommendation: keep content-hash dedup for creation; give every event a
`(created_at, origin_node_id, event_id)` triple and resolve lifecycle conflicts by
deterministic LWW on that triple (node_id tiebreak kills clock-skew nondeterminism);
**surface every resolved conflict in a `mesh status` report** — silent resolution is how
trust in the ledger dies. A Lamport counter per node is a cheap upgrade if you want
causal-ish ordering without vector-clock storage. Accept semantic near-duplicates
(differently-worded same-rule memories) as human-reconciled; no algorithm fixes those cheaply.

### Q3 — Push trigger: separate commit from push. Auto-commit locally; push piggybacked + explicit. Never silent-timer push.

Local auto-commit on fold is fine — commits are cheap and keep the working tree clean for
any dirty-check. **Push** is the friction point: (a) explicit `neurostrata-mcp mesh push`,
(b) piggyback on the user's own `git push` of the project via hook, (c) opportunistic push
at session end. All pushes must be non-interactive (`GIT_TERMINAL_PROMPT=0`) and fail-soft
into a pending-push queue — an agent blocked on a credential prompt is a wedged session.
Pull-before-push (`fetch` + rebase + replay) is mandatory, not optional. Auto-push on every
fold is commit spam and surprising network side-effects; hook-only will be forgotten within
a week.

### Q4 — Per-namespace mesh dir. **Yes, and gate it per-namespace.**

Costs nothing today, avoids a migration later, and namespace is the correct unit of sharing
consent. Concrete beneficiary the proposal missed: the **`global` namespace exists today**
and must default to *not meshed* — it holds personal/machine-global rules. Per-namespace
layout with an explicit opt-in flag per namespace is the right shape.

---

## 2. Things missed, ranked by severity

1. **CRITICAL — snapshot.json is a merge-conflict magnet.** Every node folds its tail into
   the *same single JSON file* on a 5s debounce. With ≥2 active writers, concurrent folds
   guarantee non-fast-forward pushes and textual merge conflicts on a machine-generated
   file no human can sanely hand-merge. This fires *daily* at 10 members, not at scale.
   events.jsonl appends merge acceptably (union driver); the snapshot is the problem.
   This is the design's actual breaking point — see §3.
2. **HIGH — mesh must not live on project feature branches.** If `mesh/` is tracked in the
   project's own repo, every feature branch forks memory state and every code merge becomes
   a memory merge. Learned rules are branch-orthogonal. The mesh dir must be its **own git
   repository** (nested under `.NeuroStrata/`, gitignored by the parent) or a dedicated
   orphan branch. The proposal is ambiguous on this and the wrong reading is catastrophic.
3. **HIGH — procedure performance events are not idempotent.** `procedure_perform`
   decrements `remaining_fires` and bumps counters (verified in `src/server.rs`). Replayed
   twice → double-decrement; performed offline on two nodes → counter drift no dedup-by-id
   can fix. Mesh the procedure **definition + lifecycle only** (created/superseded/
   archived); keep `remaining_fires`/`performance_count`/`last_performed_at` strictly
   per-node. Rehearsal state is local state, like claims.
4. **HIGH — deletion/right-to-be-forgotten.** Git never forgets: `archive_memory` removes
   from the working set but the content lives in ledger history and every teammate's clone.
   Required: tombstone semantics in live state, a documented purge procedure
   (filter-repo / mesh re-init), and explicit user-facing warning that mesh = permanent
   record shared with the team.
5. **MEDIUM — identity/attribution.** Events are unsigned and forgeable by anyone with push
   access. Acceptable trust model for v1 is "push access = full trust" (same as code), but
   every event must carry an author field (node_id ↔ git user.name/email) and the design
   doc must state the model plainly. Optional hardening: signed commits on the mesh repo.
   Do not build a signature layer into the event format — redundant with git.
6. **MEDIUM — replay robustness at age.** events.jsonl lives for years: schema_version per
   event, forward-compatible replay (unknown types → warn + skip, never crash), idempotent
   supersede/archive (re-superseding an already-superseded record is a no-op, not an error),
   and a replayed-event-id set for exactly-once application.
7. **MEDIUM — pre-commit "refuse if snapshot dirty" is user-hostile.** Blocking code commits
   over memory-state coherence will train users to bypass hooks. Auto-fold on commit
   instead of refusing — or eliminate entirely by making mesh a separate repo (§2.2), which
   dissolves the dirty-check problem.
8. **LOW — scale is fine.** 10 members × 1000 memories ≈ 10MB text. Git shrugs. The
   contention issue is fold races (§2.1), not data size.
9. **LOW — clock skew** on `created_at` ordering; handled by the Q2 tiebreak.

---

## 3. Alternative designs — **Approach D (per-record files) is materially better. Recommend switching.**

```
<project>/.NeuroStrata/mesh/<namespace>/     # own git repo, parent-gitignored
  ├── memories/<id>.json                      # one file per memory, full record
  └── procedures/<id>.json                    # definition + lifecycle only, no counters
```

The directory tree **is** the snapshot; `git log` **is** the event log. No fold, no
compaction, no dual-file coherence window, no debounce.

Why it beats C on every pressure point identified above:

- **Merge conflicts (§2.1):** concurrent *different*-memory writes touch different files →
  zero conflicts, ever. Conflicts occur only on same-record concurrent edits — which are
  genuine semantic conflicts that deserve deterministic LWW resolution + report (Q2
  machinery applies per-file unchanged).
- **Compaction (gone):** no tail, nothing to fold, nothing to truncate.
- **Attribution:** `git log/blame` per file gives authorship and full history free.
- **Deletion:** file removal is a first-class git operation; targeted purge via
  `filter-repo --path` is file-scoped instead of surgery on a monolithic log.
- **Pull/replay:** `fetch && merge` (fast-forward almost always), then
  `git diff --name-only <old>..HEAD` → upsert changed records into LadybugDB. Idempotent by
  construction — re-upserting a record is a no-op, which *also* fixes the replay-exactly-once
  burden (§2.6 mostly evaporates).
- **Offline (§open call):** two agents work offline, both commit locally, merge later with
  git's per-file semantics — the gold-standard offline story, stronger than C's.

Costs, honestly: no ordered global event stream (the Thalamic Bus tee writes a file instead
of appending a line — a wash); ~10k small files at scale (trivial on local disk; shard by id
prefix only if it ever matters); same-record conflicts still need the LWW resolver.

If event-sourcing purity is non-negotiable, the acceptable fallback is **C′**: git tracks
*only* events.jsonl (union merge driver via `.gitattributes`) with periodic inline
checkpoint records; snapshot.json becomes a **local derived cache, gitignored**, rebuilt on
pull. That fixes §2.1 while keeping the log. C-as-proposed (both files tracked, snapshot
rewritten by every node) is the worst of the three and I would not ship it.

**Ranking: D > C′ ≫ C.** Confidence: high on the merge-conflict analysis; moderate on D's
operational feel since no prototype exists.

---

## 4. Red lines

1. **Mesh state must never follow project feature branches.** Separate repo or dedicated
   orphan branch. Non-negotiable.
2. **Claim/heartbeat/session state never leaves the node** — in v1 (no tasks) or any future
   shared-task design. Phantom cross-machine locks are worse than no shared tasks.
3. **No blocking network operations in the mutation path.** Every git network call
   non-interactive, fail-soft, queued. An agent must never hang on a credential prompt.
4. **No counter-mutating events in the mesh without exactly-once replay.** In practice:
   procedure performance state stays local (§2.3).
5. **No silent conflict resolution that can drop a teammate's memory without a trace.**
   Deterministic rule + recorded, inspectable decision log, or don't ship.

## Summary

C's skeleton (git transport, no real-time, bus-tee ingestion, local embeddings, stable
UUIDs, tasks excluded) is sound. Its storage layout is not: the tracked, multiply-folded
snapshot.json will produce daily merge conflicts with two or more active writers. Switch to
per-record files (D) — same philosophy, same components, one fewer moving part, and git's
content-addressable merge machinery does the job the snapshot was fighting to do.
