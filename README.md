# 🧠 NeuroStrata

![NeuroStrata Banner](docs/assets/NeuroStrata-banner.png)

**Permanent agent memory, embedded in a single Rust binary.** Zero cloud. Zero separate database service. Zero external broker. The agent's rules, goals, and lessons live inside `.NeuroStrata/db` (LadybugDB, embedded C++ engine compiled from vendored source via CMake / C++20) and travel with the project over `git` via `SynapticMesh`.

If you manage architecture docs across worktrees and have ever become the continuity mechanism between chats, the reason is structural: standard RAG dumps 50-page docs into context and lets them fall out. NeuroStrata enforces that work teaches something before it closes (`done` requires an `EXTRACTED_FROM` edge; the SAS gate stops `git push` otherwise), validates actions against rules before they touch disk (semantic match against `guard_rule` Engrams, optional `podman --network=none` dry-run), and shares memory with teammates over plain `git` with validated replay rather than a SaaS broker.

The five parts are engineering, not branding:

- **Memory as Pointers, Not Pages (SynapticGraph):** Engrams store `{file, lines}` pointers alongside vector embeddings so the agent reads exactly the bytes it needs (`docs/architecture/synapticgraph.md`). Retrieval scans embedded `LadybugDB` (`src/store/ladybug.rs:87`, `:146-151`, `:224-241`) — boosted distance + bounded graph expansion, no lexical/BM25, no RRF fusion.
- **Behavioral Guard (Prefrontal Cortex):** Every proposed `bash` or file-write is embedded, ranked by similarity against rules you have taught (`guard_rule` memories), and either rejected (`DeterministicReject`, `SandboxReject`) or dry-run inside a `podman --network=none --rm` container before any byte hits disk. You teach once (`add_memory`); the cortex applies forever (`docs/organs/prefrontal-cortex.md`).
- **Goals / Done-Funnel (Central Executive):** Work moves through `open → in_progress → blocked → done`, but `done` has one guarded entrance (`neurostrata_task_complete`) that refuses unless the work produced a recorded lesson (`EXTRACTED_FROM` edge back to the Goal). The pre-push `task gate --strict` hook stops `git push` while unfinished or unconsolidated Goals exist. A reminder cannot enforce that — the binary does (`docs/organs/central-executive.md`).
- **Embedding / Bridge (Dendritic Bridge):** The engine embeds locally (`fastembed`: `NomicEmbedTextV15` / `BGEBaseENV15`, 768d) inside the Rust binary — no separate database service or cloud account required (`docs/organs/dendritic-bridge.md`). You can declare a remote bridge (`embedders.json`) for external providers; if declared but the key is missing, it refuses silently — preserving distance-space integrity over time rather than mixing incompatible embeddings quietly.
- **Team Sharing Over Plain Git (SynapticMesh):** Memory travels through a durable WAL outbox (`synaptic-mesh-record/v1`) that drains into wire-format files, then pushes via `git push`. Pull validates schema / namespace / id-match / lifecycle merge; conflicts stay preserved loudly — not resolved silently — and private rows stay sovereign. No broker, no SaaS, no MQTT (`docs/mesh/synaptic-mesh.md`).

**How Laya and Jev change nothing fundamental — and that is the point.** `LadybugDB` is the embedded engine. `Laya` (`src/judgment/laya_onnx.rs`) is the local judgment layer; its duplicate-judgment path currently produces empty results — meaning embedded retrieval does not depend on it. `Jev` (`src/judgment/typesafe_jev.rs`, `https://api.typesafe.ai/v1`) provides optional remote duplicate judgments — not embeddings — to keep distance ranking consistent if bridged. Neither replaces the embedded store; both extend it. Declaring a bridge changes which judgments feed ranking, never the database itself.

**Build evidence:** `scripts/build.sh` (Linux/macOS) / `scripts/build.ps1`; requires Rust + CMake + C++20 (`src/store/ladybug.rs` includes `common/assert.h` which needs `<format>`). The binary registers with clients via `stdio`; a local daemon on `127.0.0.1:34343` serves `/validate` (guard), `/tasks/gate`, `/graph`, `/mcp`. No Docker, no REST APIs, no MQTT broker.

**The only claim that matters:** no other agentic memory system couples task lifecycle (`done` = must have taught) to memory consolidation inside the binary, runs a behavioral guard as a namespace (not a config reminder), and publishes team memory over `git`. This is not a wrapper; it is the architecture.

---

## Source references (direct, not self-citing docs)

- `src/store/ladybug.rs:1408-1429` — embedded node + relation tables; `array_distance` ranking; bounded expansion.
- `src/store/ladybug.rs:87`, `:146-151`, `:224-241` — retrieval path (boosted distance + graph expansion, no lexical/BM25, no RRF fusion).
- `src/guard/semantic.rs:26-51` — guard semantic evaluation against `guard_rule` namespace.
- `src/store/ladybug.rs:209-223` / `:281` / `:341` — outgoing/incoming direction read via typed boolean (`row_flag_true`) — evidence orientation is correct after 1.10.1 fix (line 287, 288, 348, 4515, 4532, 6703, 6786 verified).
- `src/store/ladybug.rs:2370-2386` — access-count reinforcement (only rises, never decays; rules never autonously deleted).
- `src/task/` — state machine; `neurostrata_task_complete` single entrance; `EXTRACTED_FROM` edge required.
- `src/events/` — `ThalamicBus`; `ThalamicPulse` enum (`Created` / `Superseded` / `Archived` / `GuardedActionFired`); subscribers (`EpisodicPointerEcho`, `ExportFreshnessDirty`, `GuardEventLog`).
- `src/embed.rs:300-324` — bridge resolution; graceful fallback to `fastembed`; explicit refusal when key missing (`NEUROSTRATA_MODEL`, `embedders.json`).
- `src/synaptic_mesh/acceptance.rs:15-16` — mesh validation (schema, namespace, id/filename, lifecycle merge, conflict preservation).
- `src/daemon.rs:525-540` — loopback-only HTTP routes (`/validate`, `/graph`, `/mcp`, etc.).
- `src/store/ladybug.rs:57-63` / `:1275-1280` — `metadata.archived` tombstone filter; `archive_memory` refusal without `allow_global`.
- `docs/FEATURES.md` — feature inventory with engineering primitive / benefit / evidence line per organ.

---

## Quick start (verified against installed binary, not aspirational)

```bash
# Build requires CMake + C++20 toolchain (LadybugDB C++ engine from vendored source)
scripts/build.sh

# Per-repo gate (required, not optional — pre-push hook blocks unfinished Goals)
neurostrata-mcp hooks install

# New project onboarding (writes AGENTS.md + first mandatory Goal + ordered instructions)
neurostrata_bootstrap

# Existing project onboarding (scans repo, proposes rules, creates integration Goals)
neurostrata_task_setup
```

The daemon serves `127.0.0.1:34343`; `neurostrata-mcp status` reports health (`0` = healthy, `1` = safe to start exactly one, `2` = finishing). A second daemon is refused at the lock. `neurostrata-mcp backup` works with or without one.

---

## License
MIT — [`LICENSE`](LICENSE). Close-source or open — do not sue me.
