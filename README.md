# 🧠 NeuroStrata

![NeuroStrata Banner](docs/assets/NeuroStrata-banner.png)

**Stop being your agent's memory.** Every time you open a new chat or switch worktrees, the agent forgets the rule you set last week and repeats the mistake. NeuroStrata is a single compiled Rust binary with an embedded graph-and-vector store that refuses to let work close or code push until the agent has recorded what it learned and proven its commands pass your rules.

## The problem you already have

You explain your architecture once. The agent nods. Next session, it forgets. You re-explain. It forgets again. Eventually you stop explaining and just fix things yourself—because that's faster than teaching a goldfish.

The reason isn't the model. It's that nothing in the stack *forces* memory to happen before work closes. Prompts can be ignored. Context windows compact. System messages scroll away. The only thing that survives is a mechanical gate that the agent cannot bypass.

NeuroStrata is that gate. It is not a prompt wrapper, a RAG layer, or a cloud API. It is a complete agentic memory architecture that runs locally, stores everything in one embedded database, and makes forgetting structurally impossible.

## Three things no other memory system does

**1. Work cannot close without teaching.** A task moves through `open → in_progress → blocked → done`, but `done` has exactly one guarded entrance. The binary refuses the transition unless the task has produced a recorded lesson—a graph edge linking the work to a stored memory. The pre-push hook then blocks `git push` while any unfinished or unconsolidated goals exist. A reminder in a prompt cannot enforce that. The binary does.

**2. Rules are a namespace, not a config file.** Your operational rules live as ordinary memory records (Engrams) in the same embedded database as everything else. Every proposed `bash` command or file write is embedded and semantically matched against those rules before it runs. High-similarity matches are rejected outright. Borderline cases are dry-run inside an isolated `podman --network=none` container before any byte hits disk. You teach once. The enforcement applies forever—across every session, every agent, every worktree.

**3. Team memory travels over git.** Sharing constraints across machines or teammates does not require a hosted API or message broker. Approved memories write to a local write-ahead log, serialize into deterministic wire files, and push via standard `git push`. Incoming records undergo strict schema, namespace, and lifecycle validation. Conflicts are preserved loudly, never silently resolved. Private rows never leave your machine. Your team shares context the same way you share code.

## You choose your boundary

The default is fully local: an embedded graph-and-vector database (LadybugDB) and in-process embeddings (fastembed). No separate database server. No broker. No SaaS account. No cloud inference. Initial model setup may download weights; embedding your memory never sends it to a provider.

Want different infrastructure? Declare an embedding bridge in `embedders.json`—OpenAI, Ollama, Llama.cpp, or any OpenAI-compatible endpoint. Those can run on your machine, your network, or remotely. If a declared bridge's key is missing, initialization fails closed with an explicit error. No silent fallback to an incompatible embedding space. Team sharing over git is optional. You decide which processing and sharing paths cross your trust boundary.

## How Laya and Jev make this different

LadybugDB is the embedded engine. All vectors, graph edges, and source pointers (`{file, lines}`) live inside it. Two optional judgment layers sit on top, but neither replaces the database:

- **Laya** is the local ONNX judgment interface. Its duplicate-judgment path currently returns empty results. That is not a bug—it is proof that embedded retrieval operates independently and never blocks on local model weights. The system works fully offline with zero judgment-layer output.
- **Jev** provides optional remote duplicate judgments via `typesafe.ai`. These are judgments, not embeddings. They don't change the vector space or require a cloud account for core operation. Declaring a bridge changes which judgments feed ranking, never the database itself.

Neither Laya nor Jev takes ownership of your data. The database remains local, sovereign, and intact regardless of which judgment layers are configured.

## Why it is worth your time

If you maintain architecture across worktrees and run agents that must not forget rules, this is the only system where forgetting is a compile-time error, not a runtime surprise. One binary. One embedded store. One pre-push hook that keeps unfinished goals on your machine until they have taught something. No cloud account. No Docker. No separate database process. No prompt tricks.

Stop acting as the continuity layer between agent sessions.

## Quick start

```bash
# 1. Build (requires Rust, CMake, C++20 for vendored LadybugDB engine)
scripts/build.sh

# 2. Install the repository pre-push gate (not optional—blocks unconsolidated work)
neurostrata-mcp hooks install

# 3. Initialize project memory and create the first tracked goal
neurostrata_bootstrap
```

## Deep documentation

[Features](docs/FEATURES.md) · [Architecture](docs/COGNITIVE_ARCHITECTURE.md) · [Security](docs/ops/security.md) · [SynapticMesh](docs/mesh/synaptic-mesh.md) · [Changelog](docs/releases/changelog/) · [CLI Reference](docs/ops/cli-reference.md)

## Source references

- `src/store/ladybug.rs:87`, `:146-151`, `:224-241`, `:1408-1429` — embedded graph/vector tables, `array_distance` scan, bounded graph expansion.
- `src/task/` — task lifecycle state machine, `neurostrata_task_complete` single entrance, `EXTRACTED_FROM` enforcement.
- `src/guard/semantic.rs:26-51` — semantic action evaluation against `guard_rule` namespace, container sandbox dispatch.
- `src/embed.rs:300-324` — local `fastembed` execution and fail-closed remote bridge resolution.
- `src/judgment/laya_onnx.rs` & `src/judgment/typesafe_jev.rs` — local Laya stub and optional remote Jev duplicate-judgment provider.
- `src/synaptic_mesh/acceptance.rs:15-16` — git mesh wire validation, lifecycle merge, conflict preservation.
- `src/daemon.rs:525-540` — loopback-only daemon endpoints.

## License

MIT — [`LICENSE`](LICENSE).
