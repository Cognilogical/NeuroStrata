# Wiring Panel — Gate Prescription in `bootstrap` / `task_setup`

Status: implemented (ship-now scope, task neurostrata-majk). Answers the guinea pig's GATES-TO-PRESCRIBE ask (source document since removed; findings archived in task memories). Constraint honored: git-first, never git-only. Rule in force: never hand a project a reminder where a machine will do.

> **Recipes:** [`docs/architecture/prescribed-wire-recipes.md`](architecture/prescribed-wire-recipes.md)
> documents each `project-pipeline` wire a project implements itself — A2, A3,
> A4, A5-residual, A7, plus the A8 `gate-self-test` harness — with rationale,
> shape, keywest.health reference impls, and acceptance criteria. Start there
> when wiring a project up; this document stays the design of record.

---

## Q1 — The `wiring` block schema

One block added to the `bootstrap` / `task_setup` result, beside `instructions`:

```json
{
  "wiring": {
    "automatic": [
      {
        "id": "task-close-lock",
        "runs_in": "core",
        "blocks": true,
        "gate_point": null,
        "reason": "done requires an EXTRACTED_FROM edge; enforced in task_complete",
        "reference": null
      },
      {
        "id": "memory-to-repo-drift",
        "runs_in": "project-pipeline",
        "blocks": true,
        "gate_point": null,
        "reason": "paths in memories rot silently as files move",
        "reference": "keywest.health: scripts/ci/memory-repo-drift (reference impl available)"
      }
    ],
    "reminders": [
      {
        "id": "status-before-daemon",
        "fires_on": "mcp-call-failure",
        "text": "run `neurostrata-mcp status` — do NOT start a daemon"
      }
    ],
    "coverage": {
      "core": ["task-close-lock", "claim-exclusivity", "stale-claim-expiry",
                "zero-action-start", "single-daemon-lock", "rule-honesty"],
      "git-pre-push": ["task-gate"],
      "project-pipeline": ["memory-to-repo-drift", "repo-to-memory-drift",
                            "directive-preservation", "export-freshness",
                            "supersede-hygiene", "gate-self-test"],
      "uncovered": []
    }
  }
}
```

Field contract:

- **`id`** — stable kebab-case wire id. The registry is closed: ids are
  defined by NeuroStrata, not per-project.
- **`runs_in`** — enum of **exactly four**, closed:
  | value | meaning |
  |---|---|
  | `core` | NeuroStrata enforces it itself (tool, error path, lock). No project action. |
  | `git-pre-push` | default instance, only emitted when `detected.git == true`. |
  | `project-pipeline` | the project's own chokepoint — CI job, CMS publish step, render submission, deploy script. **Generic on purpose.** The project names the actual point in `gate_point`; we never enumerate CMSes or render farms. |
  | `agent-reminder` | no machine exists; the prompt is the mechanism. Last resort category. |

  Four values, not a taxonomy. "project-ci" from the guinea-pig doc folds
  into `project-pipeline`; CI is just the software project's pipeline.
- **`blocks`** — automatic wires only. `true` = hard fail, `false` = warn.
- **`gate_point`** — `null` until resolved. Setup resolves it: `git` detected
  → `"git-pre-push (via neurostrata-mcp hooks install)"`; no git → the agent
  MUST name the project's publish/ship step and record it as a `rule` memory.
  A `project-pipeline` wire with unresolved `gate_point` is reported in
  `coverage.uncovered` — nothing silently unowned.
- **`reference`** — pointer to a known reference implementation, or null.
- **`fires_on` / `text`** — reminders only. `fires_on` is a closed enum too:
  `mcp-call-failure | memory-added-or-superseded | memory-corrected |
  session-start | before-edit | during-work`.
- **`coverage.uncovered`** — computed: every automatic wire whose `runs_in`
  is `project-pipeline` and whose `gate_point` is null. Empty array is the
  success state; non-empty is the setup telling the agent "this gate has no
  home yet — give it one."

Minimal-entropy justification: responsibility (`runs_in`), teeth (`blocks`),
and ownership gap (`uncovered`) are the only three properties the guinea pig
proved matter. Everything else is text.

---

## Q2 — The instruction template (one, parameterized)

Not two scripts. One template, two filled slots, git branch first and dominant:

```json
{
  "step": "<n>",
  "action": "wire_gates",
  "gate_point": "<resolved-or-null>",
  "text": "Gate these checks at {gate_point}: {gate_ids}. {resolve_clause}"
}
```

`resolve_clause`, git detected (the default, stated first, always):

> "Git detected — the default and recommended instance, because a git hook is
> the one chokepoint that works on EVERY git project: run
> `neurostrata-mcp hooks install` to put the task gate on pre-push (the gate
> does not move until that hook is installed and runs `task gate --strict`).
> The `files` field of this instruction carries a runnable
> `scripts/hooks/pre-push` and `scripts/install-hooks.sh`; write them,
> install the hook, then extend that hook with the project-pipeline gates
> above — they belong in the hook, not in a workflow. CI is optional and
> vendor-specific: if this project already has GitHub Actions, GitLab CI or
> similar, re-run the project-pipeline gates there as a defense-in-depth
> second check; a git project with no CI needs nothing beyond the hook.
> Verify with the `verified` block in this payload."

### What "git-first" means in the application (FINDING-bootstrap-wiring-nudge)

**Git-first** is a claim about *breadth*, not about git's popularity. A pre-push
hook is the broadest chokepoint that exists: it runs on every push, on every
host — GitHub, GitLab, Gitea, a bare repo over SSH — and it needs nothing from
the project but git itself. CI is the narrower claim: it is vendor-specific
(GitHub Actions, GitLab CI, Circle, …), it only runs after the push, and a
non-GitHub git project cannot use it at all.

So the required default for a git project is the **hook**, and CI is the
**optional secondary** — a defense-in-depth second check for projects that
already have one. The reverse framing biases the ecosystem: pushing agents
toward CI means GitHub becomes the default target and everyone else is locked
out.

The 1.7.0 template got this exactly backwards. It said "Git detected — the
default and recommended instance" and then, in the same breath, told the agent
to "wire the project-pipeline gates above into your CI". The principle was
right; the application pointed at the narrower solution. The guinea pig
(keywest.health, 2026-10-09) landed 7 architecture gates in
`.github/workflows/` and 0 in pre-push, having followed the instruction
perfectly: where every existing guard already lives is the loudest signal, and
"add a GitHub Actions workflow" is always the path of least resistance.

Two corrections keep the principle and the application in agreement:

1. **Ship the hook, don't describe it.** `wire_instruction.files` carries a
   runnable `scripts/hooks/pre-push` (with a `PROJECT_GATES` extension point)
   and `scripts/install-hooks.sh`. If the hook is already a file in the tree,
   the agent extends it instead of inventing a workflow. This is the only
   intervention that actually flips the path of least resistance — telling a
   project to "wire a pre-push hook" does not, because writing a CI workflow
   is easier than writing a hook.
2. **Never report a guard it cannot see (Q5).** `hook_installed` is read from
   the hook git actually runs — `.git/hooks/pre-push`, or `core.hooksPath`
   when the project set one. A `git-pre-push` guard with no observable hook
   stays in the must-wire list (`coverage.uncovered`) and is never subtracted
   as verified. `verified.hook_project_guards` lists the uncommented
   `PROJECT_GATES` entries parsed from the installed hook, so an agent can
   tell which project guards actually run client-side (and which the
   vendor-specific CI, if any, is the only thing running).

`resolve_clause`, no git (same sentence shape, same breath):

> "No git detected. Identify the ONE step every unit of work must pass to
> leave this project — a CMS publish action, a render-farm submission, a
> publish script, a review-approval step. Wire
> `neurostrata-mcp task gate {namespace} --strict` (exit 1 = blocked) and the
> project-pipeline gates above into that step so nothing ships past an
> unresolved gate. Record the chosen step: `neurostrata_add_memory` with
> `memory_type: rule`, content naming the gate point — the next session must
> not have to re-derive it."

Invariants of the pattern:

1. The verb is always "gate at {gate_point}"; only the slot filling changes.
2. Git is named first and called the default. Non-git is the same instruction
   with a discovery clause, not a separate document.
3. Every non-git resolution ends by writing the gate point INTO memory —
   turning a per-project decision into a first-line rule (this is the memory
   system doing its job, not docs doing it).
4. `task gate` is transport-agnostic: it is a CLI with exit codes, so a CMS
   publish hook, a pre-push hook, and a CI step all call the identical binary.

---

## Q3 — Core vs project-side

Test applied: *is the enforcement project-agnostic?* Core ships what needs no
knowledge of the project's files or workflow; anything that reads project
content or knows what a "ruling" or a "publish" is for THIS project is
prescribed, never shipped.

| Wire | Home | Why |
|---|---|---|
| done-funnel / claim exclusivity / stale expiry / Zero-Action Start / pre-push task gate / single-daemon lock | **core (shipped)** | pure task-graph invariants; no project knowledge |
| **A6 rule honesty** | **core (new)** | vocabulary change: `memory_type: rule` gains `enforcement: ENFORCED\|PARTIAL\|NOT_ENFORCED`, `source: "<ruling ref + date>"`, `guard: "<wire id or null>"`. A rule claiming ENFORCED with a null or dangling `guard` fails `task_validate`. Applies to a CMS rule exactly as to a coding rule. |
| **R1 status-before-daemon** | **core (new)** | it is error TEXT on our own failure paths. Every MCP/CLI failure that smells like daemon-down prepends: "run `neurostrata-mcp status` — do NOT start a daemon." One string, highest leverage in the doc. |
| **A10 setup verifies** | **core (new)** | see Q5. |
| export fidelity (A1/A5 collapse) | **core (shipped)** | export now carries metadata, EXTRACTED_FROM, superseded markers. A1 shrinks to "commit the export" — absorbed by R2 + A4. A5 shrinks to a thin check on the export's own markers. |
| **A2 memory→repo drift** | project-side, core-enabled | core provides the query surface (export/list carries locations); only the project knows its filesystem. Prescribed with reference impl. |
| **A3 repo→memory drift** | project-side | "dated owner ruling in tracked docs" is un definable generically — for a CMS it is a published policy page, for code a runbook. Core cannot parse it. |
| **A4 export freshness** | project-side | freshness is a property of a COMMITTED artifact; committing is the project's VCS/pipeline. Warn-only per the doc. *As of 1.8.0 the core ships `crate::events::check_export_freshness(store)`, the typed reader of the `ExportFreshnessDirty` subscriber's per-namespace flag row; the project's gate imports it and runs the same `Err`-on-dirty shape rather than reinventing the predicate.* |
| **A5 supersede hygiene** (residual) | project-side | scans the exported JSON for `superseded`/`superseded_by` markers, fails the push if a superseded id is still referenced as live. `export-graph` writes those markers and offers `--exclude-superseded`, so the project ships the predicate, not a hand-maintained list. Checks A–E in [`prescribed-wire-recipes.md`](architecture/prescribed-wire-recipes.md). |
| **A7 directive preservation** | project-side | verbatim-preservation of rulings requires knowing where rulings live. Core records rulings as memories; protecting repo copies is the project's gate. |
| **A8 gate rot** | contract in core, runner project-side | see Q4. |
| R2–R6 | core (shipped/shipping as text) | snapshot injection, done-funnel error text, supersede tool semantics. |

Deliberately NOT in core: any check that opens project files, any per-CMS /
per-render-farm adapter, any per-language CI generator. Universal core +
client shims; the wiring panel IS the shim-prescription mechanism.

---

## Q4 — A8: does a generic gate-self-test ship?

Split verdict:

1. **The runner does NOT ship.** Planting violations means mutating real
   project files with per-file backup and byte-identical restore — the guinea
   pig corrupted two files building exactly that. A generic file-mutating
   runner inside a memory/tasking system is blast radius beyond remit.
2. **The contract DOES ship**, as documented convention:
   - every gate (ours or prescribed) exits `0` pass / `1` violation found /
     `2` infra error — already the `task gate` convention, now stated as THE
     gate contract all prescribed wires must follow;
   - every gate SHOULD accept `--self-test`: exit 0 iff a planted violation
     makes the gate exit 1 AND the tree is byte-identical afterward.
3. **Core self-tests its own gates.** `neurostrata-mcp task gate --self-test`
   runs the gate engine against a synthetic fixture namespace (an
   unextracted `done` task, a claimed task, a rotting P0) and asserts exit 1
   with the expected violations. Our gate proves it is a gate; project gates
   prove theirs with the same flag shape, implemented project-side (reference
   impl exists).

So A8 becomes: contract + flag convention in core; per-gate self-test
implementations prescribed project-side; the wiring panel lists
`gate-self-test` as a `project-pipeline` wire covering all of them.

---

## Q5 — Setup verifies (A10)

Setup gains a `verified` block, checked server-side at call time:

```json
{
  "verified": {
    "hook_installed": true,
    "status_healthy": true,
    "export_fresh": null,
    "gate_point_resolved": false,
    "hook_project_guards": ["./scripts/rules-gate.sh"]
  }
}
```

Rules:

- `null` = not applicable here (e.g. no export path in a non-git project).
- **`hook_installed` is read from the hook git actually runs**: `.git/hooks/pre-push`,
  or `<core.hooksPath>/pre-push` when the project moved its hooks dir. A project
  that relocated its hooks is hooked, not unhooked; reporting it unhooked would
  send the agent to wire a second, redundant gate.
- **`verified: true` only ever subtracts.** A guard whose `runs_in:
  'git-pre-push'` is not backed by an observable hook cannot report verified:
  it stays in `coverage.uncovered` and in the instruction list. The panel does
  not believe a claim it cannot read.
- **Instructions subtract, never add.** Anything `verified: true` is REMOVED
  from `instructions` — setup instructs only what it could not confirm. The
  guinea pig's failure (payload said `existing_hooks: []` beside an install
  instruction) becomes structurally impossible: presence/absence is one field
  consumed by one filter.
- `tasks_created` follows the same filter: no "install the hook" task when
  the hook exists. A `gate_point_resolved: false` in a non-git project
  creates exactly one task: "Identify and wire the project gate point,
  record as rule memory."
- Setup stays side-effect-light: verification is read-only (stat
  `.git/hooks/pre-push`, run `status` probe, stat export mtime). Re-runnable
  and idempotent — calling setup again is how a project re-verifies.

Shape delta: `instructions` entries gain `"kind": "verified" | "action"` so
the agent can report the verified set without executing it.

---

## Q6 — Rollout

**Ship now** (small, high-leverage, no new machinery):

1. **R1 error text** — prepend status-first guidance to daemon-failure
   messages. One string; kills the most dangerous wrong action.
2. **`wiring` block** in `bootstrap` / `task_setup` — static wire registry +
   the Q1 schema + `coverage.uncovered` computation. Output-only change.
3. **A10 verification filter** — `verified` block + instruction subtraction.
4. **Q2 instruction template** — replaces the current hook-install step text;
   git-first, parameterized.
5. **A6 core fields** — vocabulary v3: `enforcement`, `source`, `guard` on
   `memory_type: rule`; `task_validate` flags ENFORCED-without-guard.

**File as tasks** (real work, not this session):

- `task gate --self-test` (Q4.3) + gate-contract doc section.
- Prescribed-wire recipes for A2/A3/A4/A5-residual/A7, pointing at the
  guinea-pig reference implementations (docs page, not code).
  **Done:** [`docs/architecture/prescribed-wire-recipes.md`](architecture/prescribed-wire-recipes.md).
- `task_validate` named-category exposure (already filed per findings).

**Killed** (complexity without benefit):

- **A1 as a prescribed gate** — export carries edges/supersede markers now;
  it collapses into R2 + A4. Do not ship a check for a bug we fixed.
- **`runs_in` taxonomy beyond four values** — enumerating CI systems, CMSes,
  render farms is a taxonomy project; `gate_point` free text carries it.
- **Generic gate-rot runner in core** — Q4.1; remit and blast radius.
- **Per-project `wiring` overrides in the DB** — the registry is static;
  per-project state is exactly one rule memory naming the gate point.

---

## Rejected alternatives (one line each)

- **Enum-explosion `runs_in` (`cms-publish`, `render-submit`, …)**: a taxonomy
  project that ages instantly; `project-pipeline` + free-text `gate_point`.
- **Two instruction scripts (git / non-git)**: divergence guaranteed; one
  template, one slot, git named first.
- **Core CI generators per platform**: client-shim violation; we prescribe
  wires, projects wire them.
- **Ship the A8 mutation runner**: a test that destroys what it tests, at
  generic scale, inside a memory system — no.
- **A2/A3/A7 as core checks**: require parsing project docs/filesystems;
  core stays project-agnostic, query surface is the enablement.
- **Keep A1/A5 full gates**: downstream of the export-fidelity fix we
  shipped; gating a fixed bug is theatre (A8 applies to us).
- **`gate_point` as closed enum**: same taxonomy trap; free text + one rule
  memory is the entire mechanism.
- **Setup writes/verifies by mutation**: verification is read-only; repo
  mutation stays with `hooks install` or the agent.
