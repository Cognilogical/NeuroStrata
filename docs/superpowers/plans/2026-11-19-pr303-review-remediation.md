# PR #303 Review Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the remaining staged-quality, CI, and taxonomy review findings without changing the gate's staged-only contract or weakening Kani coverage.

**Architecture:** Materialize the complete Git index into a temporary directory and run every quality tool there, so `git add -p` validates exactly staged content while the real index/worktree remain untouched. Add focused Clippy dep-info coverage and extensionless Bash classification in that snapshot. Preserve the bounded Kani DAG branch, but use a BTreeMap-backed iterative Kahn DAG and shared adjacency reachability for production Rust.

**Tech Stack:** Bash 3.2-compatible shell, Git index/checkout-index, Node.js built-ins, Rust std collections, Kani 0.68.0, GitHub Actions.

**Spec:** Owner-approved five-part scope in task `neurostrata-0220`; PR #303 review findings through `239ce982`.

## Global Constraints

- No package installation, `npx`, Python tooling, unrelated rewrites, rebase, or force-push.
- Preserve fail-closed behavior, staged-only checks, standalone Kani dispatch, and pre-write CAS semantics.
- Minimal changes; existing fixture and mutation suites remain the regression contract.
- Pratik is a reviewer, not a parallel implementation lane; this task owns all remaining work.

## Review Focus

- Paths with spaces, backslashes, Unicode, symlinks, renames, and deleted files must remain safe (Task 1 fixtures).
- Empty or partially staged indices must never leak worktree content into tool results (Task 1 fixtures).
- A clean-looking but unbuilt staged `.rs` file must fail rather than silently skip Clippy (Task 2 unit/integration tests).
- Duplicate nodes, disconnected cycles, self-loops, and empty graphs must retain exact DAG/SKOS verdicts (Task 4 tests and Kani).
- CI must execute the suites as hard gates, not optional/skipped steps (Task 3 YAML/static check plus real CI).

---

### Task 1: Exact staged-content snapshot and extensionless Bash

**Files:**
- Modify: `scripts/check-staged-quality.sh`
- Modify: `scripts/test-staged-quality.sh`

**Interfaces:**
- Consumes: Git staged paths from `git diff --cached --name-only --diff-filter=ACMR -z`.
- Produces: a temporary full-index checkout; all existing tools receive snapshot paths or run with the snapshot as their root, while the caller index/worktree stay byte-identical.

- [ ] **Step 1: Replace the divergence-fail fixture with exact partial-staging assertions.** Stage badly formatted Rust, leave formatted worktree content, and require failure from the staged blob; stage formatted content, leave bad worktree content, and require success. Add an extensionless executable hook with Bash shebang and a ShellCheck failure, requiring rejection and proving the hook reaches ShellCheck.
- [ ] **Step 2: Run `bash scripts/test-staged-quality.sh`.** Expected: new partial-staging and extensionless-hook fixtures fail; previous 18 fixtures otherwise remain valid apart from the intentionally replaced fallback test.
- [ ] **Step 3: Implement snapshot setup before classification/tools.** Use `mktemp -d`, a status-preserving `EXIT` trap, and `git checkout-index --all --prefix=<snapshot>/`; fail closed on checkout errors. Remove the index/worktree divergence rejection. Run Rustfmt, Cargo, Prettier, Bash, and ShellCheck against snapshot paths, using absolute original tool binaries/configuration where dependencies are untracked. Detect `.sh` plus regular files whose staged first line is `#!/bin/bash`, `#!/usr/bin/bash`, `#!/usr/bin/env bash`, or `#!/bin/sh`/`env sh` (ShellCheck dialect from shebang); extensionless Bash also gets `bash -n`.
- [ ] **Step 4: Run focused fixtures then the full suite.** Run `bash scripts/test-staged-quality.sh`; expected all fixtures PASS. Run `bash -n scripts/check-staged-quality.sh scripts/test-staged-quality.sh` and `shellcheck -x scripts/check-staged-quality.sh scripts/test-staged-quality.sh`; expected success.
- [ ] **Step 5: Review diff and commit after owner authorization.** Include only the two scripts and any plan-checkbox updates.

### Task 2: Reject staged Rust files absent from the build graph

**Files:**
- Modify: `scripts/clippy-staged-diagnostics.mjs`
- Modify: `scripts/test-clippy-staged-diagnostics.mjs`
- Modify: `scripts/check-staged-quality.sh`
- Modify: `scripts/test-staged-quality.sh`

**Interfaces:**
- Consumes: optional `--dep-info-root <directory>` CLI argument; dep-info is Make-syntax `.d` output under an isolated `CARGO_TARGET_DIR`.
- Produces: exported `findUnbuiltStagedRustFiles(depInfoTexts, root, stagedPaths): string[]`; CLI prints one actionable error per unbuilt staged `.rs` and exits 1 even when no diagnostic matches.

- [ ] **Step 1: Add failing unit tests.** Cover built `src/main.rs`, an unbuilt `fixtures/input.rs`, multiple dep-info targets, escaped spaces, absolute/symlink-normalized roots, ignored `Cargo.toml`, and coexistence with ordinary Clippy diagnostics. Add an integration fixture whose fake Cargo emits no dep-info for a staged `.rs` and require fail-closed output.
- [ ] **Step 2: Run `node scripts/test-clippy-staged-diagnostics.mjs` and `bash scripts/test-staged-quality.sh`.** Expected: only new coverage tests fail, with missing export/dep-info behavior.
- [ ] **Step 3: Implement dep-info parsing and reporting.** Recursively read `.d` files with Node built-ins, split Make targets/dependencies with escaped-space handling, normalize paths through the existing path helper, and compare only staged `.rs` paths. Set an isolated `CARGO_TARGET_DIR` for the Clippy invocation, pass `--dep-info-root`, and make missing/empty dep-info for any staged Rust file a loud failure, never a skip.
- [ ] **Step 4: Run both suites and real Clippy.** Expected all fixture suites PASS; then `cargo clippy --workspace --all-targets --locked --message-format=json` succeeds with existing project warnings policy unchanged.

### Task 3: Execute all quality and mutation suites in CI

**Files:**
- Modify: `.github/workflows/ci.yml`
- Test: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: Ubuntu `test` job's installed Kani wrapper and `scripts` job's Node/Bash environment.
- Produces: hard-gating steps for staged-quality 18+, Clippy diagnostics 24+, and Kani six-mutant rejection.

- [ ] **Step 1: Add a static workflow self-check to the script fixture assertions.** Require exact step commands `bash scripts/test-staged-quality.sh`, `node scripts/test-clippy-staged-diagnostics.mjs`, and `NEUROSTRATA_REQUIRE_KANI=1 bash scripts/kani-mutation-check.sh`, without `continue-on-error` on those steps.
- [ ] **Step 2: Run the static check.** Expected failure because the workflow does not contain all three commands.
- [ ] **Step 3: Wire the commands.** Put both quality suites in `scripts (ubuntu)` and mutation checking immediately after Kani installation in `test (ubuntu)` so the configured wrapper/toolchain is reused. Do not add packages or duplicate Kani installation.
- [ ] **Step 4: Verify locally and in GitHub Actions.** Run all three commands locally; expected 18+/18+, 24+/24+, six mutants rejected. After publication, require the new-head Ubuntu and scripts jobs SUCCESS and read their logs for suite summaries.

### Task 4: Linear production DAG with shared semantic SKOS reachability

**Files:**
- Modify: `src/formal_kernel.rs`
- Modify: `scripts/kani-mutation-check.sh`

**Interfaces:**
- Consumes: existing public signatures `validate_taxonomy_dag(&[(u32,u32)]) -> bool` and `validate_skos_disjointness(&[(u32,u32)], &[(u32,u32)], &[(u32,u32)]) -> bool`.
- Produces: non-Kani `TaxonomyGraph` using `BTreeMap<u32, usize>`, adjacency lists, indegrees, iterative Kahn validation, and adjacency-based transitive reachability shared by related/contrast checks; Kani retains the small bounded implementation and exact proofs.

- [ ] **Step 1: Add failing regression/performance-scale tests.** Cover duplicate edges, disconnected acyclic components, disconnected cycles, long (2,000-edge) chains, transitive related/contrast conflicts in both directions, and unknown endpoint IDs. Assert the chain completes within a generous regression ceiling that the O(E²) implementation cannot meet locally without making CI timing flaky.
- [ ] **Step 2: Run focused Rust tests.** Run `cargo test --bin neurostrata-mcp formal_kernel::tests::test_validate -- --nocapture`; expected the scale test fails/times out and semantic cases remain red/green as appropriate.
- [ ] **Step 3: Implement the production graph and preserve Kani.** Build each node once, sort through BTreeMap indexing, append adjacency/indegree per edge, process all zero-indegree nodes exactly once, and reject self-loops or processed-count mismatch. Reachability traverses the same adjacency with visited nodes. Gate production and bounded Kani implementations with `cfg` so public semantics are identical.
- [ ] **Step 4: Update the DAG mutant to match the new Kani guard.** Mutate the actual bounded cycle check, retain the guard that requires the mutation to apply, and keep the other five mutants unchanged.
- [ ] **Step 5: Run the complete verification set.** Run focused tests, `cargo test --bin neurostrata-mcp formal_kernel::tests -- --test-threads=1`, Rustfmt, Clippy, standalone 28/28 Kani, and `NEUROSTRATA_REQUIRE_KANI=1 bash scripts/kani-mutation-check.sh` (6/6). Expected all pass.

### Task 5: Independent review, publication, and closure evidence

**Files:**
- Modify only if findings require: files above.

- [ ] **Step 1: Run all changed-area gates.** Expected staged-quality, Clippy, Rust, shell, Kani, and mutation suites all PASS with no generated/source mutations.
- [ ] **Step 2: Obtain independent code review.** Supply the complete diff, requirements, and test evidence; resolve every Critical/Important finding and re-run affected gates.
- [ ] **Step 3: Publish normally after owner authorization.** Use ordinary commits and `git push` only; no rebase/force-push. Require clean status, empty stash list, passing task gate, and successful push.
- [ ] **Step 4: Verify PR head CI and summarize closure.** Read the exact new-head job logs. Report every original concern as independently fixed/verified, with no silent deferrals.
