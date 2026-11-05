# Why Traditional AI Agent Memory Fails — And How NeuroStrata Fixes It

> **Why you care:** Standard RAG and flat Markdown files (`AGENTS.md`, `MEMORY.md`, `TODO.md`) fail predictably on real codebases. NeuroStrata replaces passive text reminders with binary-enforced cognitive machinery.

---

## The 5 Systemic Failure Modes of Agent Memory

### 1. Context Compaction Amnesia
* **Why Traditional Tools Fail:** Long coding sessions inevitably hit context limits. When the harness compacts conversation history, instructions read at the start of the chat evaporate, and the agent repeats mistakes fixed an hour earlier.
* **The NeuroStrata Fix:** Every turn logs to the **Episodic Buffer** (`neurostrata_append_log`), which returns tag-matched memories on the write response itself (`docs/recall.md`), while **Lock 3 (The Breath Prompt)** records state checkpoints directly onto the active Goal in LadybugDB.

### 2. Passive Markdown Ignorance (`AGENTS.md` / `MEMORY.md`)
* **Why Traditional Tools Fail:** Flat rules files grow into thousands of lines of contradictory prose. Agents either skim past the rule that matters or burn thousands of tokens reading rules for unrelated subsystems.
* **The NeuroStrata Fix:** Rules are stored as graph nodes anchored via `GOVERNS` edges to exact files and Tree-sitter AST symbols (`neurostrata_memories_for_file`). When an agent touches `src/store/ladybug.rs`, it retrieves the exact rules governing that file and its call-graph neighbors—plus the **Prefrontal Cortex** (`neurostrata_guard_validate`) blocks prohibited actions before execution.

### 3. Single-Modality RAG Blindness
* **Why Traditional Tools Fail:** Pure dense vector search fails on exact function names, error codes, and config keys (`check_caller_path`), whereas pure keyword grep fails on conceptual architectural constraints.
* **The NeuroStrata Fix:** The **Dendritic Bridge** exposes three alternative retrieval surfaces, chosen per call with the `mode` argument: a LadybugDB HNSW vector surface (`NomicEmbedTextV15`, 768 dims), a disk-backed Tantivy full-text surface, and a deterministic in-house 5-field BM25 surface (`src/fts/mod.rs`), followed by a bounded graph expansion over `GOVERNS` / `RELATES_TO` / `CONTAINS` / `DEPENDS_ON` / `EXTRACTED_FROM` edges. The surfaces are not fused into one pipeline — the caller picks the one that fits the question.

### 4. Blank-Slate Subagents
* **Why Traditional Tools Fail:** When an orchestrator spawns a worker subagent to implement a slice, the subagent starts with a blank context window and zero knowledge of project invariants—leading to reward-hacking stubs or rule violations.
* **The NeuroStrata Fix:** Claiming a task (`neurostrata_task_claim`) automatically runs a hybrid vector + 1-hop graph search on the task's title and description, returning a ready-to-paste `subagent_prompt_injection` block. Zero-touch harness plugins prepend this context automatically.

### 5. Unverified "Done" Claims & Ephemeral TODOs
* **Why Traditional Tools Fail:** Agents routinely mark markdown checkboxes `[x]` without extracting what caused the bug or verifying that all work is complete before `git push`.
* **The NeuroStrata Fix:** The **Central Executive** enforces **Lock 2 (Done-Funnel)**—`neurostrata_task_complete` refuses to close a task unless an architectural memory (`EXTRACTED_FROM`) is recorded—and **Lock 1 (`pre-push` Git Gate)** blocks `git push` if any task is still `in_progress` or lacks extraction.
