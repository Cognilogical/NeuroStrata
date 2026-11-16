# Tri-Strata — Global, Domain, and Task Namespace Isolation

> **Why you care:** Company-wide infrastructure rules should apply everywhere, project-specific architecture rules should stay inside their repository, and temporary debugging notes should never pollute long-term memory. Tri-Strata enforces this separation at the storage and query boundary.

---

## 1. The Three Cognitive Strata

NeuroStrata partitions knowledge into three hierarchical tiers inspired by human cognitive memory systems:

| Stratum | Scope | Typical Contents | Persistence & Sharing |
| :--- | :--- | :--- | :--- |
| **Tier 1: `global` (Semantic Cortex)** | Cross-project universal mandates | Container policies (`podman` over `docker`), security baselines, operator coding preferences, global guard rules. | Permanent in LadybugDB; queried alongside every project snapshot. |
| **Tier 2: `<project>` / Domain (Declarative Memory)** | Single repository or bounded domain | Module invariants, API contracts, Tree-sitter AST graph, `GOVERNS` file bindings, procedures, and domain schemas. | Permanent per-namespace; shareable across teammates via **SynapticMesh**. |
| **Tier 3: Task & Episodic Buffer (Working Memory)** | Active session & Goal lifecycle | Claimed `task` state machines, Breath checkpoints, and rolling 500 KB `.NeuroStrata/sessions/current.md` conversational log (the Episodic Buffer, `src/buffer.rs`). | Consolidated into Tier 2 via Lock 2 (`neurostrata_task_complete`) when work finishes. |

---

## 2. Declarative Domain Shelves Within a Namespace

Within any Tier 2 project namespace, memories can be tagged with a declarative `domain` slug (such as `database`, `frontend`, `auth`, `code_ast`, or `judgment_providers`):
- **Domain Registry (`docs/architecture/domains/`):** Markdown domain specifications declare `domain`, `description`, and `governs_paths` in YAML frontmatter.
- **Soft-Filtered Retrieval:** Passing `domain` to `neurostrata_search_memory` narrows recall to matching domain shelves—while automatically falling back to the full namespace if a strict domain filter would return zero candidates (`src/server.rs`).

---

## 3. Multi-Checkout & Worktree Source Anchoring

Developers and parallel subagents frequently work across multiple Git worktrees or directory checkouts of the same repository. NeuroStrata prevents path drift across checkouts using stable source anchors:
- Repository-relative `governs` targets (`NeuroStrata::src/server.rs`) remain independent of transient worktree directory paths.
- `neurostrata-mcp project-context bind <source-id> <marker-path>` registers `.NeuroStrata` project markers so `export-graph` and file-governance lookups resolve deterministically from any linked worktree.
