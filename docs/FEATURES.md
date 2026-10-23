# FEATURES — NeuroStrata: Plain-English Feature Inventory

> This inventory translates every organ into what it means *for you* and why no other memory system offers it. Cognitive-science branding preserved; engineering primitives explained afterwards.

| Organ | Engineering Primitive | What it does for you | Evidence line |
| :--- | :--- | :--- | :--- |
| **Memory Core** | SynapticGraph + LadybugDB embedded vector store | Permanent, project-scoped memory — your AI never re-learns a stack across chats. Pointers, not pages. | §2 |
| **Memory Core** | Engram (`{file, lines}` pointer rows) | Memory returns exact file+line refs instead of dumping docs into context, so reasoning stays intact. | line 17 (Pointer-Wiki) |
| **Memory Core** | Dual-Track Bi-Temporal Graph (vector distance + bounded expansion) | Skips BM25 ranking entirely — retrieval relies on true semantic similarity plus structural neighbours, no RRF. | line 17 erratum |
| **Memory Core** | Eidetic Recall (boot-time top-5 snapshot) | Session starts grounded instantly — agent doesn't burn tokens blindy searching a repo. | §Zero-Overhead line 67 |
| **Memory Core** | Synaptic Pruning (access-count only, never auto-delete) | Your rules stay retrievable; nothing you built gets quietly dropped while it was still live. | line 79 |
| **Memory Core** | Negative exemplars (`metadata.attempt`, 1.11) | A failed approach isn't forgotten — it's evidence the agent didn't give up; it surfaces alongside successful facts. | 1.11 headline line 27 |
| **Tri-Strata** | Global / Domain / Task namespaces | No cross-project contamination: company rules never leak into a bug fix. | §122, "Tri-Strata Model" |
| **Tri-Strata** | Domain registry (per namespace, 1.11) | Every domain write is checked against registered slugs before it lands; unknown writes are refused with the remedy. | 1.11 headline line 27 |
| **Prefrontal Cortex** | Behavioral Guard (`guard_rule` namespace + Podman dry-run) | State-mutating actions are verdicted before they run — `rm -rf /` rejected with a named rule. | §Prefrontal Cortex line 213 |
| **Prefrontal Cortex** | Deterministic + sandbox verdict vocabulary | Semantic match → immediate reject; optional podman container — either verdict fires before any byte hits disk. | §Prefrontal Cortex lines 224–234 |
| **Central Executive** | Goal lifecycle state machine + done-funnel | Work is "done" only when it has taught the brain something via an extracted Engram — experience doesn't evaporate. | §Central Executive line 89 |
| **Central Executive** | Zero-Action Start + exclusive claim lease | No edits before a goal exists & is claimed by this session; duplicate work fails loudly at claim time. | §Central Executive line 258 |
| **Central Executive** | SAS pre-push gate (`task gate --strict`) | Finish pushes keep unfinished goals on-machine; unfinished or unconsolidated targets block the git push. | §Central Executive line 262 |
| **Central Executive** | Wiring panel + retired-row semantics | Wires described explicitly (auto vs reminder); retired rows never look like live work. | §Central Executive line 279–282 |
| **Dendritic Bridge** | External OpenAI-compatible embedder + local fastembed fallback | Bring your own embedder or run local — one file decides which, nothing breaks on offline runs. | §Dendritic Bridge line 323 |
| **Dendritic Bridge** | Explicit refusal on missing key (no silent width-swap) | If a bridge's API key is missing, you get a clear error weeks later rather than silently degraded recall. | §Dendritic Bridge line 325 |
| **Thalamic Bus** | In-proc pub-sub + typed pulses (Created/Superseded/Archived/GuardedActionFired) | Memory mutations propagate wires-to-wires — bookkeeping happens *because* a write happened, not because an agent remembered. | §Thalamic Bus line 92 |
| **Thalamic Bus** | Export-freshness gate (`dirty: true`) | `export-graph` refuses a stale export naming every dirty namespace at once — one clean flag clears it. | §Thalamic Bus line 52 |
| **Procedural Memory** | `memory_type:"procedure"` w/ rehearsal budget | Knowing-how survives forgetting: a finite rehearsal stamp strengthens or lapses it, no state lost mid-session. | 1.10–1.11 changelog line 55 |
| **SynapticMesh** | Git-transport team sync (durable WAL → wire records, validated pull, conflicts preserved loudly) | Team memories sync over plain `git push` — no broker, no SaaS, merge clashing preserved visibly. | §SynapticMesh line 339 |
| **Episodic Buffer** | Background session log + `### 🔄 Topic Switch` markers + 500KB rollover | Conversational context is recovered via two-pass grep+read, not archaeology — nothing is lost between chats. | §Episodic Buffer line 201 |
| **Security** | Loopback-only daemon (`stdio` JSON-RPC) + secret scrubbing + Cypher escaping | OWASP Top-10 LLM mitigations active; agents cannot exfiltrate from this server, cannot drop data destructively via MCP. | §Security lines 518–522 |

## Why No Other System Offers This

Three things make NeuroStrata singular in the agentic-memory space:

1. **Task lifecycle is coupled to memory consolidation.** The Central Executive refuses `done` until an `EXTRACTED_FROM` edge exists. Mem0, Zep, Letta/MemGPT, and the common RAG-MCP passivity store facts but don't enforce that knowledge was produced. No competitor gates completion on extracted memory at the binary level.

2. **A pre-execution behavioral guard lives in the same store as everything else.** Rules are ordinary vector rows (`guard_rule`); every state-mutating action gets semantically matched then optionally dry-run before execution. No peer executes a guard organ — they are storage.

3. **Single-binary, zero-config cognition plus git-native mesh.** Embedded LadybugDB + in-proc fastembed, loopback-only daemon, secret scrubbing, and the published-via-git synaptic-mesh transport. Zep/Mem0 require cloud or self-hosted infra.
