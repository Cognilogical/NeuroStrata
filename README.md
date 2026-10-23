# 🧠 NeuroStrata
![NeuroStrata Banner](docs/assets/NeuroStrata-banner.png)

**The memory layer your AI is missing — permanent, enforceable, local-only.**

[![Rust](https://img.shields.io/badge/Rust-1.75+-000000?style=flat-square&logo=rust)](https://www.rust-lang.org/)
[![MCP](https://img.shields.io/badge/Protocol-MCP-blue?style=flat-square)](https://modelcontextprotocol.io/)
[![LadybugDB](https://img.shields.io/badge/Vector%20DB-LadybugDB-orange?style=flat-square)](https://ladybugdb.com/)

Agents lose context and repeat mistakes across chats. NeuroStrata is one Rust binary that gives your AI a permanent, enforceable memory — with a conscience that blocks bad actions before they run and a goal-system that refuses to finish empty-handed.

**Embedded LadybugDB · stdio-only MCP · loopback daemon · zero Docker / zero cloud · secret-scrubbed · Cypher-escaped.** Nothing leaves the machine.

---

## Why it wins (3 claims, evidence-linked)

1. **Task lifecycle locked to memory consolidation** — `neurostrata_task_complete` refuses `done` until an `EXTRACTED_FROM` edge exists; the pre-push SAS gate (`task gate --strict`) blocks unfinished work. No passive RAG store does this at the binary level. (Proof: README §Central Executive, Lock 2 / line 89.)
2. **Pre-execution behavioral guard as a memory namespace** — rules live in `guard` namespace as ordinary `guard_rule` Engrams; every state-mutating action is semantically matched, optionally dry-run inside a network-isolated `podman --network=none` container, and refused with a named verdict (`DeterministicReject` / `SandboxReject`). (Proof: §Prefrontal Cortex, lines 211–243; verdict vocabulary 224–234.)
3. **Single-binary, embedded-vector, git-native team mesh** — LadybugDB + `fastembed` dendrite in-process; `embedders.json` declares remote bridges; no silent fallback on missing keys. SynapticMesh syncs over plain git with validated pull, lifecycle merge table, conflicts preserved loudly — no SaaS broker. (Proof: §Zero-Overhead 61–67, §Security 515–522, §SynapticMesh 335.)

---

## The five organs (TL;DR)

| # | Organ | What it does for you |
|---|-------|---------------------|
| 🧠 | **SynapticGraph + Engrams** | Memory as pointers (`{file, lines}`), not pages — compact reading, instant grounding. [Deep → docs/architecture/synapticgraph.md] |
| 🛡️ | **Prefrontal Cortex** | Blocks bad actions with named verdicts + optional sandbox. [Deep → docs/organs/prefrontal-cortex.md] |
| 🎯 | **Central Executive** | Goals move `open → in_progress → blocked → done`; only `done` with extracted memory passes the gate. [Deep → docs/organs/central-executive.md] |
| 🌉 | **Dendritic Bridge** | Remote embedder or fully local (`fastembed`) — graceful, never silent. [Deep → docs/organs/dendritic-bridge.md] |
| 🕸️ | **SynapticMesh** | Team memory over `git push`; conflicts stay visible. [Deep → docs/mesh/synaptic-mesh.md] |

Also: **Thalamic Bus** (typed mutation pulses, export-freshness gate), **Procedural Memory** (rehearsal-stamped knowing-how), **Episodic Buffer** (session logs with topic-switch markers), **Source-Anchor Registry** (multi-checkout provenance).

**Full feature inventory + benefit translations:** [`docs/FEATURES.md`](docs/FEATURES.md). **Cognitive-science naming & engineering equivalents:** [`docs/science/nomenclature.md`](docs/science/nomenclature.md). **Why standard AI memory fails:** [`docs/science/why-memory-fails.md`](docs/science/why-memory-fails.md).

---

## 🆕 What's New in 1.11.0 (brief)
**Recursion closure:** negative exemplars (`metadata.attempt`), domain registry per namespace (rejects unknown domain with remedy), snapshot domain strap, `domain_filter` + `recall` metrics sections. Vocabulary v6.

Full changelog split to [`docs/releases/changelog/`](docs/releases/changelog/) — per-release pages for 1.10.1 (retired-row coherence, supersession chain bounded + cycle-safe), 1.10.0 (source-anchor registry, gate distinction of history vs live), 1.9.0 (SynapticMesh), 1.8.0 (Thalamic Bus, export-freshness gate, procedural memory, archive tool, bus metrics).

---

## 🚀 Getting Started (compressed)

```bash
# Build (needs Rust + CMake + C++20; uses embedded LadybugDB)
scripts/build.sh

# Install binary
sudo mv dist/linux/neurostrata-mcp /usr/local/bin/

# Wire the guard + task gate (per repo)
neurostrata-mcp hooks install        # pre-push gate
neurostrata_bootstrap                 # new project (AGENTS.md + first Goal)
neurostrata_task_setup                # existing project
```

No Docker, no remote DB, no REST APIs. The daemon listens loopback only (`127.0.0.1:34343`) and serves `/validate`, `/tasks/gate`, `/graph`, `/mcp`, `/bus/metrics`, `/backup`, `/shutdown` — all inside the machine.

**Visualize memory:** the Obsidian plugin reads `.NeuroStrata/db` directly; 2D canvas, right-click prune, zero network ([docs/ops/obsidian.md](docs/ops/obsidian.md)).

---

## 🧬 Biological Nomenclature ↔ Engineering

| Term | Engineering Primitive |
|------|-----------------------|
| SynapticGraph | Knowledge engine (vector distance + bounded graph expansion) |
| Engram | Vector row with metadata, domain tags, graph edges |
| Synaptic Pruning | Access-count reinforcement; never auto-deletes |
| Tri-Strata | Global / Domain / Task namespace tiers |
| Pointer-Wiki / Compact Reading | Memory returns `{file, lines}` not full prose |
| Prefrontal Cortex | Behavioral guard (semantic match + Podman sandbox) |
| Central Executive | Goal management (state machine + done-funnel + SAS gate) |
| Dendritic Bridge | Remote embedder / graceful local `fastembed` |
| Thalamic Bus | In-process pub-sub (`ThalamicPulse` variants) |
| Thalamic Relay / Export-Freshness | `dirty` sentinel; `export-graph` refuses stale |
| Procedural Memory | `memory_type:"procedure"` with rehearsal budget |

Full table and science references: [`docs/science/nomenclature.md`](docs/science/nomenclature.md); broader cognitive-science grounding (Liu et al. 2023, Tulving 1972, O'Keefe & Nadel 1978, Brooks 1983, Webb/Mondal/Momennejad 2025, Liu/Ma/Li/Zhou NeurIPS 2024, Baddeley, Norman & Shallice 1986, Squire & Alvarez 1995) preserved in [`docs/references.md`](docs/references.md).

---

## 🛡️ Security & Compliance

- **Loopback-only:** agents speak stdio; daemon binds `127.0.0.1:34343`; nothing published beyond machine (mitigates LLM07 / remote RCE).
- **Secret scrubbing:** high-entropy payloads rejected, forcing a redaction loop (mitigates LLM06).
- **Cypher-hardening:** quote/backslash escaping in LadybugDB interpolations.
- **Guarded curation:** no destructive MCP tool exists — edit/delete/move are CLI/GUI only, one id at a time (mitigates LLM08).
- **Resilient soft locks:** done-funnel + SAS gate enforced in the binary and hook, no external tooling to approve (mitigates LLM09, "happy-path tunnel vision").

Full defense: [`docs/ops/security.md`](docs/ops/security.md).

---

## 📚 Documentation (split from this README)

- [Architecture / Memory core](docs/architecture/synapticgraph.md) | [Tri-Strata](docs/architecture/tri-strata.md)
- [Prefrontal Cortex — Behavioral Guard](docs/organs/prefrontal-cortex.md) | [Central Executive — Goals](docs/organs/central-executive.md) | [Dendritic Bridge](docs/organs/dendritic-bridge.md) | [Thalamic Bus](docs/organs/thalamic-bus.md) | [Procedural Memory](docs/organs/procedural-memory.md)
- [SynapticMesh — Team Git Sync](docs/mesh/synaptic-mesh.md)
- [Why memory fails / science](docs/science/why-memory-fails.md) | [Nomenclature table](docs/science/nomenclature.md)
- [Operations / Security / Obsidian](docs/ops/security.md) | [CLI Ref (pointer)](docs/ops/cli-reference.md) | [Upgrading / Crashes](docs/ops/upgrading.md)
- [Feature inventory (plain-English benefits)](docs/FEATURES.md) — the full competitive comparison, organized per organ.

---

## 🛠️ MCP Tool Reference (high-level)

**Memory:** `add_memory`, `search_memory`, `get_memory`, `get_snapshot`, `supersede_memory`, `archive_memory`, `ingest_directory`, `append_log`.
**Goals:** `task_create`, `task_claim`, `task_update`, `task_list`, `task_complete`, `task_validate`, `bootstrap`, `task_setup`.
**Guard:** `guard_validate`, `guard_learn`, `guard_list_rules`; daemon `POST /validate`.

Every tool is additive — nothing destroys a memory from the MCP surface.

---

## License
MIT. See [`LICENSE`](LICENSE). — I wrote it; you can use it, close-source it, or not — just don't sue me.

---

*Built entirely in Rust. No external network dependence at runtime. One binary, one embedded database, one standard.*
