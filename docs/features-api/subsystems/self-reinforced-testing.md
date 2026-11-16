# Self-Reinforced Testing Framework (SRTF)

## 1. Purpose & Vision
The Self-Reinforced Testing Framework (SRTF) is an autonomous evaluation loop designed to formally verify the effectiveness of NeuroStrata's agent instructions (`SKILL.md` and `AGENTS.md`). By observing a "Subject Agent" operating inside a completely isolated sandbox, we can empirically measure its adherence to memory retention protocols, measure hallucination rates, and iteratively tune the skill instructions to maximize architectural compliance.

## 2. The 5 Core Verification Axioms
The SRTF evaluates the Subject Agent against five non-negotiable operational axioms. If the agent fails to perform these autonomously, the skill instructions must be mutated and improved.

1. **The Bootstrapping Axiom:** If `.NeuroStrata/docs/` is missing, the agent must proactively invoke `neurostrata-mcp ingest <dir> <namespace>` to initialize the project architecture into the LadybugDB vector store.
2. **The Graph Generation Axiom:** Following significant ingestion or architectural changes, the agent must autonomously export the graph (`neurostrata-mcp export-graph <out.json>`) so the result can be inspected in the web UI. *(There is no `neurostrata_generate_canvas` tool, and the Obsidian canvas export was never implemented — see `docs/ops/obsidian.md`.)*
3. **The Habitual Memory Commit Axiom:** Before declaring a task complete or closing a tracker issue, the agent MUST run `neurostrata_add_memory` to permanently store at least one newly learned contextual fact, fix, or architectural rule.
4. **The Session Backup Axiom:** The agent must maintain an append-only log in `.NeuroStrata/sessions/*` to prevent context loss.
5. **The Task Tracker Axiom:** The agent must strictly adhere to the NeuroStrata task protocol, claiming an existing task (`neurostrata_task_claim`) or creating one (`neurostrata_task_create`) before modifying any codebase files. *(The legacy `bd` / Beads tracker is deprecated and replaced by the NeuroStrata task tools.)*

## 3. Black-Box Isolation Architecture (Containerized)
To prevent the Subject Agent from cross-contaminating the global LadybugDB memory or colliding with the global MCP server, the SRTF relies on a strict Black-Box Isolation strategy using `podman` containers. 

Because the agent runs entirely inside a containerized namespace, we achieve 100% isolation automatically without needing to patch the underlying `neurostrata-mcp` Rust server's hardcoded paths or ports:

* **Filesystem Isolation:** The container has its own `~/.local/share/neurostrata/db` directory. The agent's vector database writes cannot physically escape the container or touch the host's LadybugDB files.
* **Network Isolation:** The container has its own network stack. When the agent spins up the local `neurostrata-mcp` daemon inside the container, it binds to `127.0.0.1:34343` *internally*. Because we do not publish or map that port to the host machine (`-p`), there are no "Address already in use" conflicts with the host's global daemon.
* **Instruction Override:** The sandbox container is injected with an ephemeral `.config/opencode/opencode.json`, and isolated copies of `AGENTS.md` and `SKILL.md`. The Subject Agent only reads the experimental instructions being tested.

## 4. The Evaluation Loop
The orchestrator (Evaluator Agent) will run the following continuous improvement loop:

1. **Setup:** Build the isolated `podman` image containing the opencode agent environment, the `neurostrata-mcp` binary, and a real-world repository (e.g., Express.js). Inject the current iteration of the skill instructions.
2. **The Trial:** Use `podman run` to spawn a fresh `NeuroStrata-Task` subagent inside the container. Issue a complex architectural prompt (e.g., "Refactor the routing logic and document the architecture").
3. **The Audit:** Once the Subject container terminates, the Evaluator extracts the container's isolated LadybugDB files (or runs a query command inside the container before exit) and reads the NeuroStrata task records (`neurostrata-mcp task list <namespace> --status done`) to score compliance against the 5 Axioms.
4. **The Mutation:** If the Subject failed an axiom (e.g., forgot to run `neurostrata_add_memory`), the Evaluator formulates a theory and rewrites the `SKILL.md` template on the host to enforce the behavior more strictly.
5. **Repeat:** Rebuild the container image with the mutated `SKILL.md` and run the trial again until a 100% Axiom compliance rate is achieved.

## 5. Target Testbed
**Repository:** `expressjs/express`
**Rationale:** A medium-sized, highly structured, and well-known codebase. It provides ample complexity for the AST parser and graph generator, serving as an ideal proving ground for the agent's architectural comprehension and memory extraction capabilities.
