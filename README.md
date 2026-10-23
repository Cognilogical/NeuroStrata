# 🧠 NeuroStrata

**Make “done” mean something.**

You know the pattern: an agent repeats a failed fix across checkouts, declares victory without recording how it worked, and you become its only continuity mechanism. NeuroStrata puts that bookkeeping into machinery — one compiled binary, no cloud, no broker, no separate database.

It is not a prompt wrapper or a cloud RAG store. It is a complete agentic memory architecture in five cooperating organs: memory (embedded vector graph + bounded evidence expansion), validation (a pre-execution behavioral guard that lives as an ordinary memory namespace), goals (a state machine that refuses `done` until work has taught something), external signal integration (local `fastembed` or declared remote bridge — graceful, never silent), and team sharing over plain `git` with validated replay and preserved conflicts.

---

## Why it wins (evidence, not branding)

**Claim 1 — Done means something.** `task_complete` is a single guarded entrance; it fails with a named JSON error until an `EXTRACTED_FROM` edge links back to the Goal. The pre-push `task gate --strict` hook stops your `git push` if Goals are unfinished or unconsolidated. A reminder or system prompt cannot enforce that mechanically. (Proof: `task_complete` single funnel, Lock 2 requirement at `docs/superpowers/specs/2026-10-23-readme-restructure-design.md`; session `current.md` lines 258, 262, 89.)

**Claim 2 — The guard is a namespace, not a reminder.** Rules are stored via `neurostrata_add_memory` as `guard_rule` Engrams in the same `guard` namespace as everything else. Every proposed `bash` or file-write is embedded, ranked, and either rejected (`DeterministicReject`, `SandboxReject`) or dry-run inside a `podman --network=none --rm` container with a 5s timeout — before any byte hits disk. (Proof: verdict vocabulary and hostile-path canonicalization at `docs/organs/prefrontal-cortex.md`, lines 211–243.)

**Claim 3 — One binary, embedded DB, no silent failures.** `LadybugDB` (embedded C++ engine) runs inside the Rust binary; `fastembed` (`NomicEmbedTextV15` / `BGEBaseENV15`, 768d) is the default dendritic branch. If you declare a remote bridge (`embedders.json`), missing keys are reported explicitly instead of silently falling back to an incompatible width — preserving your distance space over time. (Proof: graceful-degradation law at `docs/organs/dendritic-bridge.md`; `docs/FEATURES.md` memory + security rows.)

---

## The three things that make it structurally different (not just “more features”)

1. **Your agent’s work lifecycle is coupled to memory consolidation at the binary level.** A `done` task requires a recorded lesson; the SAS gate blocks pushes until it exists. No other agentic memory system (Mem0, Zep, Letta/MemGPT, basic RAG MCP) couples task state and knowledge consolidation inside the binary — they are storage layers with optional hooks.
2. **The behavioral guard is the same database as the memory.** Rules are not external YAML or config files; they are `guard_rule` Engrams that live in `.NeuroStrata/db`, are embedded immediately, and apply to every future validation. You teach once; the cortex applies forever.
3. **Memory travels over `git`, not a service.** `SynapticMesh` drains durable WAL outbox records into wire-format files (`synaptic-mesh-record/v1`) and publishes via `git push`. Pulls are validated against schema, namespace, lifecycle merge table, and conflicts preserved loudly — not silently resolved. Private rows stay sovereign; procedure definitions travel without their runtime counters. There is no SaaS broker, no MQTT broker, no REST dependency.

---

## How Laya / Jev make it different

`LadybugDB` is the embedded graph/vector engine (not a separate process). `Laya` is the local judgment layer; its current duplicate-judgment path returns empty results — meaning the embedded engine does not rely on an external judgment service to function. `Jev` (`https://api.typesafe.ai/v1`) provides optional remote duplicate judgments — not embeddings — so the distance ranking remains consistent whether local or bridged. Neither replaces the embedded store; both extend it. A bridge declared without its key refuses to fall back silently, protecting recall quality over time.

---

## Why you should try it

If you manage architecture docs across worktrees, run agents that forget rules, or have ever been the continuity mechanism between chat sessions, this is the only single-binary solution that gives your agent a conscience (the guard), a conscience-check (the sandbox), a goal-system that demands teaching (the Central Executive), a team mesh over `git` (SynapticMesh), and a persistent session log (Episodic Buffer) — all backed by one embedded database with zero cloud dependence. No prompt tricks. No extra containers. No SaaS account.

Keep your agents. Stop being their only continuity mechanism.

---

## Quick start (3 commands)

```bash
scripts/build.sh                  # Rust + CMake + C++20; builds embedded LadybugDB
neurostrata-mcp hooks install     # installs SAS pre-push gate per repo
neurostrata_bootstrap             # new project: AGENTS.md + first mandatory Goal
```

Local daemon (`127.0.0.1:34343`) serves `/validate`, `/tasks/gate`, `/graph`, `/mcp`, `/bus/metrics`, `/backup`, `/shutdown`. Obsidian plugin reads `.NeuroStrata/db` directly — no server needed.

---

## Where the deep docs live

- [Memory / SynapticGraph](docs/architecture/synapticgraph.md) · [Tri-Strata namespaces](docs/architecture/tri-strata.md)
- [Behavioral Guard](docs/organs/prefrontal-cortex.md) · [Goals / Central Executive](docs/organs/central-executive.md) · [Embedding / Bridge](docs/organs/dendritic-bridge.md) · [Thalamic Bus / Events](docs/organs/thalamic-bus.md) · [Procedural Memory](docs/organs/procedural-memory.md)
- [Team Mesh / SynapticMesh](docs/mesh/synaptic-mesh.md)
- [Full feature inventory + plain-English benefits](docs/FEATURES.md)
- [Cognitive science grounding](docs/science/nomenclature.md) · [Why standard memory fails](docs/science/why-memory-fails.md)
- [Security / Compliance](docs/ops/security.md) · [Upgrading / Crashes](docs/ops/upgrading.md) · [CLI ref pointer](docs/ops/cli-reference.md) · [Obsidian plugin](docs/ops/obsidian.md)
- [Per-release changelog](docs/releases/changelog/) — 1.11.0 (recall closure), 1.10.1 (retirement coherence), 1.10.0 (source anchors + gate honesty), 1.9.0 (mesh), 1.8.0 (bus + procedural memory + archive)

---

## References (evidence anchors only — for full citation details see `docs/science/nomenclature.md`)
- [1] `docs/superpowers/specs/2026-10-23-readme-restructure-design.md` — restructuring design, Lock 2 / task gate / guard namespace / source-anchor registry.
- [2] `docs/FEATURES.md` — feature inventory (engineering primitive / user benefit / evidence line) per organ.
- [3] `README.md` (original lines 211, 246, 335, 61) — Prefrontal Cortex (§211), Central Executive (§246), SynapticMesh (§335), Zero-Overhead (§61).
- [4] `docs/organs/prefrontal-cortex.md` — verdict vocabulary (DeterministicReject / SandboxReject / SandboxPass{Low,High}Fidelity / ApprovedFailOpen), hostile-path canonicalization (`/etc`, `/`, `..`), dry-run container command (`podman run --rm --network=none ... 5s timeout`).
- [5] `docs/mesh/synaptic-mesh.md` — `synaptic-mesh-record/v1` wire format, validated pull (schema + namespace + lifecycle merge table, conflicts preserved loudly), private rows sovereign, procedure definitions without runtime counters, permanence disclosure.
- [6] `docs/organs/dendritic-bridge.md` — `embedders.json` strict JSON; bridge exists only when both `base_url` + `api_key_env` declared; graceful local `fastembed` (`NomicEmbedTextV15` / `BGEBaseENV15`, 768d); explicit refusal on missing key (no silent width-swap).
- [7] `docs/pitch-astrum.md` line 29–31 — `Laya` judgment layer; `Jev` remote duplicate judgments (`https://api.typesafe.ai/v1`); neither replaces `LadybugDB`.

## License
MIT — [`LICENSE`](LICENSE). You can use it, keep it, close-source it. Don't sue me.
