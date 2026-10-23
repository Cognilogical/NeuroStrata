# 🧠 NeuroStrata

![NeuroStrata Banner](docs/assets/NeuroStrata-banner.png)

Every time you spin up a new chat or switch git worktrees, your coding agent forgets the architectural rules it learned twenty minutes ago—making you the human glue holding its state together. NeuroStrata is a single compiled Rust binary with an embedded C++20 graph-and-vector store that refuses to let work close or code push until the agent records what it learned and proves its commands pass your rules.

## Three Structural Differences

Passive memory tools (Mem0, Zep, Letta/MemGPT, basic RAG MCP servers) treat memory as an optional scratchpad that agents can ignore when context gets tight. NeuroStrata enforces continuity at the binary boundary:

1. **Closing a task requires a recorded lesson (Central Executive):** Tasks move through a strict state machine (`open` → `in_progress` → `blocked` → `done`), and the only call that transitions a task to `done` (`neurostrata_task_complete`) rejects the request at the JSON-RPC layer unless an `EXTRACTED_FROM` graph edge links that task to a stored memory. Paired with the pre-push git hook (`neurostrata-mcp task gate --strict`), an agent literally cannot finish its ticket or push code until its findings are committed to the graph.
2. **Behavioral guard is an active memory namespace (Prefrontal Cortex):** Your operational rules are not system-prompt wishes that scroll out of the context window; they live in the database as `guard_rule` records (Engrams). Before a shell command or file edit touches disk, NeuroStrata embeds the proposed action, scores it against your stored rules, and either blocks known anti-patterns (`DeterministicReject` / `SandboxReject`) or dry-runs the command inside an isolated `podman --network=none --rm` container.
3. **Team memory travels over plain git (SynapticMesh):** Sharing engineering constraints across machines or teammates does not require a hosted API or message broker. Approved memories write to a local write-ahead log (`synaptic-mesh-record/v1`), serialize into deterministic wire files, and travel over standard `git push` and `--ff-only` pulls. Incoming records undergo strict schema, namespace, and lifecycle validation; divergent edits are flagged explicitly as conflicts rather than silently overwritten, and private local state never leaves your disk.

## You Choose Your Boundary

The default is fully local: an embedded graph-and-vector database (LadybugDB) and in-process embeddings (fastembed). No separate database server, broker, or external inference service is required. Initial model setup may download weights; embedding your memory does not require sending it to a provider.

Want different infrastructure? Declare an embedding bridge in `embedders.json`: OpenAI, or compatible Ollama and Llama.cpp endpoints. Those endpoints can be on your machine, your network, or remotely hosted. If the selected remote bridge's configured key is missing, initialization fails closed with an explicit error—not a silent switch to local inference. Optional remote duplicate judgment (TypeSafe Jev) is configured separately; it is not an embedding bridge. Team sharing over git is optional too. You decide which processing and sharing paths cross your trust boundary.

## How Laya and Jev Fit Without Touching the Core Store

All vectors, graph edges, and source pointers (`{file, lines}`) live inside the embedded LadybugDB engine (`src/store/ladybug.rs`), embedded locally in-process via `fastembed` (`NomicEmbedTextV15` / `BGEBaseENV15`, 768d). You never need an external service to store or query memory.

What sits above the store is a replaceable judgment layer that refines candidate ranking and duplicate detection without ever altering the underlying database:

- **Laya (`src/judgment/laya_onnx.rs`):** The local ONNX judgment interface. Its duplicate-judgment path currently returns empty output—proving that core embedded graph-and-distance retrieval operates independently and never blocks on local model weights.
- **Jev (`src/judgment/typesafe_jev.rs`):** An optional remote judgment bridge via `typesafe.ai` (`https://api.typesafe.ai/v1`) that returns typed duplicate judgments—not embeddings—to sharpen ranking when configured.

Neither Laya nor Jev replaces LadybugDB, and neither takes ownership of your data. Configuring a bridge (`embedders.json`) only changes which judgment signals inform ranking; the database remains local, sovereign, and intact. If a declared remote key is missing, NeuroStrata fails closed rather than silently mixing incompatible vector spaces.

## Why It Is Worth Your Time

If you maintain architecture across worktrees and run agents that must not forget rules, this is the only system where forgetting is a compile-time error, not a runtime surprise. No cloud account. No Docker. No separate database process. One binary, one embedded store, one pre-push hook that keeps unfinished goals on your machine until they've taught something. Stop acting as the continuity layer between agent sessions.

## Quick Start

```bash
# 1. Build the single binary (requires Rust, CMake, and a C++20 compiler for vendored LadybugDB)
scripts/build.sh

# 2. Install the repository pre-push gate to block unconsolidated work from pushing
neurostrata-mcp hooks install

# 3. Initialize project memory and create the first tracked goal (use neurostrata_task_setup for existing repos)
neurostrata_bootstrap
```

## Deep Documentation

[`docs/FEATURES.md`](docs/FEATURES.md) · [`docs/COGNITIVE_ARCHITECTURE.md`](docs/COGNITIVE_ARCHITECTURE.md) · [`docs/architecture/synapticgraph.md`](docs/architecture/synapticgraph.md) · [`docs/organs/central-executive.md`](docs/organs/central-executive.md) · [`docs/organs/prefrontal-cortex.md`](docs/organs/prefrontal-cortex.md) · [`docs/organs/dendritic-bridge.md`](docs/organs/dendritic-bridge.md) · [`docs/organs/procedural-memory.md`](docs/organs/procedural-memory.md) · [`docs/organs/thalamic-bus.md`](docs/organs/thalamic-bus.md) · [`docs/mesh/synaptic-mesh.md`](docs/mesh/synaptic-mesh.md) · [`docs/ops/security.md`](docs/ops/security.md)

## Source References

- `src/store/ladybug.rs:87`, `:146-151`, `:224-241`, `:1408-1429` — embedded C++20 graph/vector tables, `array_distance` scan, and bounded graph expansion.
- `src/store/ladybug.rs:209-223`, `:2370-2386` — typed boolean edge orientation (`row_flag_true`) and monotonic access reinforcement.
- `src/task/` — task lifecycle state machine and `neurostrata_task_complete` `EXTRACTED_FROM` enforcement.
- `src/guard/semantic.rs:26-51` — semantic action evaluation against the `guard_rule` namespace and container sandbox dispatch.
- `src/embed.rs:300-324` — local `fastembed` execution and fail-closed remote bridge resolution (`embedders.json`).
- `src/judgment/laya_onnx.rs` & `src/judgment/typesafe_jev.rs` — local Laya stub and optional remote Jev duplicate-judgment provider.
- `src/synaptic_mesh/acceptance.rs:15-16` — git mesh wire validation, lifecycle merge, and conflict preservation.
- `src/daemon.rs:525-540` — loopback-only daemon endpoints (`127.0.0.1:34343`: `/validate`, `/tasks/gate`, `/graph`, `/mcp`).

## License

MIT — [`LICENSE`](LICENSE).
