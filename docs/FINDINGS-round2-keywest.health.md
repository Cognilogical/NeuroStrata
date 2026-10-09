# Findings — round 2 (2026-10-08, later)

Delta only. The first findings batch is gone from `docs/` — consumed into
`design-wiring-panel.md` and the task memories. These are what came out of verifying the
fixes.

## What I confirmed fixed

| Was | Now |
|---|---|
| no `status` subcommand | ✅ works — `healthy -- one daemon is serving every console` |
| `export-graph` dropped `metadata`/`related_to` | ✅ both export now — `related_to` under `metadata.related_to`, `governs` under `metadata.governs` |
| superseded memories exported unmarked | ✅ `superseded` + `superseded_by` on every node, **and** a new `--exclude-superseded` flag |
| `doctor` blocked by the daemon lock | ✅ runs with the daemon up |

Verified by diffing a fresh export against a hand-maintained edge file across all five
governance anchors: every `related_to` array matched the export exactly. That let us retire
the sidecar — see the note at the bottom.

---

## BUG-9 — `doctor` has no namespace scope and silently reports a different project (HIGH)

**Expected:** `doctor` takes a namespace, or reports all of them, or says which it chose.

**Actual:** `neurostrata-mcp doctor --help` shows **no options at all**. It picks a namespace
at random across runs and prints its findings as if they were the whole truth.

Three consecutive calls, same machine, same database:

```
keywest.health: 771 memories   ... declared targets that match nothing ingested: 31
global:          78 memories   ... declared targets that match nothing ingested:  0
Nibble.Fish:     28 memories   ... declared targets that match nothing ingested:  0
```

**Why this is worse than a missing flag:** the output is unqualified. Reading
`declared targets that match nothing ingested: 0`, a caller concludes the source graph is
clean — about a completely different project.

**It cost us a real misdiagnosis.** We reported the 31 stale targets as fixed, because the
standalone call happened to land on a namespace with none. The 31 were still there the whole
time. That is a confident wrong answer produced by a health tool, which is worse than no
health tool.

**Suggestion:** `doctor [--namespace <ns>|--all]`, defaulting to `--all` with a per-namespace
breakdown — or refuse to run without a namespace. Never print an unqualified result.

Related: `doctor` is also still described as *"Report what an upgrade left inconsistent,
changing nothing"* while being grouped with the database-locking commands in some paths.
Now that it runs with the daemon up, the description is closer to true — but the scope bug
remains.

---

## FEATURE-4 — memories have no provenance; add a `source` pointer (MEDIUM)

Every memory carries `access_count` and `valid_from`. Nothing says **where the claim came
from** — not the runbook line, not the transcript or owner message, not the commit.

So a memory is an assertion, not an auditable record. When a memory and some other artifact
disagree, there is no way to tell which is authoritative or when each was true.

We hit this in miniature today: a `governs` path appeared in two artifacts spelled two ways
(`mallory/bus/natsbus/` and `mallory/bus/natsbus`). The only way to determine which was
correct was to know that one of them had been written by hand. A `source` field would have
settled it immediately — and, more usefully, would have shown that *both* were hand-written
and the memory layer was not the authority.

**Why it matters for the product's own thesis:** NeuroStrata's value is that it is a *source
graph*, not a text dump. `locations` and `related_to` already give structural provenance.
This is the same idea applied to the claim itself.

**Suggestion:**

```json
"source": {
  "kind": "owner-quote | doc | transcript | commit | derived",
  "ref": "docs/runbooks/twilio.md#L42",
  "captured_at": "2026-10-08"
}
```

Then `search_memory` can rank a sourced memory above an unsourced one; a memory that
conflicts with its own source can be flagged stale; and `supersede_memory` can record *why*
the correction happened, not just that it did.

**Cheap first step:** make `source` optional, but require it when `memory_type: rule`. Rules
are the load-bearing claims — exactly the ones that need to be checkable rather than
believed.

---

## Note on what we changed in response

Upstream's export fidelity fix let us delete two hand-maintained crutches in the guinea pig:

- `docs/neurostrata/graph-edges.json` — a hand-written copy of the edges, created only
  because the export dropped them
- `docs/neurostrata/superseded.txt` — a hand-written list driving a jq delete in our export
  script, created only because superseded memories were unmarked

Both are gone. Our gate now asserts the **export is faithful** instead of comparing the
export against a second copy of itself. That comparison had already started producing
noise — a trailing-slash mismatch between two files I had written the same day — which is
exactly the failure mode of keeping a duplicate.

Lesson worth recording on your side: **when you fix the export, projects can delete their
workarounds.** It would be worth saying so in the release notes, because a project that
built a sidecar will not notice on its own that it can now stop maintaining it.

---

## BUG-10 — `supersede_memory` replaces the text but carries the old metadata over (HIGH)

**Expected:** `supersede_memory` is described as *"The corrected text, written in full. It
replaces the old wording rather than being appended to it."* A correction that fixes a wrong
pointer should leave the memory pointing at the right thing.

**Actual:** the **metadata** — `governs`, `related_to`, `location` — is copied from the old
memory unchanged. Only the prose is replaced.

**Repro:** take a memory whose `Governs` names a file that has been deleted. Supersede it
with corrected text naming the live file, and put the corrected paths in the `Governs:` line
of the content, matching the existing format. The result contains **both**:

```
Code Graph Locations: docs/runbooks/SUCCESSION.md, voice-gateway/gateway_tts.go, review-engine/main.go
Governs: ["docs/runbooks/SUCCESSION.md","voice-gateway/gateway_tts.go","review-engine/main.go"]        <- corrected text

Code Graph Locations: docs/runbooks/SUCCESSION.md, twilio-gemini-bridge/plugins/ai/vertex/voice.go, review-engine/main.go
Governs: ["docs/runbooks/SUCCESSION.md","twilio-gemini-bridge/plugins/ai/vertex/voice.go","review-engine/main.go"]   <- stale metadata
```

One memory, two contradictory `Governs` lists. The structural pointer stays stale forever and
the record now contradicts itself.

**Why this is high, not cosmetic:** it makes `supersede_memory` — the *documented remedy* for
a stale rule — unable to fix stale rules. Your own guidance is *"Fix with
`neurostrata_supersede_memory`, never delete"*. For this class of defect that instruction is
currently unachievable.

It also produced a false sense of completion. The call reported success
(`Superseded e8623130… with f037711d…`) and the stale pointer was still there. Only a gate
caught it.

**Compounding:** the `Governs:` line *in the content text* is not the source of truth for the
metadata — it renders alongside it. So the two can silently disagree and a reader cannot tell
which governs. If the content line is documentation it should not look identical to the
authoritative value.

**Suggestion:**
1. `supersede_memory` should accept the same metadata parameters as `add_memory` (`governs`,
   `contained_by`, `related_to`) and replace them wholesale — or at least offer
   `--replace-metadata`.
2. If metadata is meant to be derived from the content's `Governs:` line, derive it and remove
   the ambiguity about which is authoritative.
3. Either way, `supersede_memory` should **report what it did not change**. Saying "superseded
   with X" while leaving a stale structural pointer is the confident-wrong-answer problem
   again.

**Consequence for us:** a stale `governs` pointer cannot be repaired through the public API.
Our `neurostrata-export-gate` reports it as a warning rather than failing — failing would hold
the build red on something no agent can resolve. When this is fixed, that check goes back to
blocking.
