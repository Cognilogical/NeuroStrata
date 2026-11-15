# NeuroStrata v1.15.0 Field Audit Fixes & Multi-Harness Enhancements Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix all 6 bugs and implement all 5 general-purpose enhancements from the v1.15.0 Field Audit (`/tmp/neurostrata-agent-report.md`) across the Rust core and all harness plugins, verified by unit/integration tests and delivered via a Pull Request from branch `opencode/v115-field-audit-fixes`.

**Architecture:**
1. **Shared Plugin Core (`@neurostrata/plugin-core` at `plugins/core/`)**: Centralizes MCP JSON-RPC client, token loading, project-root namespace discovery (Bug 2), canonical tool classification & OpenCode V2 Code Mode `execute` inspection (Bug 3), unified `handleHookEvent` / `handleClaudeHookEvent` (Bug 4), LLM-visible file-touch memory delivery + `.NeuroStrata/session_queries.json` ledger sync (Bug 5), daemon-backed active task/session resolution (Enhancement 1), dynamic subagent prompt injection with daemon `subagent_prompt_injection` + referenced file-path memory prefetch (Enhancement 2), and verifiable subagent Graph-First rule predicate (Enhancement 3).
2. **Thin Harness Adapters (`plugins/{opencode,claude,codex,github-cli}`)**: Import and re-export from `../../core/dist/index.js` so both repo-local `plugins/<harness>/dist/index.js` and extracted `.NeuroStrata/plugins/<harness>/dist/index.js` share one implementation (Enhancement 5).
3. **Rust Core Updates (`src/task/harness_plugins.rs`, `src/task/mod.rs`, `src/server.rs`, `src/store/ladybug.rs`)**:
   - Wire `.opencode/plugins/neurostrata.js` and `.opencode/opencode.json` in `wire_harness_config`, and install `.NeuroStrata/plugins/core/dist/index.js` in `auto_install_detected_plugins` (Bug 1).
   - Make `SCAFFOLDED_PRE_COMMIT_HOOK` (`src/task/mod.rs` and `scripts/hooks/pre-commit`) language- and layout-agnostic (Bug 6).
   - Replace the prose skip clause in `SUBAGENT_GRAPH_FIRST_RULE_SNIPPET`, `build_task_graph_context`, and `.agents/` templates with the verifiable file-and-memory-ID predicate (Enhancement 3).
   - Resolve active `InProgress` task's `session_id` in `handle_guard_validate` when caller's `session_id` is empty/`"unknown"`, and expose `session_id` + `subagent_prompt_injection` on `in_progress` tasks in `neurostrata_task_list` (Enhancement 1).
   - Bundle 1-hop related memories (`DEPENDS_ON`/`SUPERSEDES`/`RELATES_TO`) and compact AST neighborhood summary (`symbols`, `callers`, `callees`, `imports`, `importers`, `blast_radius_count`) into `neurostrata_memories_for_file` when `include_ast_edges` is `true` (default `true`), and fix `server.rs:1350` query-hint extraction for `path`/`id` (Bug 5 & Enhancement 4).

**Tech Stack:** Rust (`tokio`, `serde_json`, LadybugDB), TypeScript / Node.js ESM (`node:test`).

---

## Global Constraints
- `src/task/harness_plugins.rs` embeds `plugins/*/dist/index.js` via `include_str!`; plugin `dist/index.js` files MUST be built before `cargo test` / `cargo build`.
- `.gitignore` must not ignore `plugins/opencode/dist/` or `plugins/core/dist/` so fresh clones/worktrees compile out of the box.
- In `initialize(ctx)`, register `"execute.after"` BEFORE `"execute.before"` so existing tests that record the last `ctx.tool.hook` call see `"execute.before"`.
- Keep `injectSubagentPrompt(target, namespace, taskId, extraContext?)` synchronous for backward compatibility with existing tests while providing async `enrichAndInjectSubagentPrompt(target, namespace, taskId)` for live hook execution.

---

### Task 1: Shared `@neurostrata/plugin-core` & Thin Harness Adapters (Bugs 2, 3, 4, 5 & Enhancements 1, 2, 3, 5)

**Files:**
- Create: `plugins/core/package.json`
- Create: `plugins/core/tsconfig.json`
- Create: `plugins/core/index.ts`
- Create: `plugins/core/dist/index.js`
- Create: `plugins/core/test.mjs`
- Modify: `plugins/opencode/index.ts`, `plugins/opencode/dist/index.js`, `plugins/opencode/test.mjs`
- Modify: `plugins/claude/index.ts`, `plugins/claude/dist/index.js`, `plugins/claude/test.mjs`
- Modify: `plugins/codex/index.ts`, `plugins/codex/dist/index.js`, `plugins/codex/test.mjs`
- Modify: `plugins/github-cli/index.ts`, `plugins/github-cli/dist/index.js`, `plugins/github-cli/test.mjs`
- Modify: `.gitignore`

- [ ] **Step 1: Create `plugins/core/package.json`, `plugins/core/tsconfig.json`, and `plugins/core/index.ts`**
  Implement in `plugins/core/index.ts`:
  - `findProjectRoot(startDir = process.cwd())` & `discoverProjectNamespace(startDir = process.cwd())`:
    1. Walk up from `startDir` looking for `.NeuroStrata/session_queries.json` → parse `.namespace` if non-empty string.
    2. Walk up from `startDir` looking for `.git` → `path.basename(gitRoot)`.
    3. Fallback to `path.basename(path.resolve(startDir)) || "default"`.
  - `resolveConfig(pluginConfig, cwd = process.cwd())`:
    Resolves `namespace` in priority order: `pluginConfig?.namespace` (non-empty) → `process.env.NEUROSTRATA_NAMESPACE` (non-empty) → `discoverProjectNamespace(cwd)`. Never defaults to `"NeuroStrata"`.
  - Canonical `TOOL_CLASSIFICATION`:
    - `READ_FILE_TOOLS`: `["read", "Read", "read_file", "view", "cat"]`
    - `MUTATING_FILE_TOOLS`: `["write", "Write", "write_file", "create_file", "edit", "Edit", "MultiEdit", "edit_file", "patch", "patch_file", "apply_patch", "replace_string_in_file", "insert_edit_into_file"]`
    - `SHELL_TOOLS`: `["bash", "Bash", "shell", "run_shell_command", "run_command", "execute_command", "terminal"]`
    - `SUBAGENT_TOOLS`: `["subagent", "task", "Task", "delegate_task", "spawn_agent", "dispatch_agent"]`
    - `CODE_EXEC_TOOLS`: `["execute"]`
    - Helper predicates: `isFileTouchTool(name)`, `isMutatingTool(name)`, `isSubagentTool(name)`, `isCodeExecTool(name)`.
  - OpenCode V2 Code Mode inspector `inspectCodeModeExecution(code: string)`:
    Detects `neurostrata_task_claim`, `neurostrata_task_update`, `neurostrata_task_complete`, and `neurostrata_memories_for_file` inside `input.input.code` / `input.args.code`, extracting `{ id, session_id, note, path }` arguments and updating `activeTaskId`, `activeSessionId`, and `mutationsSinceNote`.
  - Daemon-backed task state resolver `resolveActiveTaskFromDaemon(namespace: string)`:
    Calls `mcpClient.callTool("neurostrata_task_list", { namespace, status: "in_progress" })`, parses the active task's `id`, `session_id`, and `subagent_prompt_injection`, caches them in `activeTaskId` / `activeSessionId` / `cachedSubagentPromptByTask`, and returns them.
  - File-touch recall + LLM context delivery + `.NeuroStrata/session_queries.json` recording:
    - `prefetchMemoriesForFile(namespace, filePath, opts?)` queries `neurostrata_memories_for_file`, formats `[NeuroStrata <type> <id> (valid_from=...)] <content>` (plus compact AST summary if `ast_summary` is returned), records the query in `<projectRoot>/.NeuroStrata/session_queries.json`, and returns capped snippets.
    - `formatFileTouchRecallBlock(filePath, snippets)` formats `[NeuroStrata Governing Memories for ${filePath}]\n${snippets.join("\n")}`.
    - In `execute.before`, attach `additionalContext` / `neurostrataContext` on `input`, store pending recall for `execute.after`, and return `{ additionalContext, neurostrataContext }` when non-empty.
    - In `execute.after`, append/prepend the pending `[NeuroStrata Governing Memories for <path>]` block onto `output.output` / `output.content` / `input.output`.
    - In `handleHookEvent(event)` (and alias `handleClaudeHookEvent(event)`), handle `PreToolUse` and `Stop` across all harnesses:
      - Subagent tools → `await enrichAndInjectSubagentPrompt(toolInput, config.namespace, activeTaskId)` and return `updatedInput`.
      - File-touch tools → `prefetchMemoriesForFile` and include `additionalContext: block` + `systemMessage: block` in the allow response.
      - Mutating tools → resolve active task from daemon if `activeTaskId === null`, run `neurostrata_guard_validate`, deny on reject, or allow (merging `additionalContext` if file-touch snippets were also recalled).
  - Dynamic subagent prompt injection (`buildSubagentContextHeader`, `injectSubagentPrompt`, `extractRepoFilePaths`, `enrichAndInjectSubagentPrompt`):
    - Uses the verifiable predicate from Enhancement 3:
      `Before editing any file, check whether its path and governing memory IDs (\`mem-...\`) are already listed under \`### NeuroStrata Task & Graph Context\` in your prompt. For any file you touch that is NOT listed in that block, call \`neurostrata_memories_for_file\` first.`
    - `extractRepoFilePaths(prompt)` extracts repo-relative file paths from the subagent prompt, prefetches their governing memories from the daemon, and embeds both the daemon's `subagent_prompt_injection` and per-file governing memories directly into the injected header.

- [ ] **Step 2: Refactor `plugins/{opencode,claude,codex,github-cli}/index.ts` into thin adapters importing `../../core/dist/index.js`, compile all `dist/index.js`, and update `.gitignore`**
  - Remove `plugins/opencode/dist/` from `.gitignore`.
  - Compile `plugins/core` and all 4 harness adapters.
  - Run `node --test` on `plugins/core/test.mjs` and `plugins/{opencode,claude,codex,github-cli}/test.mjs`.

---

### Task 2: Rust Core Fixes & Enhancements (Bugs 1, 5, 6 & Enhancements 1, 3, 4)

**Files:**
- Modify: `src/task/harness_plugins.rs`
- Modify: `src/task/mod.rs`
- Modify: `scripts/hooks/pre-commit`
- Modify: `.agents/skills/neurostrata/SKILL.md`
- Modify: `.agents/agents/NeuroStrata-Task.md`
- Modify: `src/server.rs`
- Modify: `src/store/ladybug.rs`

- [ ] **Step 1: Update `src/task/harness_plugins.rs` (Bug 1, Bug 4, Enhancement 3, Enhancement 5)**
  - Embed `CORE_INDEX_JS` (`include_str!("../../plugins/core/dist/index.js")`) and `CORE_PKG_JSON` (`include_str!("../../plugins/core/package.json")`) and always write `.NeuroStrata/plugins/core/dist/index.js` and `.NeuroStrata/plugins/core/package.json` in `auto_install_detected_plugins`.
  - Update `CLI_HOOK_RUNNER_MJS` to call `const handler = plugin.handleHookEvent ?? plugin.handleClaudeHookEvent;` and pass `process.env.NEUROSTRATA_NAMESPACE || undefined` so project-root namespace discovery runs when the env var is unset.
  - Update `wire_harness_config` for `"opencode"` to:
    1. Write `.opencode/plugins/neurostrata.js` containing:
       ```js
       export { default } from "../../.NeuroStrata/plugins/opencode/dist/index.js";
       export * from "../../.NeuroStrata/plugins/opencode/dist/index.js";
       ```
    2. Merge `"./.NeuroStrata/plugins/opencode/dist/index.js"` into `.opencode/opencode.json` → `"plugins"` (creating or preserving existing keys/plugins) when `.opencode` exists or `is_active` is true, and also keep updating root `opencode.json` if it exists.
  - Update `SUBAGENT_GRAPH_FIRST_RULE_SNIPPET` (and upgrade existing patched agent `.md` files containing the old `unless the parent prompt states` clause) to the verifiable predicate from Enhancement 3.
  - Update `.agents/skills/neurostrata/SKILL.md` and `.agents/agents/NeuroStrata-Task.md` with the verifiable predicate.

- [ ] **Step 2: Update `src/task/mod.rs` and `scripts/hooks/pre-commit` (Bug 6, Enhancement 1, Enhancement 3)**
  - Update `build_task_graph_context` (`src/task/mod.rs:445-452`) to use the verifiable predicate in `subagent_prompt_injection`.
  - Update `record_graph_query` (`src/task/mod.rs:458-525`) to fall back to walking up from `std::env::current_dir()` to find `.git` when `project_root` is not in DB rows, and include `"governed_prefixes"` in `.NeuroStrata/session_queries.json` entries.
  - Update `handle_task_list` (`src/task/mod.rs`) so `in_progress` task entries include `session_id` and `subagent_prompt_injection` (if stored on the task metadata), enabling stateless plugins to fetch active task state + prompt injection in one call.
  - Replace `SCAFFOLDED_PRE_COMMIT_HOOK` in `src/task/mod.rs` and `scripts/hooks/pre-commit` with a layout- and language-agnostic implementation that checks all staged files (`git diff --name-only --cached`, excluding `.NeuroStrata/`) against `.NeuroStrata/session_queries.json` by full path or directory prefix (`dirname`), warns if unqueried, and appends to `.NeuroStrata/session_queries.json` cleanly across any language/directory layout (`pkg/`, `apps/`, `scripts/`, `src/`, etc.).

- [ ] **Step 3: Update `src/server.rs` and `src/store/ladybug.rs` (Bug 5, Enhancement 1, Enhancement 4)**
  - In `src/server.rs:1348-1357`, fix `hint` extraction to check `a.get("path").or_else(|| a.get("file_path")).or_else(|| a.get("id")).or_else(|| a.get("memory_id"))` and also call `crate::task::record_graph_query(&store, &namespace, "neurostrata_memories_for_file", path).await` inside `handle_memories_for_file`.
  - In `src/server.rs:7711-7743` (`handle_guard_validate`), when `active_task` (`Status::InProgress`) is resolved and caller `session_id` is empty or `"unknown"`, fall back `session_id` to `active_task`'s `metadata.task.session_id` (or `"daemon-resolved"`).
  - In `src/store/ladybug.rs`, add `pub async fn ast_neighborhood_for_file(&self, namespace: &str, source: Option<&str>, path: &str, governing_ids: &[String]) -> Result<(Vec<Value>, Value)>` returning `(related_memories, ast_summary)` (`symbols`, `callers`, `callees`, `imports`, `importers`, `blast_radius_count`).
  - In `src/server.rs` (`handle_memories_for_file` and tool schema), add optional `include_ast_edges: bool` (default `true`) and include `"related_memories"` and `"ast_summary"` in the response when `include_ast_edges` is `true`.
  - Add Rust unit/integration tests covering Bug 1 (`.opencode/plugins/neurostrata.js` + `.opencode/opencode.json`), Bug 6 (layout-agnostic pre-commit hook on `pkg/auth/handler.go` and `apps/web/index.tsx`), Enhancement 1 (`handle_guard_validate` with empty/`"unknown"` `session_id` when an `InProgress` task exists), Enhancement 3 (verifiable predicate in `build_task_graph_context` and `SUBAGENT_GRAPH_FIRST_RULE_SNIPPET`), and Enhancement 4 (`include_ast_edges` on `handle_memories_for_file`).
