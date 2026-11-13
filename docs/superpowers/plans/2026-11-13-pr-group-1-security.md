# PR Group 1: Security & Dependabot Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Eliminate polynomial ReDoS in `extractRepoFilePaths` (`#131`, `#58`), rebuild all 5 plugin `dist/index.js` bundles, and bump vulnerable dependencies in `src-tauri/Cargo.lock`, `plugins/opencode/package.json`, and `web-ui/package-lock.json` (`#70`).

**Architecture:** Replace the backtracking regex in `plugins/core/index.ts` with a single-pass token scanner, rebuild `plugins/*/dist/index.js`, and apply targeted dependency upgrades for `rustls`/`glib`, `vitest`, and `minimatch`.

**Tech Stack:** TypeScript, Node.js (`node --test`), Cargo (`src-tauri`), npm (`web-ui`, `plugins/*`).

---

### Task 1: Replace `extractRepoFilePaths` regex with linear-time scanner and rebuild bundles

**Files:**
- Modify: `plugins/core/index.ts:1280-1305`
- Modify: `plugins/core/test.mjs`
- Regenerate: `plugins/core/dist/index.js`, `plugins/claude/dist/index.js`, `plugins/codex/dist/index.js`, `plugins/github-cli/dist/index.js`, `plugins/opencode/dist/index.js`

- [ ] **Step 1: Write the failing/regression test in `plugins/core/test.mjs`**
  Add a test that runs `extractRepoFilePaths("a/".repeat(25000) + "!")` and asserts elapsed time `< 50` ms, plus verifies normal repo paths (`src/store/ladybug.rs`, `plugins/core/index.ts`, `.github/workflows/ci.yml`) are extracted and URLs (`https://example.com/a/b.ts`) are ignored.
- [ ] **Step 2: Implement linear-time token scan in `plugins/core/index.ts`**
  Scan whitespace/delimiter-separated tokens (`/[A-Za-z0-9._/-]+/g`), strip trailing `.`, validate at least one `/` with valid directory segments and known extension `.([A-Za-z0-9]{1,8})`, and skip tokens preceded by `://`.
- [ ] **Step 3: Rebuild all 5 plugin bundles and run `node --test plugins/core/test.mjs`**

### Task 2: Bump Dependabot targets (`#70`)

**Files:**
- Modify: `src-tauri/Cargo.toml` / `src-tauri/Cargo.lock`
- Modify: `plugins/opencode/package.json`
- Modify: `web-ui/package.json` / `web-ui/package-lock.json`

- [ ] **Step 1: Update `rustls` in `src-tauri/Cargo.lock` (`cargo update -p rustls --manifest-path src-tauri/Cargo.toml`) and update/remove `glib` to `>= 0.20`**
- [ ] **Step 2: Update `vitest` in `plugins/opencode/package.json` to `^4.1.2`**
- [ ] **Step 3: Update `minimatch` in `web-ui/package-lock.json` to `>= 10.2.3`**
