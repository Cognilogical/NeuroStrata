# 🧠 NeuroStrata Cognitive Architecture

NeuroStrata utilizes a sophisticated, neuroscience-inspired memory architecture designed explicitly for autonomous software engineering agents. By moving beyond flat vector blobs, NeuroStrata implements a **Dual-Track Bi-Temporal Graph Memory System** that ensures agents possess stable, relational, and self-optimizing context for any codebase.

Here are the formal cognitive techniques powering the NeuroStrata engine:

### 1. Semantic Graph Edges (Relational Node Traversal)
*Inspired by human associative memory.*
Instead of treating architectural rules as isolated facts, NeuroStrata nodes form a directional knowledge graph. Memories contain a `related_to` schema array. When an agent updates a database rule, it can traverse the semantic edges to instantly identify connected API contracts or frontend component constraints, preventing cascading regressions.

### 2. Immutable Temporal Audit Trail (Bi-Temporal Memory)
*Inspired by human episodic memory.*
Agents never overwrite history. When an architectural decision changes, the system applies a `valid_to` soft-deletion timestamp to the old node and creates a new node with a `valid_from` timestamp. This bi-temporal design maintains a perfect, queryable audit trail of how the codebase architecture evolved over time.

### 3. Neural Gain Mechanism (Access-Based Synaptic Weighting)
*Inspired by the Ebbinghaus Forgetting Curve and synaptic plasticity.*
All vector databases rank results by mathematical distance (e.g., L2 or Cosine). NeuroStrata applies a **Neural Gain Filter** on top of this. Every time an agent successfully utilizes a memory, its `access_count` increments. During retrieval, the engine dynamically calculates a final salience score: `Base Distance - (Access Frequency * Neural Boost)`. Outdated, unused rules naturally decay out of the context window, while highly-accessed core principles permanently float to the top.

### 4. Domain-Isolated Knowledge Shelves (Contextual Compartmentalization)
*Inspired by declarative knowledge clustering.*
To prevent massive context hallucination, NeuroStrata categorizes vectors into explicit declarative `domains` (e.g., `frontend`, `database`, `devops`, `api`). This allows agents to compartmentalize their focus, drastically shrinking the vector search space and ensuring high-fidelity retrieval for domain-specific tasks.

### 5. Pre-computed Cognitive Snapshots (Zero-Shot Context Grounding)
*Inspired by the psychological concept of "Working Memory Priming".*
Instead of forcing agents to blindly search a new repository and waste expensive token processing, NeuroStrata features a `neurostrata_get_snapshot` tool. This instantly returns the top-5 highest-weighted, active cognitive nodes for any project. This "wake-up context" perfectly grounds an agent in a repository's most critical rules the second a session begins.

### 6. Reciprocal Rank Fusion (Hybrid Semantic & FTS Retrieval)
*Inspired by human dual-process theory (gist vs. verbatim recall).*
Relying solely on dense vector embeddings is notoriously poor for finding exact variable names, acronyms, or specific syntax. NeuroStrata's Rust backend utilizes `tantivy` to combine dense vector semantic search with exact Full-Text Search (BM25 keyword matching). These two retrieval tracks are merged via Reciprocal Rank Fusion (RRF), ensuring agents recall both the "gist" of an architectural concept and the exact "verbatim" code syntax.

### 7. 3-Tier Stratification (Contextual Hierarchy)
*Inspired by the division of Working Memory vs. Long-Term Memory.*
To prevent context bleeding between vastly different projects or languages, NeuroStrata organizes memory across three hard boundaries:
- **Global:** Universal software engineering principles or developer preferences that apply universally.
- **Domain/Project:** Architectural rules tightly bound to a specific codebase's namespace.
- **Task:** Ephemeral, short-term context tied to an active problem-solving session.

### 8. Git-Atomic Task State (Object Permanence via Beads)
*Inspired by spatial object permanence.*
An AI's memory and task state should never decouple from the physical code. NeuroStrata tightly integrates with `bd` (Beads) and Dolt. An agent's active issue state, assignment, and task memory are tied directly to the Git tree. If a developer rolls back a branch or checks out an old commit, the agent's memory and issue state instantly roll back with it, ensuring the AI's worldview always perfectly matches the physical codebase state.

### 9. Extended Cognition Workspace (Human-AI Symbiosis via NeuroVault)
*Inspired by the extended mind thesis.*
Memory is not locked in a black-box database. Through the NeuroVault and Obsidian integration, the exact same memory nodes the AI uses to structure code are exposed as a visual, navigable markdown graph for human developers. Humans can curate, audit, and organically learn from the same cognitive graph the AI relies on, creating true symbiosis.

### 10. Asynchronous Executive Function (Orchestrator-Worker Pattern)
*Inspired by the brain's executive control system (frontal lobe).*
Operating highly intelligent frontier models on trivial code changes is financially and computationally wasteful. NeuroStrata enforces an Orchestrator/Worker divide. A high-intelligence Orchestrator acts as the "executive," routing context, managing the memory graph, and structuring plans, while aggressively offloading physical code execution (file writes, linting, compiling) to a cheaper, narrowly-focused `NeuroStrata-Task` agent.
