# Task 0.2 — Sync Resolution: Fork `develop` → Upstream Target

Anthropic-Memory Parity program, Milestone 0. This document is the
sync-resolution required by the plan (Task 0.2): it names the target,
the path, the fallback, and the price. **It authorizes nothing.**
The sync itself, its merge PR, and every filing or merge named here
proceed only under Pratik's separate authorizations (see §7, the
M0 hard gate).

- **Resolution date:** 2026-11-07
- **Base this resolution is written against:** fork `develop` @
  `5b8f9a7c3b7808da1f0c550e8eb081667335954c` ("fix(cli): send the
  auto-spawned daemon's stderr to daemon.log instead of /dev/null
  (#27)"), verified live at writing time (`git fetch origin develop`
  → `git rev-parse origin/develop` returned exactly this SHA;
  this branch's HEAD was verified identical before work started).
- **Companion evidence:** Task 0.1 baseline census
  (`baseline-census.md`, same program directory on its own branch) —
  its divergence numbers are re-derived, not copied, in §3 below.
- **Method:** git object inspection only (`rev-list`, `diff`,
  `merge-tree`, `ls-tree`, `show`). No binary was built or run, no
  daemon or store was touched, nothing was pushed anywhere.

## 1. Target (SHA-pinned)

**Upstream commit
`8fdae4b745269b4097ecbf02a6375f0a0c3b792c`** — upstream PR #57
("fix(mesh): gate unix-only APIs in mesh import tests so they compile
on Windows"), verified at writing time:

```
$ git show -s --format='%H %P %s' 8fdae4b745269b4097ecbf02a6375f0a0c3b792c
8fdae4b745269b4097ecbf02a6375f0a0c3b792c cd66291203deec830aa48b9cb85f64ac3ce23d7c fix(mesh): gate unix-only APIs in mesh import tests so they compile on Windows (#57)
```

What the target contains:

- **Version 1.16.1** (`Cargo.toml` at the target: `version =
  "1.16.1"`; the v1.16.1 fix commit `584567e` is in its ancestry).
- **PR #56** as its direct parent: `cd66291203deec830aa48b9cb85f64ac3ce23d7c`
  ("chore(deps): refresh Cargo.lock to clear 9 RUSTSEC advisories
  (#56)"). `cd66291` is #56 and does **not** by itself contain #57 —
  that distinction is why the pin is `8fdae4b`, not `cd66291`
  (plan judge's RC1).
- **PRs #59 and #60**, merged before #56/#57, are also inside the
  target by ancestry.
- The judgment internals the rest of the program depends on
  (census row 2j): `HeuristicJudgmentProvider` as the active provider
  for `Auto`/`Heuristic` modes and working shadow-verdict machinery
  (`last_shadow`, recorded in `check_duplicates`) — same trait
  surface as develop, different provider internals (plan RC4).
- The Phase 2 surfaces absent on develop (census rows 5c–5e, 8, 10):
  `src/fts/`, `src/lexical.rs`, `src/rdf/`, `src/community.rs`,
  `src/rebac.rs`, `src/did.rs`, `include_ast_edges`.

**SHA-pin rule.** The target is this exact commit, not "upstream
main". If upstream has advanced past `8fdae4b` when the sync
executes, the delta is **re-censused under Task 0.1** (divergence
counts and any new primitives re-derived) and the pin is re-ruled —
the delta is never silently absorbed into the sync.

**Live delta (re-census seed, not part of the target) — verified
at 2026-11-08 08:41 UTC** by a fresh `git fetch upstream` in the
refresh session that produced this section's current text
(the first version of this section recorded a one-commit delta
with PR #62 still open; that was accurate when its evidence was
gathered, but upstream moved before the document was delivered,
and the review process caught it — a correction then recorded a
four-commit delta at `fc0381d`, and upstream has moved once more
since; the delta below replaces that as well):

- Upstream `main` now sits at
  **`51be592335b4b447252af198b375836bc7246f8d`**, exactly **5
  commits past the target** (`git rev-list --count 8fdae4b..51be592`
  → 5; the chain is linear, each commit's sole parent is the
  previous one):
  1. `581ae333a79460497be904c24122b93ecbead58c` — PR #61,
     "test(harness): read request before responding in federation
     tests; raise daemon probe budget" (test-harness only;
     touches `src/daemon.rs` and `src/synaptic_mesh/federation.rs`
     test code paths).
  2. `7a79ad0fb54a88a62a08158bea68a229f9d7f0cb` — PR #62,
     "feat: Autonomous Memory Control Plane (Waves 1–5)", merged
     2026-11-07T21:11:04Z. (The earlier **watch note** on this PR
     is discharged: it merged, and it is delta, not target.)
  3. `8d290e7125b8fc09efa38ea7a7c2f95a9bccef58` — PR #63,
     "feat(mesh): NeuroMesh near-real-time multi-agent cognitive
     mesh transport" (commit dated 2026-11-07T22:33:31Z).
  4. `fc0381d23106ff782130283e3e8966321b8a7152` — "release:
     v1.17.0 — NeuroMesh real-time transport & Autonomous Memory
     Control Plane" (commit dated 2026-11-07T22:42:29Z; tag
     `v1.17.0` fetched with it).
  5. `51be592335b4b447252af198b375836bc7246f8d` —
     "feat(guard,snapshot,plugins): surface global & pinned golden
     rules at startup and guard validation (neurostrata-0wle)"
     (commit dated 2026-11-08T00:17:46Z).
- Measured against the census base, the live tip is now 778
  upstream-only commits with 161 differing `src/` files (445
  files total) — versus 773 / 154 against the target. (The
  `src/` and total counts are unchanged from the `fc0381d`
  measurement: every file the newest commit touches was
  already in the differing set against the census base.)
  **§3's conflict surface stays priced against the target**, as the
  gate requires; these live-tip numbers are the re-census seed
  for execution time, not a re-pricing.
- **This is the SHA-pin rule operating as designed.** The target
  stays `8fdae4b`; the delta is recorded, not absorbed. The
  fork's mirror is unchanged by any of it: fork `main` was
  re-verified in the same session at exactly
  `8fdae4b745269b4097ecbf02a6375f0a0c3b792c`. If upstream moves
  again before the sync executes, this subsection is re-derived
  once more — that is the rule working, not a defect in it.

## 2. Path — forward-merge via the fork's own `main`

The fork's mirror branch already sits exactly at the target, so the
sync needs no new upstream fetch of content — only verification:

1. **Verify the mirror.** In the sync clone:
   `git fetch origin main develop`, then confirm
   `git rev-parse origin/main` =
   `8fdae4b745269b4097ecbf02a6375f0a0c3b792c`, and confirm
   **string-equality with upstream** for that commit: the commit is
   content-addressed, and the check is
   `git diff origin/main 8fdae4b…` producing no output (trivially,
   they are the same object) **plus** a fetch of upstream
   `main`'s ref in the sync session showing upstream's tree at
   `8fdae4b` is the object the fork mirrors (coordinator verified
   this mirror state via `ghg` on 2026-11-07; the sync session
   re-verifies rather than trusting this document).
2. **Branch** a sync branch off current `develop` (tip at execution
   time; this resolution is written against `5b8f9a7`, and if
   develop has moved, the conflict numbers in §3 are re-derived).
3. **Forward-merge** `origin/main` (`8fdae4b`) into the sync branch.
   This is a true merge, not a rebase and not a cherry-pick series:
   the fork's history (538 develop-only commits) is preserved.
4. **Resolve conflicts against the preservation checks in §4** —
   they are merge-time acceptance checks, not aspirations.
5. **Post-merge steps, in order:**
   a. `binaries/` check (see §4.3).
   b. **Re-run the Task 0.1 census on the new base** (the M0 gate
      requires census + baselines re-run on the post-sync base;
      the census's provider-identity rows will change — that is
      expected and must be recorded, not smoothed over).
   c. **Re-apply / verify the upstream-push guard** (§6):
      `scripts/hooks/pre-push` and `scripts/install-hooks.sh` are
      themselves on the predicted conflict list (§3), so the merge
      can clobber them; after resolution, the installed hook must
      be re-tested with the fabricated-URL cases in §6 before any
      push from the sync clone.
   d. **Full fork CI matrix as the gate**: the `test (ubuntu)`
      check for iteration, then the full 3-OS test + build matrix
      green on the merge PR's exact head, with all review threads
      resolved — per the standing merge discipline, a summary or a
      stale head is not evidence.
6. **The merge PR needs Pratik's per-PR word**, and the sync
   execution needs his separate authorization (§7). This document
   is neither.

## 3. Expected conflict surface (computed, 2026-11-07)

Re-derived at writing time between `5b8f9a7…` and `8fdae4b…`
(merge-base `7d836c72e81552523b86abbb0b80d71709551c4e`):

```
$ git rev-list --count 5b8f9a7..8fdae4b   → 773   (upstream-only commits)
$ git rev-list --count 8fdae4b..5b8f9a7   → 538   (develop-only commits)
$ git diff --name-only 5b8f9a7 8fdae4b -- src | wc -l → 154 differing src files
$ git diff --shortstat 5b8f9a7 8fdae4b
  430 files changed, 90072 insertions(+), 32127 deletions(-)   (all files)
  154 files changed, 51549 insertions(+), 23513 deletions(-)   (src/ only)
```

A read-only dry run predicts the actual textual conflicts:

```
$ git merge-tree --write-tree --name-only 5b8f9a7… 8fdae4b…   (git 2.43.0; exit 1)
164 conflicted files, of which 95 are under src/
```

104 of the 154 differing `src/` files were changed on **both**
sides since the merge-base, which is why the conflict count is so
high: this is not a merge where one side stood still.

**Hottest files** — top 15 `src/` files by two-tip churn
(`git diff --numstat 5b8f9a7 8fdae4b -- src`, added+deleted):

| # | File | Added | Deleted | Total churn |
|---|------|------:|--------:|------------:|
| 1 | `src/store/ladybug.rs` | 7,083 | 10,993 | 18,076 |
| 2 | `src/server.rs` | 5,623 | 620 | 6,243 |
| 3 | `src/synaptic_mesh/import.rs` | 3,545 | 345 | 3,890 |
| 4 | `src/main.rs` | 1,060 | 2,088 | 3,148 |
| 5 | `src/embed.rs` | 476 | 2,250 | 2,726 |
| 6 | `src/parser/ingest.rs` | 1,311 | 804 | 2,115 |
| 7 | `src/task/mod.rs` | 1,281 | 530 | 1,811 |
| 8 | `src/synaptic_mesh/worker.rs` | 1,429 | 134 | 1,563 |
| 9 | `src/events/bus.rs` | 77 | 1,355 | 1,432 |
| 10 | `src/rdf/ontology.rs` | 1,269 | 0 | 1,269 |
| 11 | `src/graphql/mod.rs` | 983 | 0 | 983 |
| 12 | `src/fts/mod.rs` | 975 | 0 | 975 |
| 13 | `src/time_series.rs` | 956 | 0 | 956 |
| 14 | `src/kge.rs` | 880 | 0 | 880 |
| 15 | `src/registry.rs` | 827 | 45 | 872 |

All of rows 1–9 and 15 also appear on the `merge-tree` conflict
list; rows 10–14 are upstream-new files (add-only, no conflict
expected). One structural fact the merge must confront head-on:
**upstream deleted `src/events/outbox.rs`** (791 lines on develop,
absent at the target; the target's `src/events/` has no `outbox.rs`).
Develop's durable-sequencing design (census rows 3b–3c) lives in
that file, and plan Task 2.1 builds on it — see §5(b) for what that
means if the sync is deferred, and §4 for the merge-time check.

**Full-sync price (the path in §2), stated as estimates:**
effort is dominated by the 164 predicted conflicts, concentrated
in the table above. A realistic planning figure is **several
focused engineer-days of conflict resolution** (the ladybug.rs /
server.rs / import.rs resolutions are design decisions, not
textual edits), **plus** the §4 preservation checks, **plus**
roughly **5–15 `test (ubuntu)` iteration runs and 2–4 full CI
matrix runs** before the gate in §2 step 5(d) can pass. Risk:
**high but bounded and one-time** — the divergence resets to zero
against the target, every later phase builds on the resolved base,
and the cost does not recur. The main risk shape is a wrong
semantic resolution in a hot file (§4 exists to catch exactly
that), not effort overrun.

## 4. Forward-merge semantics that must be preserved

These are merge-time checks. Each one is verified on the merge
result, with file evidence, before the merge PR is called ready.

1. **Laya honest semantics (2026-11-01 v1.14.3-patch forward-merge
   precedent).** The fork keeps its honest semantics: the Laya
   provider surfaces `Unavailable`/errors honestly (census row 2h:
   `LayaOnnxProvider` in develop loads to `RuntimeState::Unavailable`
   and `judge_duplicates` returns an error) rather than silently
   substituting another judge in the Laya slot. From that merge,
   the only upstream Laya behavior adopted was upstream's
   `fetch_model`. The sync resolution must not reintroduce a
   silent mock-as-Laya substitution (census row 2d describes
   develop's current `DeduplicationChecker::new` wiring, where the
   Mock sits in the Laya slot by an in-code decision — any change
   to that wiring in the merge is a deliberate, recorded decision,
   not a conflict-resolution accident).
   *Deliberate exception, recorded here so it is not mistaken for
   a violation:* adopting the target's `HeuristicJudgmentProvider`
   as the `Auto`/`Heuristic` active provider and its shadow-verdict
   machinery **is** the sync's purpose (it changes Task 2.6's sweep
   behavior — plan RC4). Honest-semantics governs failure surfacing,
   not a freeze on provider identity.
2. **Upstream #43/#46 fixes adopted — direction: toward the
   TARGET.** *(Corrected in review: the first version of this
   item attributed #43/#46 to fork PRs on develop and directed
   resolution toward develop. That was wrong in attribution and
   reversed in direction; the verified record follows.)* #43 and
   #46 are **upstream issues** (both CLOSED — #43: "bug(mesh):
   bundle-only records are applied without `validate_record`
   validation"; #46: "bug(mesh): records are signed before
   lifecycle fields are preserved and RDF is rebuilt"). No PR
   #43 or #46 exists on the fork or upstream (verified via `ghg`
   reads in the correction session). The fixes landed
   **upstream**: issue #43 → PR #53 MERGED ("fix(mesh): route
   bundle-only records through validate_record and epoch_gate",
   commit `7df9e41`); issue #46 → PR #51 MERGED ("fix(mesh): sign
   records after lifecycle preservation and RDF rebuild", commit
   `e2beea0`). Both commits are ancestors of the target and are
   **not** ancestors of develop; the fork-port branches carrying
   the same fix subjects are ancestors of neither. Content check:
   `validate_bundle_record` appears **6× at the target** (all in
   `src/synaptic_mesh/import.rs`), **0× on develop** and **0× on
   PR #23's head**; develop's `import.rs` contains **zero**
   `bundle` mentions at all — the bundle path does not exist on
   develop (the target's `import.rs` has 313). Preservation rule
   for the merge, therefore: in `src/synaptic_mesh/import.rs`
   (3,890 churn, conflicted) and the signing/lifecycle code paths,
   conflicts in the **bundle-gate (`validate_bundle_record`
   routing) and sign-after-lifecycle-preservation behaviors
   resolve toward the target-side fixed behavior**. Develop has
   no competing fixed behavior to preserve here — it simply
   lacks the path. (This reverses only these two behaviors;
   §4.1's Laya honest-semantics direction is unchanged.)
3. **`binaries/` policy ("binaries only for releases").** Commit
   `6ce7021` dropped vendored release binaries from develop, and
   its message instructs: upstream's tree carries `binaries/`, each
   forward-merge re-adds it, re-delete in the sync's fix commit
   citing `6ce7021`. **Measured caveat at writing time:** neither
   tip currently lists the directory —
   `git ls-tree -r --name-only <tip> -- binaries` returns 0 entries
   for both `5b8f9a7…` and `8fdae4b…`, and `binaries/` does not
   appear in the two-tip diff. The merge-time step is therefore a
   **check**: if the merge result contains `binaries/`, delete it
   in the sync's fix commit citing `6ce7021`; if it does not,
   record its absence in the merge PR. The policy is preserved
   either way; the unconditional "re-delete" wording in `6ce7021`
   predates the current upstream tree shape.
4. **Judgment trait surface stays duplicate-only in name.** The
   trait at both tips exposes `judge_duplicates` and nothing else
   (census row 1 / flag F3). The merge must not be used to smuggle
   in a contradiction method — that is plan Task 2.7's new work,
   on its own branch, later.

## 5. PR #23 disposition

- **State:** fork PR #23 is **CLOSED, unmerged** (2026-11-06).
  Its head branch is `merge/main-into-develop-1bfc129` @ `2c866d5`
  (still present on the fork remote at writing time:
  `refs/heads/merge/main-into-develop-1bfc129`).
- **Disposition under this resolution: superseded. It is not a
  sync vehicle**, for these reasons (the set as corrected in
  review — an earlier version's third reason attributed fixes
  to fork PRs #43/#46 on develop, which do not exist; §4.2 gives
  the verified record):
  1. **Ceiling.** It merges upstream `main` @ `1bfc129` = **v1.15.0
     only**. It does not reach the target (§1) and lacks part of
     the analyzed set — v1.16.x, #56, #57, and the judgment
     internals among them.
  2. **CI-red history.** Its CI red traces to upstream issue #42
     ("fresh checkout cannot build —
     `plugins/opencode/dist/index.js` is gitignored but required
     by `include_str!`"), now CLOSED upstream; the proven fix
     shape is upstream PR #48 (MERGED: "fix(ci): fail fast when
     an `include_str!` target is missing from the checkout").
     The history is explained, but it is a record of a vehicle
     built against a tree that could not build from a fresh
     checkout.
  3. **It also lacks the #43/#46 fixes present at the target.**
     Its tree, like develop, has no bundle path:
     `validate_bundle_record` appears 0× on its head (`2c866d5`),
     versus 6× at the target. Even as a partial vehicle it would
     arrive without the upstream bundle-gate and
     sign-after-preservation fixes (upstream PRs #53/#51) that
     the target carries (§4.2).
- **Its branch:** the resolution **recommends** retiring
  `merge/main-into-develop-1bfc129` — and executes nothing. Branch
  deletion follows Pratik's standing deletion rule: he names the
  exact target, gives explicit confirmation, a **1-hour enforced
  delay** passes, he **reconfirms**, and only then is it deleted.
  Until that flow runs, the branch stays where it is.

**T15a/T16 sequencing — resolved by events, no constraint
remains.** The question Milestone 0 absorbed (whether the sync
had to wait for, or be sequenced around, T15a/T16) is moot: fork
PR #24 (T15a) merged as `48bf626`, PR #25 (T16) merged as
`d2f87a1`, PR #26 (attestation fix) merged as `c7d8f20`, and
PR #27 (T19) merged as `5b8f9a7` — all already on `develop`, in
the census base's own ancestry. The sync proceeds against the
current develop tip, whenever it is authorized.

## 6. Upstream-push guard (deliverable, this PR)

**Files:** `scripts/hooks/pre-push` (guard added) +
`scripts/install-hooks.sh` (installer updated) — in the same PR
as this document.

**What already existed, and what changed.** Both files predate
this task: `scripts/hooks/pre-push` was the NeuroStrata task-gate
hook and `scripts/install-hooks.sh` installed it. This task does
**not** overwrite them — the upstream guard is added as the
**first** guard in the same `pre-push` (so a single installed hook
carries both), and the task gate below it is preserved verbatim.
The installer is updated for one correctness fix the guard's
worktree use exposes: it now resolves the hooks directory via
`git rev-parse --git-common-dir` (shared by all worktrees of the
clone) instead of `$root/.git/hooks` (in a linked worktree
`$root/.git` is a file, not a directory), while still honoring
`core.hooksPath` when set, since git itself honors it first. The
installer is idempotent (an identical destination is reported as
"Already installed" and left in place) and prints what it did.

**Guard behavior.** The hook reads the remote name (`$1`) and URL
(`$2`) git passes it, normalizes the URL (lowercase; strip scheme,
userinfo, trailing `/` and `.git`; scp-style `host:path` → 
`host/path`), and **rejects with exit 1** and a clear message when
the resulting path ends with the segment
`cognilogical/neurostrata` — on **any** host, so
`https://github.com/Cognilogical/NeuroStrata[.git]`,
`git@github.com:Cognilogical/NeuroStrata.git`,
`ssh://git@github.com/Cognilogical/NeuroStrata[.git]`, and the
same path on any mirror/proxy/bogus host are all rejected. The
fork (`pratik-saptarshi/NeuroStrata`) and every other remote
**pass** (exit falls through to the task gate). The guard runs
before the task gate, unconditionally, whether or not
`neurostrata-mcp` is installed, and it has no skip variable.
Install step (also in the hook's header comment):

```
bash scripts/install-hooks.sh
```

**Known limitations, recorded honestly:** (1) git rewrites a
remote URL through `url.<base>.insteadOf` config *before*
invoking `pre-push`, and the hook sees the rewritten URL
(verified in test 2c below). An `insteadOf` rule that rewrote an
upstream URL to a local path would therefore bypass the guard.
No such rule exists in this clone's config at writing time; the
guard covers the configured-remote surface, which is the
accidental-push risk it exists for. (2) *Review residuals —
unnormalized URL forms:* the guard normalizes case, scheme,
userinfo, scp form, trailing slash and one trailing `.git` —
and nothing else. Fed literally, these forms are **not**
normalized and the guard passes them (exit 0, re-verified in the
correction session): percent-encoded characters in the path
(`%43ognilogical/…`, `Cognilogical%2FNeuroStrata`), a mid-path
dot segment (`Cognilogical/./NeuroStrata`), a double `.git`
suffix (`NeuroStrata.git.git`), a query string
(`NeuroStrata.git?query=1`), and an extra path suffix
(`NeuroStrata/extra`, `NeuroStrata.git/extra`). (A dot segment
placed *before* the owner — `github.com/./Cognilogical/…` — is
still rejected, incidentally, because the normalized path keeps
the matching suffix; the residual is the mid-path forms.) These
residuals are assessed **safe in practice**: none of them is a
valid GitHub push endpoint for this repository as written —
reaching upstream through one requires deliberately crafting
the URL *and* a transport/server that normalizes it back into
the real path — and the disclosed `insteadOf` bypass in (1) is
strictly stronger than all of them. They are recorded here so
the guard's exact surface is known; the hook is deliberately
not extended to chase them.

**Tests run at writing time (2026-11-07), and their actual
outputs.** No test ever configured a remote pointing at the real
`github.com` upstream URL, and no test push could reach it.

1. *Direct invocation* — the hook called with fabricated
   `argv`/empty stdin (`bash scripts/hooks/pre-push testremote
   <url>`). In this shell `neurostrata-mcp` is not on `PATH`, so
   allowed pushes exit 0 after the gate's "not found" warning —
   the guard's own verdict is what the exit codes below measure:

   | Case | URL | Exit | Verdict |
   |------|-----|-----:|---------|
   | upstream https | `https://github.com/Cognilogical/NeuroStrata.git` | 1 | REJECTED (guard message printed) |
   | upstream https, no `.git` | `https://github.com/Cognilogical/NeuroStrata` | 1 | REJECTED |
   | upstream ssh (scp) | `git@github.com:Cognilogical/NeuroStrata.git` | 1 | REJECTED |
   | upstream ssh (URL) | `ssh://git@github.com/Cognilogical/NeuroStrata.git` | 1 | REJECTED |
   | upstream path, bogus host | `https://nonexistent.invalid/Cognilogical/NeuroStrata.git` | 1 | REJECTED |
   | upstream, lowercase | `https://github.com/cognilogical/neurostrata.git` | 1 | REJECTED |
   | fork https | `https://github.com/pratik-saptarshi/NeuroStrata.git` | 0 | allowed |
   | fork ssh | `git@github.com:pratik-saptarshi/NeuroStrata.git` | 0 | allowed |
   | unrelated repo | `https://github.com/someone-else/some-repo.git` | 0 | allowed |
   | similar name, not upstream | `https://github.com/Cognilogical/NeuroStrata-fork.git` | 0 | allowed |

   Guard message (verbatim, first case): "Push blocked by
   upstream-push guard: remote 'testremote' points at upstream
   Cognilogical/NeuroStrata (https://github.com/Cognilogical/NeuroStrata.git)."

2. *End-to-end in a scratch clone under `/tmp`* (bare "fork
   stand-in" + clone; hook installed via `scripts/install-hooks.sh`
   — first run "Installed …/hooks/pre-push", second run "Already
   installed (identical)", i.e. idempotent):
   a. `git push upstream` with upstream URL
      `https://nonexistent.invalid/Cognilogical/NeuroStrata.git`:
      **git failed at transport before the hook ran** — exit 128,
      "fatal: unable to access 'https://nonexistent.invalid/…':
      GnuTLS recv error (-110)". The push reached nothing (the
      host does not resolve to upstream; the environment's egress
      proxy terminated the TLS attempt), but this attempt does
      **not** demonstrate hook rejection, and is not claimed to.
      Git contacts the remote to list refs before invoking
      `pre-push`, so an unreachable remote never reaches the hook.
   b. Same scratch clone, upstream remote re-pointed at a **local
      bare stand-in whose path carries the upstream segment**
      (`/tmp/<scratch>/Cognilogical/NeuroStrata.git` — a local
      path, never a `github.com` URL; even with no hook, a push
      could only reach that scratch bare repo): `git push
      upstream` → **exit 1, rejected by the hook**, message:
      "Push blocked by upstream-push guard: remote 'upstream'
      points at upstream Cognilogical/NeuroStrata
      (/tmp/<scratch>/Cognilogical/NeuroStrata.git)." followed by
      git's "error: failed to push some refs". The stand-in bare
      repo held **zero refs** afterwards — nothing was transferred.
   c. Control for (b)'s `insteadOf` limitation: with
      `url.<local-bare>.insteadOf` rewriting the bogus-https URL
      to a local bare repo, the push **succeeded** (exit 0) —
      the hook saw the rewritten local URL. This is the
      limitation recorded above, demonstrated rather than asserted.
   d. `git push origin` to the local bare fork stand-in →
      **exit 0, succeeded** ("[new branch] HEAD -> main").
   The scratch directory was deleted after the outputs above
   were captured.

3. *Installed in the main clone.* `scripts/install-hooks.sh` was
   run from this worktree; because the installer resolves the
   hooks directory via `git rev-parse --git-common-dir`, that one
   run installed the hook for the whole clone at the main
   clone's **`.git/hooks/pre-push`**
   (verified byte-identical to this branch's
   `scripts/hooks/pre-push` via `cmp`; a second installer run
   reported "Already installed (identical)"). The main clone's
   `upstream` remote exists —
   `git -C <main-clone> remote get-url upstream`
   → `https://github.com/Cognilogical/NeuroStrata.git` — and
   rejection there is demonstrated by invoking the **installed**
   hook with that URL as a fabricated argument, **not** by a real
   push: installed hook + upstream URL → **exit 1** (guard
   message as in test 1); installed hook + the fork `origin` URL
   → **exit 0**.

## 7. Fallbacks, priced

If the full sync (§2) cannot land within the window Pratik sets,
there are exactly two fallback elections. Both are priced here;
neither is taken by this document.

### (a) Partial sync — cherry-pick / hand-port only what Phase 1 needs

**What Phase 1 (Tasks 2.1–2.12) actually consumes from upstream,
derived from the census:**

| Candidate | Needed by | Judgment |
|-----------|-----------|----------|
| `HeuristicJudgmentProvider` (`src/judgment/provider.rs`) | Task 2.6 sweep behavior as designed on the target base | **Include** — the only provider change Phase 1's design assumes |
| Shadow-verdict machinery (`src/judgment/mod.rs`: `last_shadow`, shadow invocation/recording in `check_duplicates`) | Task 2.6 (shadow records alongside the active verdict) | **Include** — inseparable from the provider change |
| `src/judgment/canonical.rs` (canonical duplicate key) | Dependency of the heuristic provider (its first matching stage) | **Include** — the provider does not compile/behave without it |
| Shadow-verdict consumption in `src/server.rs` | Surfacing shadow verdicts on the write path | **Partial** — port only if the M2 gate needs server-visible shadow output; otherwise the machinery records in-process and Task 2.6 tests consume it directly |
| `include_ast_edges` (`src/server.rs`) | Nothing in Phase 1 | **Exclude** — Phase-2-adjacent (retrieval surface for Tasks 3.x); porting it drags in the server/graph surface for no Phase 1 benefit |
| `src/fts/`, `src/lexical.rs`, `src/rdf/`, `src/community.rs`, `src/rebac.rs`, `src/did.rs` | Nothing in Phase 1 (Phase 2 prerequisites) | **Exclude** — Phase 2 still cannot start on a partial sync |
| Upstream's deletion of `src/events/outbox.rs` / bus rewrite | Nothing — Task 2.1 builds **on** develop's outbox | **Exclude, actively harmful** — importing it would remove the substrate Task 2.1 extends |

**Price.** The included set is small in file count — the judgment
subtree, ~3 files plus optionally a slice of `server.rs` — and
that is where the honest answer turns: **every included file is
on the `merge-tree` conflict list and was changed on both sides**
(`judgment/mod.rs` 267 two-tip churn, `provider.rs` 166,
`canonical.rs` both-changed, `laya_onnx.rs` 699 if its wiring is
touched, `server.rs` 6,243 if shadow consumption is ported).
Upstream's judgment changes are not an isolated cherry-pickable
commit series — they are spread across the 773-commit divergence
— so "cherry-pick" in practice means **hand-port**: doing, for
these files, the same semantic conflict resolution the full sync
does for them, then validating a **third judgment configuration
that exists on neither base** (develop's checker wiring +
upstream's provider internals). Estimated price: **on the order
of half to most of the full sync's judgment/server resolution
effort, plus 2–4 `test (ubuntu)` iteration runs and 1 full CI
matrix run, plus a census re-derivation for the judgment rows** —
while buying **none** of the full sync's other returns: the
remaining ~150 differing `src/` files stay diverged, Phase 2
stays blocked, and the full sync (with these same files resolved
*again*, this time against the hand-ported hybrid) is still owed
later. **Priced conclusion: for the one primitive Phase 1 needs,
partial-sync cost approaches full-sync cost; as a program move it
is strictly worse than either the full sync or election (b).**
Its only defensible use is as a time-boxed unblock if Pratik
needs Task 2.6's designed behavior before any sync window exists
at all.

### (b) Build-on-current-base election — build Phase 1 on `5b8f9a7`

(The plan's gate text says "build on `6ce7021`"; the census
reconciled that SHA as an ancestor — `6ce7021` is the
binaries-drop commit 4 commits back from the current tip. The
election, if taken, is to build on the **current develop tip**,
`5b8f9a7` at this writing.)

**Immediate price: ~zero.** No sync, no extra CI beyond Phase 1's
own runs; Phase 1 branches as soon as Pratik records the election.

**Deferred price — the forward-merge grows, in three compounding
ways:**

1. **Phase 1 edits land in already-conflicted files.** Tasks
   2.1–2.11 modify `src/events/bus.rs` (1,432 churn, conflicted;
   upstream rewrote it *down* by 1,355 deletions),
   `src/events/outbox.rs` (see 2 below), `src/server.rs` (6,243,
   conflicted), `src/judgment/mod.rs` (267, conflicted), and
   `src/main.rs` (3,148, conflicted — the `consolidate`
   subcommand). Every Phase 1 hunk in these files is a hunk the
   later merge must resolve *in addition* to today's 164
   conflicts — the conflict **count** barely moves (the files are
   already conflicted), but the conflict **volume and semantic
   load** grows by roughly the size of Phase 1's shared-file
   edits. Planning estimate: **the eventual sync costs ~30–60%
   more resolution effort than doing it now**, before items 2–3.
   The new `src/consolidation/*` files are the exception: new
   files on develop's side are low-conflict (add-only against a
   target that has no such directory) and survive the merge
   essentially intact.
2. **The `outbox.rs` deletion turns a merge into a redesign.**
   Task 2.1/2.2 build the consolidation event class and watermark
   on develop's outbox sequencing. Upstream **deleted** that
   file. A later sync therefore does not rebase Phase 1's event
   work — it requires **re-implementing Tasks 2.1–2.2's substrate
   on upstream's bus shape**, with its tests re-proven. That is
   rework of two tasks, not a conflict-resolution surcharge, and
   it is the single largest line in this fallback's deferred
   price.
3. **Evidence expires.** Phase 0 baselines and the Task 0.1
   census are base-specific. Building on `5b8f9a7` and syncing
   later means the census re-run (§2 step 5b) and any
   base-sensitive Phase 0 numbers are re-done at sync time
   anyway — the M0 gate's "(a) sync landed + census re-run"
   condition is deferred, not discharged.

**Behavioral caveat (must be attached to the election, not
discovered later):** on this base, Task 2.6's sweep runs against
develop's judgment reality — `MockJudgmentProvider` in the active
slot for `Auto`/`Heuristic`/`LayaLocal` modes, which returns "no
duplicates" unconditionally (census rows 2b–2g, flag F2), with
**no** shadow machinery. Task 2.6's tests can still pass (they
script providers), but the production sweep is a **no-op judge**
until the sync, and any M2-gate precision/recall measured on this
base is measured against a different judge than the post-sync
engine — the numbers are **not comparable** across the sync, and
the gate evidence may need re-running. Phase 2 additionally
**cannot start** on this base at all (plan Phase 2 dependency:
there are no three search surfaces to fuse).

## 8. The M0 hard gate

> **Phase 1 may not branch until (a) the sync to `8fdae4b` has
> landed on `develop` and the census + baselines are re-run on
> the new base, OR (b) Pratik's recorded election for a priced
> fallback — partial sync (§7a) or build-on-current-base (§7b) —
> with the priced cost accepted.**
>
> "Disposition recorded" alone does not open the gate. Sync
> execution itself proceeds only under Pratik's separate
> authorizations: the sync PR's merge is his per-PR word, as is
> every other merge and filing in this program. This document —
> and the branch carrying it — authorizes nothing, pushes
> nothing, and files nothing against upstream; the guard in §6
> exists to keep it that way mechanically.
