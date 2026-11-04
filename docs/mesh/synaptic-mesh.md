# SynapticMesh — Brokerless Git Team Memory Synchronization

> **Why you care:** Team memories travel over standard Git — no SaaS broker, no central database server, sovereign private rows are never overwritten, and conflicting edits are preserved visibly rather than silently clobbered.

---

## 1. Architecture Overview

While **SynapticGraph** manages cognitive memory inside a single developer's local LadybugDB store, **SynapticMesh** (`src/synaptic_mesh/`) synchronizes shareable architectural rules, facts, and procedure definitions across a team using an ordinary Git repository as the transport layer.

```text
Developer A (LadybugDB) ──► Outbox JSONL ──► git push / pull --ff-only ──► Inbox Replay ──► Developer B (LadybugDB)
```

Key design properties:
- **Zero External Infrastructure:** Any Git remote (GitHub, GitLab, Gitea, bare SSH repo) acts as the mesh transport.
- **Explicit Shareability Boundary:** Only `rule`, `fact`, and `procedure` definitions explicitly marked with `eligibility: shareable` cross the mesh.
- **Strict Local Privacy:** Tasks, AST `structural` nodes, raw vector embeddings, episodic session logs, and `private` memories never leave the local machine.
- **SHACL & Schema Validation on Ingress:** Every incoming mesh record is validated against `src/schemas/shacl/mesh-record-shapes.v1.ttl` before replay.

---

## 2. Shareable vs. Private Sovereignty

| Record Type | Crosses Mesh? | Sovereignty Rules |
| :--- | :--- | :--- |
| `rule` / `fact` (`eligibility: shareable`) | **Yes** | Exported to outbox JSONL; re-embedded locally by each receiving peer. |
| `procedure` (`eligibility: shareable`) | **Definition Only** | Trigger and instructions travel; local runtime counters (`performance_count`, `last_performed_at`, `last_episodic_pointer`, and fire budgets) are stripped on export and rejected if present on import. |
| `private` Memories | **Never** | Sovereign to the author's local store; incoming mesh records can never overwrite a local private row sharing the same ID. |
| `task` & `structural` AST Nodes | **Never** | Strictly local to the checkout and Central Executive session. |

---

## 3. Deterministic Conflict Preservation

Distributed teams occasionally modify the same shared rule concurrently. SynapticMesh never guesses or silently drops an edit (`src/synaptic_mesh/conflict.rs`):
1. If an incoming mesh record diverges from a locally modified record with a non-ancestor provenance chain, both variants are preserved and recorded in the conflict registry.
2. `neurostrata-mcp synaptic-mesh status` and `synaptic-mesh conflicts` surface the exact conflicting record IDs and authors.
3. Agents are instructed by the honesty protocol to halt synthesis over conflicted IDs until resolved.

---

## 4. Honesty & Fail-Soft Operational Rules

- **Explicit-Zero Status Lines:** When the outbox and conflict queues are empty, `synaptic-mesh status` explicitly reports `0` pending and `0` conflicts — never blank output or silent omissions.
- **Fail-Soft Transport:** If the remote Git server is offline or unreachable, outbox records queue locally and report a single status line. Mesh transport failures never block local queries or fail the source repo's `pre-push` hook.
- **Fast-Forward Discipline (`--ff-only`):** The mesh repository uses `git pull --ff-only` and standard pushes only. Force-pushing is prohibited so no teammate's accepted memory history can be erased. Recovery guidance (`neurostrata-mcp synaptic-mesh recover`) is print-only and never runs `git reset --hard`.

---

## 5. CLI Usage

```bash
# Bind a local directory/repo as the team SynapticMesh transport
neurostrata-mcp synaptic-mesh init /path/to/team-mesh-repo

# Inspect outbox queue, inbox replay state, and conflict count
neurostrata-mcp synaptic-mesh status --namespace NeuroStrata

# Pull peer records (--ff-only), validate & import, and push local outbox
neurostrata-mcp synaptic-mesh sync --namespace NeuroStrata

# Inspect any divergent record variants
neurostrata-mcp synaptic-mesh conflicts --namespace NeuroStrata

# Print non-destructive recovery steps if the mesh git tree diverged
neurostrata-mcp synaptic-mesh recover
```
