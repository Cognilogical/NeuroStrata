# Guinea-pig findings — keywest.health (2026-10-08)

Filed from the first real-project run of NeuroStrata as a project's primary memory and
task layer. The project is the guinea pig; these are what it turned up.

Scope note: these come from using the tooling as documented during a migration off
`beads` (`.beads/issues.jsonl`, 189 issues) onto NeuroStrata tasks. No source was read
to produce these — all are observed behaviour with repro steps.

Severity is my read, adjust as you see fit.

**STATUS UPDATE (later 2026-10-08):** All three data-safety items (BUG-1, BUG-2, BUG-3) are
**FIXED and verified live** in v1.7.0 (stomped): `neurostrata-mcp status` exists (exit 0
healthy / 1 down / 2 busy), a second daemon is refused at the lock with a legible message,
and `backup` is a checkpointed file snapshot that works with or without a daemon (round-trip
verified on an 8-namespace store) — the engine's `EXPORT DATABASE` turned out to SIGSEGV in
lbug's planner on *every* store, so the SQL export path is gone entirely. FEATURE-1 and
FEATURE-2 are also done (the extraction-lock error names both compliance paths; the
one-daemon-per-store rule is now in README and CLI-readme). Open: BUG-4 (`task_setup`
rule validation), BUG-5 (import via daemon — runbook: shutdown → import → auto-restart),
BUG-6/7 (`export-graph` fidelity — use `backup` snapshots as the safety copy instead),
FEATURE-3 (document `task_validate` categories). Tracked as NeuroStrata tasks.

---

## BUG-1 — `daemon` starts on an already-in-use store and SIGSEGVs (HIGH)

**Expected:** starting a second daemon against a store that a live daemon already holds
fails cleanly — e.g. `daemon already running on 127.0.0.1:34343 (pid N)` and a non-zero
exit that is not a signal.

**Actual:** SIGSEGV. Exit 139, no message.

**Repro:**
```
neurostrata-mcp daemon        # terminal 1, leaves daemon listening on 34343
neurostrata-mcp daemon        # terminal 2
# -> "Segmentation fault (core dumped)", exit 139
```
The first daemon logs a clean startup (`NeuroStrata Daemon listening on 127.0.0.1:34343`)
and continues running, so the crash is in the second process only.

**Why it matters more than a normal crash:** NeuroStrata's model is one daemon and many
consoles sharing it. A single-daemon invariant with multi-client shared access has no
guard, and the failure mode for violating it is memory unsafety, not a refusal. An agent
encountering a dead MCP connection has a strong incentive to "just start one" — which is
exactly the action that risks the store. A clean refusal removes the incentive entirely.

**Suggestion:** acquire an exclusive lock on the store (or on a pidfile) at daemon startup
and refuse if held. This is the highest-value fix in this document — it is the one that
can cost a user their data.

---

## BUG-2 — no `status` subcommand, so an agent cannot ask "is the daemon up?" (HIGH)

**Expected:** `neurostrata-mcp status` (or `ping`, `info`) reports whether the daemon is
listening, its pid, and the store path.

**Actual:**
```
$ neurostrata-mcp status
error: unrecognized subcommand 'status'
```

**Why it matters:** the only way to learn the daemon is down is to have a call fail. There
is no read-only preflight. Combined with BUG-1 this is what produces the bad outcome — an
agent cannot check first, so it guesses, and the guess that "works" is the destructive one.

**Suggestion:** add `status`. Cheap, and it makes BUG-1 much harder to hit.

---

## BUG-3 — `backup` SIGSEGVs when the daemon is not running (HIGH)

**Expected:** `neurostrata-mcp backup <dir>` with no daemon exits non-zero with
`daemon not running` (or starts a temporary read-only handle and backs up anyway).

**Actual:** SIGSEGV. Exit 139, **no output at all**.

**Repro:**
```
neurostrata-mcp shutdown
neurostrata-mcp backup /tmp/ns-test
# -> exit 139, no stdout, no stderr
```
With the daemon running the same command exits 1 with a legible error instead. So the
crash is specific to the no-daemon path.

**Why it matters:** `backup` is the documented safety step before anything that rewrites
ids. A tool whose safety rail segfaults at exactly the moment you need it is worse than no
rail — and in our case it aborted a migration with a stack trace where a one-line
"daemon not running" would have been actionable.

**Suggestion:** same guard as BUG-3 requires — check the connection first and return a
typed error.

---

## BUG-4 — `task_setup` proposes rules that contradict the project's standing rules (HIGH)

`task_setup` inferred `languages: ["javascript"]` from the presence of `package.json` and
proposed:

> "Node project: install from the lockfile and keep the package scripts (test, lint)
> green before pushing."

The project is **185 Go files to 72 JS**, and it carries an explicit standing rule
recorded in the project's own rule register: *no Python / Go only for anything that runs
against, serves, or mutates a live system.* The JS is tooling.

Accepting that suggestion would have injected a contradiction into the layer meant to be
the project's first-line truth — which is precisely the failure mode `supersede_memory`
exists to repair.

**Suggestion:**
1. Validate proposed rules against existing memories before proposing them; surface
   conflicts as conflicts rather than as a flat suggestion list (`conflicts: []` was empty
   here, so the mechanism may exist but did not fire).
2. Detection heuristics should weigh the dominant language, not the presence of one
   manifest.
3. Proposed rules should be marked as *heuristic, unverified* so a caller cannot mistake
   them for ground truth.

---

## BUG-5 — the documented migration path is blocked by the daemon that documents it (MEDIUM)

`task_setup` returns as its own step 2:

```
neurostrata-mcp task import keywest.health --from-beads .beads/issues.jsonl
```

Running it:

```
CRITICAL ERROR: The NeuroStrata daemon is currently running and holds the database lock.
You cannot run database-modifying CLI commands while the daemon is active.
Run `neurostrata-mcp shutdown` to stop it safely.
```

So the tool's own instruction requires tearing down the shared daemon that every console
is connected to. In a multi-console setup that is not a safe instruction to follow — which
is how a user ends up looking for another way to force it (see BUG-1).

**Suggestion:** either route `task import` through the running daemon, or make the
migration a daemon-side operation. If the shutdown really is required, `task_setup`
should say so and explain the blast radius instead of handing over a command that cannot
run.

---

## BUG-6 — `export-graph` drops `metadata` and `related_to` (MEDIUM)

`export-graph` writes only `absolute_path, content, domain, id, location, memory_type,
namespace` per node. It does **not** carry `metadata` or `related_to`.

Those are the knowledge-graph edges — the `Governs`, `contained_by` and `related_to`
relationships that make this a graph rather than a text dump. The portable export is what
a project treats as its backup, so the export is silently losing the structure it exists
to preserve.

We worked around it by hand-writing `docs/neurostrata/graph-edges.json` in the consuming
project, which is obviously not sustainable.

**Suggestion:** include `metadata` and `related_to` in the export. If size is the concern,
gate them behind a flag — but the default export should not be lossy about the graph.

---

## BUG-7 — `export-graph` exports superseded memories with no marker or filter (MEDIUM)

Superseded memories are correctly hidden from `search_memory`, but `export-graph` emits
them alongside live ones with no flag and no way to exclude them. Our portable backup was
carrying two contradictory memories with nothing indicating which was current — *"Pilot =
polished British English accent"* next to *"Australian… NOT British."*

We now maintain a hand-written `superseded.txt` id list and drop those rows after export.

**Suggestion:** mark superseded nodes in the export (`superseded: true`, `superseded_by:
id`) and/or add `--exclude-superseded`. The live layer already has the concept; the
artifact should too.

---

## FEATURE-1 — `task_complete`'s memory-extraction lock is excellent; make it visible (LOW)

`task_complete` refuses to finish until at least one memory has been extracted from the
task. That is a genuinely good piece of design — it turns "every close carries the
learning" from a convention into a machine.

Worth surfacing in the docs and in the error message when it refuses, because it is the
feature most likely to change how a team works and it is currently discoverable only by
hitting it.

---

## FEATURE-2 — document the shared-daemon invariant explicitly (MEDIUM)

The core operating rule — **one daemon per store; every console shares it; never spawn
your own** — is not stated where a new integrator will find it. It is inferable from
`shutdown`'s error text after you have already gone wrong.

Given BUG-1 makes the wrong action catastrophic, this belongs up front: README, and in
whatever an agent reads first. A short *"if the MCP connection fails, do not start a
daemon — check with `status`"* would have prevented this entire incident.

---

## FEATURE-3 — `task_validate` reports `{ok, violations, stale, unextracted_done, counts}` (LOW)

The shape is good. Worth documenting what qualifies as a `violation` versus `stale` versus
`unextracted_done` — at the moment the categories are only distinguishable by triggering
them. That also makes it possible to integrate against without guessing.

---

## Summary

| # | Issue | Severity | Theme |
|---|---|---|---|
| BUG-1 | second `daemon` on live store SIGSEGVs instead of refusing | High | data safety |
| BUG-2 | no `status` preflight | High | data safety |
| BUG-3 | `backup` SIGSEGVs with no daemon | High | data safety |
| BUG-4 | `task_setup` suggests a rule contradicting the project's rules | High | trust of generated content |
| BUG-5 | migration command blocked by the daemon that emits it | Medium | migration |
| BUG-6 | `export-graph` drops `metadata` / `related_to` | Medium | export fidelity |
| BUG-7 | `export-graph` leaks superseded memories unmarked | Medium | export fidelity |
| FEATURE-1 | surface the `task_complete` memory lock | Low | docs |
| FEATURE-2 | document the single-daemon shared-store invariant | Medium | docs |
| FEATURE-3 | document `task_validate`'s categories | Low | docs |

**The one to fix first is BUG-1.** The others cost time or fidelity; BUG-1 can cost a
user their database, and BUG-2 is what makes it likely to be hit. They share a root
cause: the daemon boundary is not enforced, and there is no way to ask about it.
