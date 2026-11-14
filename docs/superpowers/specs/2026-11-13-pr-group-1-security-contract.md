# PR Group 1: Security & Dependabot Fixes — Contract (#169)

Status: Consolidated remediation for `#131`, `#70`, `#58` against `main` (`9e3b2fb`).

## Contracts

### C1 — Linear-time repo file-path extraction (`#131`, `#58`)
`extractRepoFilePaths(text: string)` in `plugins/core/index.ts` extracts repo-relative file paths without nested regex quantifiers (`(?:[A-Za-z0-9._-]+/)+`) or backtracking character classes. Path tokenization operates in $O(n)$ time with respect to `text.length`, and all 5 harness plugin `dist/index.js` bundles (`plugins/core`, `plugins/claude`, `plugins/codex`, `plugins/github-cli`, `plugins/opencode`) are regenerated cleanly from `plugins/core/index.ts`.

### C2 — Adversarial ReDoS regression budget (`#131`)
`plugins/core/test.mjs` includes a regression test feeding an adversarial 50,000-character slash/alphanumeric non-matching string (`"a/".repeat(25000) + "!"`) to `extractRepoFilePaths`, completing in `< 50ms` while preserving existing path extraction semantics.

### C3 — Dependabot advisory resolution (`#70`)
- `src-tauri/Cargo.lock`: `rustls` upgraded to `>= 0.23.37` (`0.23.38`); `glib 0.18.5` is transitively pinned by upstream `tauri v2` (`gtk 0.18` / `webkit2gtk 2.0.1` Linux bindings) and cannot be bumped to `0.20` without an upstream Tauri GTK crate release.
- `plugins/opencode/package.json`: unused `vitest` devDependency removed (preserving `"node": ">=18.0.0"` compatibility while eliminating the `vitest` advisory surface).
- `web-ui/package-lock.json`: `minimatch` resolved to `>= 10.2.3`.

## Tests

| Test | Contract |
|---|---|
| `extractRepoFilePaths handles adversarial slash sequences in linear time` (`plugins/core/test.mjs`) | C1, C2 |
| `cargo check --manifest-path src-tauri/Cargo.toml` / `npm test` | C1, C3 |
