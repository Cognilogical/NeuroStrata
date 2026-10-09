# Prescribed-Wire Recipes — project-pipeline gates

Status: **written** (task `neurostrata-wimj`). Companion to
[`docs/design-wiring-panel.md`](../design-wiring-panel.md), which enumerates the
wires and assigns them to homes; this page is the recipe for each
`project-pipeline` wire a project must implement itself.

**Constraint honored:** core stays project-agnostic. Nothing here ships in the
Rust server. The panels in `docs/design-wiring-panel.md` Q3 assign A2/A3/A4/
A5-residual/A7 to `runs_in: project-pipeline` for exactly one reason — each one
needs to know what *this* project's files are called and where its rulings live.
NeuroStrata ships the **query surface** (the export carries `absolute_path`,
`location`, `metadata.governs`, `metadata.related_to`, `superseded`,
`superseded_by`); the project ships the **predicate**.

Reference implementations are the `keywest.health` guinea pig
(`~/Documents/keywest.health`, sessions 2026-09-27 →). Every file path in this
document is relative to that repo root unless it starts with `docs/` inside
NeuroStrata.

---

## The contract every prescribed gate obeys

Stated once, so each recipe below only states its delta. This is the A8 split
from [`docs/design-wiring-panel.md`](../design-wiring-panel.md) Q4: core ships
the **contract and its own self-test**, never the file-mutating runner.

| Rule | Value | Source |
|---|---|---|
| Exit codes | `0` clean · `1` violation found · `2` could-not-check (infra) | Q4.2; already the `task gate` convention (`neurostrata-mcp task gate --help`) |
| Teeth | `blocks: true` → exit 1 fails the pipeline. `blocks: false` → warn only, exit 0 | Q1 field contract; `src/task/wiring.rs` |
| Self-test | accept `--self-test`: exit 0 **iff** a planted violation makes the gate exit 1 AND the tree is byte-identical afterward | Q4.2, Q4.3 |
| Language | do not enforce with a tool the project has banned (keywest: Go-only products, no Python in tracked trees) | `scripts/neurostrata-sync-gate.sh:33-34` |
| Registry honesty | if a gate is `PARTIAL`, say so in the file header and print the known gap | `scripts/owner-directive-gate.sh:24-26,126-131` |

The `--self-test` convention exists because **a gate that passes its own test is
not a gate**. keywest learned this the expensive way: `phi-guard` was green for
its first build while being blind, because `grep -E` against a `(?i)` pattern
matches nothing and still exits 0
(`scripts/tests/gate-negative-controls.sh:8-11`). The same trap killed two
bugs in the export gate's first build (`scripts/neurostrata-export-gate.sh:63-67`
and `:83-85`).

**Do not open the NeuroStrata database from CI.** Every recipe below reads the
*committed export JSON*, nothing else. That is deliberate on three counts: the
database is not in CI, DB-touching CLI commands refuse to run while the daemon
holds the lock, and each `LadybugStore` open reserves an 8 TiB mmap region
(`lbug::SystemConfig::default()` never shrinks the C++ engine's open-time
reservation; memory `8c85f445-8904-4239-8f08-7f4de4f241d8`, task
`neurostrata-c8te`). Reading a JSON file in CI costs nothing and has none of
those failure modes.

---

## A2 · `memory-to-repo-drift` — memories point at files that moved

**Rationale.** A memory carrying `locations: [{path, lines, symbol}]` is a
structural claim, not prose: it says "this rule governs that file". The moment
the file moves, the claim silently becomes false and the rule stops being
applied to the code it was written for — while still surfacing in
`get_snapshot` with full authority. Two such rot events were found by accident
in keywest on day one, not by any gate (`scripts/neurostrata-sync-gate.sh:9-15`).
`neurostrata-mcp doctor` reports the same class as "declared targets that match
nothing ingested".

**The shape.** Load the export, select nodes for **your namespace only**,
skip `superseded == true`, take `absolute_path` ∪ `location`, relativize
(repo root prefix stripped, bare paths resolved against the root), drop
anything under another project's prefix, then assert every remaining path
exists. Exit 1 naming each stale path; exit 2 if `jq` is missing or the export
is absent — never exit 0 on an unreadable input.

Three non-obvious requirements, all of which bit keywest:

1. **Scope by namespace, and skip superseded.** The export carries every
   project on the machine (8 namespaces, 1275 nodes in keywest's copy).
   Filtering is not cosmetic: NeuroStrata's own rows carry `./src/daemon.rs`
   paths that would flood the check. And a retired memory pointing at a
   retired path is *history, not a defect* — it is marked
   `superseded: true`, so it must be filtered explicitly
   (`scripts/neurostrata-sync-gate.sh:63-82`).
2. **Fail with the fix, not just the finding.** The report names
   `neurostrata_supersede_memory`, and states *never delete* — a correction is
   a compensating event, not an erasure (`:110-116`).
3. **One gate, one job.** Do not also scan `metadata.governs` here; that is the
   export gate's check B. Overlapping gates duplicate failures and produce
   violations nobody can repair (`:75-78`).

**Reference impl.** `scripts/neurostrata-sync-gate.sh`, check A (lines 58-117).
Gate point: `.github/workflows/architecture-gates.yml:36-37`.

```bash
mapfile -t allpaths < <(jq -r --arg ns "$NS" '
  [ .nodes[] | select(.namespace == $ns) | select(.superseded != true)
    | .absolute_path // empty, .location // empty ]
  | map(select(length > 0)) | .[]' "$GRAPH" 2>/dev/null | sort -u)
```

**Acceptance criteria.**

- [ ] Exit 1 when a live node's path does not resolve; the report names the path.
- [ ] Exit 2 when the export is missing or `jq` is unavailable (never 0).
- [ ] A superseded node with a dead path does **not** fail the gate (negative control: set `superseded: true` on the planted node).
- [ ] Another project's node with a dead path does **not** fail the gate (negative control: plant in a foreign namespace).
- [ ] `--self-test` plants the violation, gets exit 1, and `cmp` proves the tree byte-identical afterward.
- [ ] The gate never invokes `neurostrata-mcp` (proved by grepping the script).

---

## A3 · `repo-to-memory-drift` — rulings written down but never recorded

**Rationale.** The inverse direction, and the more dangerous one: a dated owner
ruling written into a tracked doc but never added to memory is **invisible to
the layer agents query first**. keywest's arrangement is explicit — the memory
layer is *first line*, files are *backup*
(`docs/neurostrata/README.md:18-24`) — so an unrecorded ruling is a ruling the
agent never sees. It was measured, not guessed: 21 of 22 owner phrases were
missing on 2026-10-08.

**The shape.** Find dated ruling lines in tracked markdown, extract the
owner's *quoted* words, and require that phrase to exist in the memory layer.

The narrowness is the lesson. An earlier version required every dated line to
appear verbatim in memory; memories condense and paraphrase, so it failed
**227/227 on a healthy tree** and would have been disabled within a week
(`scripts/neurostrata-sync-gate.sh:149-159`). The load-bearing content of a
ruling is his own words inside the quotation marks — that is distinctive, that
is what "verbatim means his words" protects. Lines with no quote are **skipped,
not guessed at**.

Mandatory scope exclusions, each with a reason:

| Exclude | Why |
|---|---|
| `docs/neurostrata/` | generated export; would compare the export to itself |
| `docs/directive-manifest.md` | a *mirror* generated by the A7 gate's `--write`; checking a mirror against memory is circular — it reproduces every quote by design |
| `docs/operating-state/` | journal archives; history is not active doctrine |
| `prompts/` | live agent dialogue, governed by a golden test; 112 of the first 138 "unrecorded" hits were prompt dialogue |
| `scratch/` | not doctrine |

Normalize before comparing: lowercase, collapse whitespace, strip trailing
punctuation — an earlier exact match missed two rulings because the doc put the
full stop *inside* the quotes and the memory put it *outside*
(`:143-147`). Also reject quoted timestamps and single-word fragments as
non-rulings (`:174-181`).

**Ratchet, do not mandate.** Store the observed gap in a checked-in baseline
file. Fail only if the count *rises above* it; when it falls, print the
instruction to lower the baseline **in the same commit** so the gain locks in.
A gate red from day one gets disabled; a gate that only ratchets keeps the
improvement (`:202-232`). keywest's baseline is `docs/neurostrata/sync-baseline.txt`,
currently `0`.

**Reference impl.** `scripts/neurostrata-sync-gate.sh`, check B (lines 119-232),
baseline at `docs/neurostrata/sync-baseline.txt`.

**Acceptance criteria.**

- [ ] Exit 1 only when `unrecorded > baseline`; prints the new number, the old, and the phrase.
- [ ] Exit 0 when `unrecorded < baseline` (with the "lower the baseline now" instruction) and when `== baseline`.
- [ ] Unquoted dated lines are counted as `skipped_noquote`, not as violations.
- [ ] The four excluded paths above are excluded by an explicit, commented filter — not by accident.
- [ ] Both exclusion directions hold under `--self-test`: plant a real ruling in a scanned doc → exit 1; plant the same line in `docs/operating-state/` → exit 0.
- [ ] Normalization is exercised: a quote differing only by trailing punctuation matches (keywest lost two rulings here).

---

## A4 · `export-freshness` — warn only, never block

**Rationale.** Freshness is a property of a **committed artifact**, and
committing is the project's VCS and pipeline. But freshness is *advisory*: the
export can legitimately lag a session — it is regenerated at a checkpoint, not
per commit. A stale backup is still a backup. So this is the one wire in the
`project-pipeline` set with `blocks: false`
(`src/task/wiring.rs`, wire `export-freshness`: *"a stale backup is still a
backup; warn, never block"*).

**The shape.** `age_days = (now - mtime(export)) / 86400`; warn when
`> MAX_AGE_DAYS` (keywest: 14, overridable by env). **Exit 0 either way.** The
warning names the refresh command.

Do not let the freshness check absorb a fidelity check. keywest's export gate
originally did both and collapsed into a re-check of a bug already fixed
upstream; the shipped version does only the fidelity job
([`docs/design-wiring-panel.md`](../design-wiring-panel.md) Q3, "A1 shrinks… A5
shrinks to a thin check on the export's own markers", and the "Killed" list:
*do not ship a check for a bug we fixed*).

**Reference impl.** `scripts/neurostrata-sync-gate.sh`, check C (lines 234-243) —
last block in the file, no `failed=1`, no influence on the exit.

```bash
age_days=$(( ( $(date +%s) - $(stat -c %Y "$GRAPH") ) / 86400 ))
if [ "$age_days" -gt "$MAX_AGE_DAYS" ]; then
  echo "    WARN: older than $MAX_AGE_DAYS days — refresh with scripts/neurostrata-export.sh"
fi
```

**Acceptance criteria.**

- [ ] Exit 0 when the export is older than `MAX_AGE_DAYS`, with the WARN printed.
- [ ] Exit 0 when the export is fresh.
- [ ] The WARN names the exact refresh command.
- [ ] This check provably cannot set the gate's failure flag — asserted by the negative-control suite: with the export `touch`ed to an old date, the combined gate still exits 0.
- [ ] Never `git commit` the export from inside the gate. Refreshing and committing are a human/agent step; the gate only reports.

---

## A5-residual · `supersede-hygiene` — no retired record posing as live

**Rationale.** Corrections must be **compensating events, not erasures**:
`neurostrata_supersede_memory` writes the corrected text and keeps the old
memory readable by id. That only holds if the export carries both, *marked* —
which is exactly the property that decayed twice upstream. `export-graph` used
to write only `absolute_path, content, domain, id, location, memory_type,
namespace` — dropping `metadata` and `related_to` entirely, and giving
superseded memories no marker at all (`scripts/neurostrata-export-gate.sh:10-14`).

**The shape.** Four checks, all against the export:

| | Check | Catches |
|---|---|---|
| A | every node carries `superseded` **and** `superseded_by`; `metadata.related_to` and `metadata.governs` **exist somewhere** in the export | the export silently hollowing out again |
| B | every `governs` path resolves — **live nodes only** | a rule pointing at a deleted file |
| C | every `related_to` id names a node present in the export | orphan edges |
| D | a node with `metadata.anchor` carries both `related_to` and `governs` | a governance anchor that governs nothing |
| E | `bool(superseded) == bool(superseded_by)` | half-marked retirement |

Check A must be **existence, not per-node population**: most memories carry
only `access_count`/`valid_from`, so sampling the first node reports the graph
fields absent when they are present exactly where they belong — bug #1 in this
gate's first build (`:63-67`).

Check B runs on **live nodes only**. A superseded memory pointing at a retired
path is expected — that is what history is. The first build flagged it and
called the backup unfaithful: bug #2 (`:83-85`).

**The residual, stated honestly.** The panel's Q3 phrasing for A5-residual is
"checking the project's **exclusion list** against the export is project
config". That was true while projects hand-maintained a
`docs/neurostrata/superseded.txt` driving a `jq` delete. It stopped being true
upstream: `export-graph` now writes the markers and offers
`--exclude-superseded` (confirmed in `neurostrata-mcp export-graph --help`),
so keywest deleted the sidecar
(`scripts/neurostrata-export.sh:102-111`). The sidecar "was the second copy
that disagreed with the first" — a hand-maintained duplicate of an export is
worse than no backup, because it is trusted.

So the residual a project now implements is:

- If you keep **no** exclusion list (the shipped shape): run checks A and E.
  They are what stops the hollowing-out from recurring.
- If you **do** keep one (an `--exclude-superseded` export variant, a filtered
  view, a hand-maintained list): check that it agrees with the export's own
  markers. A disagreement is a hard fail. Never hand-maintain it; derive it.
- Never rebuild a sidecar to work around a missing export field. That is the
  bug, not the fix — file it upstream (`scripts/neurostrata-export-gate.sh:133-136`).

**Reference impl.** `scripts/neurostrata-export-gate.sh`, checks A-E (lines
62-114), failure report at 125-138. Gate point:
`.github/workflows/architecture-gates.yml:42-43`.

**Acceptance criteria.**

- [ ] Exit 1 when a node lacks `superseded`/`superseded_by`.
- [ ] Exit 1 when `metadata.related_to` is absent from every node in the namespace.
- [ ] Exit 1 on a `related_to` id that names no node (plant a nil UUID).
- [ ] Exit 1 on a half-marked supersede (`superseded: true`, `superseded_by: null`).
- [ ] Exit 1 on a live node whose `governs` path does not resolve.
- [ ] Exit **0** when only a *superseded* node has a dead `governs` path (negative control — bug #2's regression guard).
- [ ] The failure message says "re-export", names the export script, and explicitly tells the reader **not** to rebuild a sidecar.
- [ ] Exit 2 (not 0) if the export is missing or unparseable.

---

## A7 · `directive-preservation` — a recorded ruling must survive verbatim

**Rationale.** The owner's rule (keywest `AGENTS.md`, 2026-10-03): recorded
owner words are spellchecked and otherwise kept — *"verbatim means his words,
not his typos."* Moving a directive is not permission to reword it. The worst
failure mode of a refactor that reorganizes docs is **silent loss of a ruling**,
and silent loss is invisible by construction. keywest shrank `AGENTS.md` and
`bosun.md` on 2026-10-08 and needed a machine to prove nothing was lost
(`scripts/directive-preservation-gate.sh:8-13`).

**The shape.** Baseline-and-compare, with the baseline a checked-in manifest
of verbatim lines:

1. `--write` regenerates `docs/directive-manifest.md` from the tracked tree.
2. Default (verify) pulls the recorded lines out of the manifest's code block,
   flattens the tracked markdown corpus once, and requires each line to appear
   **exactly**, modulo surrounding whitespace only.
3. Missing or reworded → exit 1, list the lost lines (truncated to 20).
4. The fix, in the message: restore the wording, or — if the owner approved the
   change — regenerate with `--write` **in the same commit**, so the decision is
   visible in the diff.

The exclusion that makes this a gate rather than theatre: **exclude the
manifest itself.** An earlier version included it, and the check was
tautological — every recorded line trivially "existed" in the manifest that
recorded it, so the gate could never fail and gave false assurance while
silently losing directives. Only a **negative control** (reword one line)
exposed it (`:44-47`).

**Reference impl.** `scripts/directive-preservation-gate.sh` (82 lines, whole
file). Negative control: `scripts/tests/gate-negative-controls.sh:194-199`
deletes a recorded directive from `AGENTS.md` and asserts the gate exits
non-zero, then asserts green after restore. Gate point:
`.github/workflows/architecture-gates.yml:33-34`.

**Acceptance criteria.**

- [ ] Exit 1 when a recorded line is deleted from the tree; the report shows the line and the remedy.
- [ ] Exit 1 when a recorded line is **reworded** (whitespace-only differences still pass).
- [ ] Exit 2 when the manifest is missing — never 0.
- [ ] The corpus excludes the manifest, `docs/neurostrata/`, and `scratch/`.
- [ ] `--write` regenerates the manifest deterministically (sorted, deduplicated) and is safe to run in the same commit as a deliberate rewording.
- [ ] Negative control exists: reword one recorded line in a scratch copy → exit 1 → restore → exit 0, with `cmp` proving byte-identical restore.

---

## Appendix · A8 · `gate-self-test` — the harness that keeps the other gates honest

Not one of the five, but it is the `project-pipeline` wire that makes their
acceptance criteria enforceable, and the recipe is the sharpest thing in the
guinea pig: **a gate that cannot fail its own negative control is not a gate,
it is a script that exits 0.** Every gate in keywest was tested, but the tests
were ad-hoc and never committed, so nothing prevented silent gate rot.

The shape (`scripts/tests/gate-negative-controls.sh`, 356 lines):

- For each gate: plant a violation it is *required* to catch → assert non-zero
  exit → restore → assert green again.
- **Assemble planted payloads from fragments at runtime**, so the harness file
  does not itself contain the shapes the guards hunt for.
- **Scope every plant.** A plant in another namespace is invisible to a
  namespace-scoped gate, so the control passes for the wrong reason — that is
  what happened the first time here: the *control* was broken, not the gate
  (`:317-321`).
- `trap cleanup EXIT` + a final `verify_restored` that `cmp`s every mutated
  file against its own backup.
- Per-file backup paths, derived by flattening separators — **and locals
  declared then assigned, never on one line.** Bash expands every word before
  `local` assigns, so `$TMP/$(basename "$path").bak` read an unbound `path`
  under `set -u`; every backup landed on the same file and the EXIT trap wrote
  `AGENTS.md`'s content into `docs/rules.md` and deleted a recorded owner
  directive. **The safety net was the thing that caused the damage** (`:68-88`).
- Report a gate that cannot fail its control as `BROKEN`, not as passing.

Wire it into the same pipeline as the gates themselves
(`.github/workflows/architecture-gates.yml:49-50`).

Related core-side rule (NeuroStrata, `neurostrata-mcp task gate --self-test`):
short-circuit the self-test **before** any daemon probe or lock check, run the
engine in-process, and name the missing/extra violation kind in the FAIL message
(memory `a2be86b7-ca23-411f-b4b7-ca0cc40849e1`,
`eed92855-8ea0-43c8-a095-47f04af72b15`). The drill: temporarily silence one
rule with `if false && …`, rebuild, run the CLI, capture the exit code. "It
compiles" is not "it works".

---

## Rejected shapes (one line each)

- **Opening the NeuroStrata database from CI** — not in CI, lock-guarded, and
  an 8 TiB mmap reservation per open (memory `8c85f445`).
- **Requiring every dated doc line to appear verbatim in memory** — measured
  wrong: memories paraphrase, 227/227 false positives on a healthy tree.
- **Including the A7 manifest in the A7 corpus** — tautological; the gate can
  never fail.
- **Checking `metadata.governs` in both A2 and A5** — one gate, one job.
- **Blocking on export freshness** — a stale backup is still a backup.
- **A generic file-mutating runner in core** — Q4.1: blast radius beyond the
  memory system's remit; the guinea pig corrupted two files building one.
- **Rebuilding a hand-maintained sidecar when an export field is missing** — a
  second copy of the graph is what the export gate replaced.

---

## Sources

- `docs/design-wiring-panel.md` — Q1 schema, Q3 core/project split, Q4 A8
  contract, Q5 verification, Q6 "file as tasks".
- `src/task/wiring.rs` — the shipped registry: ids, `runs_in`, `blocks`,
  `reason` strings quoted above.
- `docs/architecture/legacy/keywest.health_legacy.md` — guinea-pig context.
- Memories: `7134dc9d-d080-42ce-9bfb-9d33f8a59b7e` (wiring panel),
  `eed92855-8ea0-43c8-a095-47f04af72b15` (core/project self-test split),
  `a2be86b7-ca23-411f-b4b7-ca0cc40849e1` (self-test drill),
  `bcecac84-54d1-4ad7-9ec5-f42e2aa326fb` (gate contract / exit codes),
  `8c85f445-8904-4239-8f08-7f4de4f241d8` (lbug 8 TiB sharp edge).
- Reference impl (external repo `~/Documents/keywest.health`):
  `scripts/neurostrata-sync-gate.sh`, `scripts/neurostrata-export-gate.sh`,
  `scripts/neurostrata-export.sh`, `scripts/directive-preservation-gate.sh`,
  `scripts/tests/gate-negative-controls.sh`,
  `.github/workflows/architecture-gates.yml`, `docs/neurostrata/README.md`,
  `docs/neurostrata/sync-baseline.txt`.