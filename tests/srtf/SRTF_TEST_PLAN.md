# NeuroStrata SRTF: Master Test Plan & Behavioral Inventory

## Objective
Define the strict behavioral contract for NeuroStrata agents based on the system's embedded skills (`neurostrata`, `graphify`, `beadboard-driver`) and global constraints. Establish a deterministic simulation environment (Self-Reinforced Testing Framework - SRTF) to verify compliance via artifact and state assertions, rather than random metrics or LLM guesswork.

---

## Pillar 1: Initialization & Bootstrap (The Blank Slate)
When an agent is dropped into an unknown or fresh repository, it must establish the foundation before modifying code.

1. **Project Status Check**: Assert `project.md` exists and contains the required "Environment Status Cache" table.
2. **Graphify AST/Semantic Bootstrap**: Assert the `.NeuroStrata/docs/` directory is created, proving the agent invoked `./scripts/bootstrap.sh <pwd>` to generate the foundational C4 knowledge graph.
3. **LanceDB Bootstrap Node**: Query the LanceDB database (`SELECT * WHERE memory_type = 'bootstrap'`) to verify a project-level context anchor exists.
4. **Pre-Flight Recall Sequence**: Parse the agent's LLM tool-call execution trace to assert that `neurostrata_get_snapshot` or `neurostrata_search_memory` was invoked *before* any modifying actions (like `bash` or `edit`).

---

## Pillar 2: Coordination & State (BeadBoard)
The agent operates within a swarm topology and must maintain explicit, visible state via `bd` and `bb`.

1. **Explicit Task Claiming**: Verify via `bd show <id>` or `.beads` SQLite inspection that the active bead has an assigned `agent-bead-id`.
2. **State Publishing**: Verify the agent properly emitted state transitions (`bd set-state <bead_id> state=... --reason="..."`).
3. **Async Workflow Delegation**: Verify that long-running file parsing, large edits, or deep reasoning are either placed in backlog beads via `bash` or explicitly delegated using `NeuroStrata-Task`, rather than synchronously blocking the user chat.
4. **Continuous Session Logging**: Assert the existence of a populated `.NeuroStrata/sessions/*.log` file, proving the agent used `neurostrata_append_log` to capture the conversation and its thought process.

---

## Pillar 3: Graphify & Wiki Phase (Knowledge Synthesis)
If the agent learns something complex or processes a multi-step task, it must promote vector memory to a structured knowledge graph.

1. **Vector-to-Wiki Promotion**: Inject a massively complex multi-step prompt during evaluation. Assert the agent creates a markdown file in `docs/architecture/domains/` with the mandatory YAML frontmatter (`domain`, `description`, `governs_paths`), rather than stuffing the entire explanation into the LanceDB vector space.
2. **Graphify Update Invocation**: Assert the agent ran `graphify` (or `/graphify --update`) to sync the new Wiki files into `graph.json`.
3. **Canvas Synchronization**: Assert that `.NeuroStrata/docs/NeuroStrata MemorySpace.canvas` has an updated modification time, proving the agent ran `neurostrata_generate_canvas`.

---

## Pillar 4: The Execution & Memory Commit (The Wrap-Up)
The agent cannot finish a session without securely landing the context.

1. **The Habitual Memory Commit**: Query LanceDB after the execution sandbox completes. Assert that a new row exists with a timestamp matching the execution window (proving `neurostrata_add_memory` was called).
2. **Graph Edges & Cross-Linking**: Assert that the newly created memory has a populated `related_to` array, proving the agent connected the new fact to an existing node.
3. **Bi-Directional Code Anchors**: Assert that the newly created memory has `metadata.refs` containing valid file paths, anchoring the concept to physical code.
4. **The Remote Push**: Inspect `git log` and the bash execution trace to verify the mandatory closeout sequence: `git pull --rebase`, `bd dolt push`, and `git push`.

---

## Evaluation Mechanics (train_srtf.py)
The evaluation script will run a Podman container. After the agent finishes its task, a Python verification sequence will execute `assert` statements across the container's workspace output (verifying file creation, checking the embedded LanceDB instance, and parsing bash logs). 

*   **Recall Score:** Calculated from Pillar 1 metrics.
*   **Coordination Score:** Calculated from Pillar 2 metrics.
*   **Graph/Wiki Synthesis Score:** Calculated from Pillar 3 metrics.
*   **Habitual Commit/Extraction Score:** Calculated from Pillar 4 metrics.

The cumulative score determines whether the agent's current behavioral prompt (in `SKILL.md`) is successfully governing its actions.