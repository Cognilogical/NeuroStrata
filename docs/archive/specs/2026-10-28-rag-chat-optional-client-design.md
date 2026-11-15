# RAG Chat Optional Client Design

> Status: **APPROVED** (2026-10-28).
> Task: `neurostrata-miuu` (RAG chat optional client, OxiRS §1.13 client track).
> Companion: `docs/superpowers/specs/2026-10-28-graphql-curated-read-api-design.md`, `docs/superpowers/specs/2026-10-27-neurostrata-rdf-semantic-layer-design.md`.

## 1. Problem

`src/rag_chat.rs` currently exposes `build_prompt(context: &str, question: &str)` and `answer_offline(context: &str, question: &str) -> String`. These are not a retrieval client: there is no retrieval integration, no citation, no LLM provider abstraction, and no deterministic context assembly. NeuroStrata retrieval must remain LLM-free; chat is an optional client layer that calls retrieval.

## 2. Goal

Add an **in-house RAG chat client module** (`src/rag_chat.rs`, always compiled, no feature gate) that:

- retrieves context from a `VectorStore` + `Embedder` deterministically,
- builds a structured prompt with 1-based citations (`[1]` → memory id),
- supports a pluggable `LlmProvider` trait with an offline deterministic default,
- never puts an LLM in core retrieval,
- honors ReBAC and egress privacy,
- is usable from CLI, daemon, and MCP without blocking Tokio.

## 3. Constraints

- Core retrieval and store remain LLM-free.
- `src/rag_chat.rs` is always compiled in default builds (no `#[cfg(feature = "rag-chat")]`); there are no disabled stubs. Optional behavior is controlled by `RagChatConfig.enabled` (default `true`), not by compile-time features.
- `LlmProvider::answer` is `async` and uses `reqwest::Client` (non-blocking). No `reqwest::blocking`.
- Deterministic context assembly: same query + same store state → same hits, citations, and prompt.
- Tie-breaking: search hits are sorted by `(score.total_cmp, id)` before ranking.
- Privacy:
  - Local-only providers may include ReBAC-visible rows.
  - Remote providers may include only rows with `metadata.eligibility == "shareable"` (SynapticMesh gate) unless an explicit egress policy allows otherwise.
  - Principal is resolved from daemon/session auth context, never raw client JSON.
- No LLM in tests (offline provider only).
- UTF-8 safe truncation at `char` boundaries.

## 4. Scope

In scope:

- `RagChatClient` / `ask` orchestration.
- Prompt builder with citations.
- `LlmProvider` trait + `OfflineProvider` + `OpenAiCompatibleProvider`.
- CLI command `neurostrata-mcp chat ask`.
- Daemon HTTP endpoint `POST /chat/ask`.
- MCP tool `neurostrata_chat_ask`.

Out of scope:

- Streaming token responses.
- Fine-tuning or embedding LLMs.
- Multi-turn conversation state persistence (client-side only).

## 5. Approach

### 5.1 Retrieval (`src/rag_chat.rs`)

```rust
pub struct RagHit {
    pub rank: usize,
    pub memory_id: String,
    pub score: f32,
    pub snippet: String,
    pub derivation: Vec<String>,
}

pub struct RagContext {
    pub query: String,
    pub hits: Vec<RagHit>,
    pub total: usize,
}

pub async fn retrieve_context(
    store: Arc<dyn VectorStore>,
    emb: Arc<dyn Embedder>,
    namespace: &str,
    principal: &str,
    query: &str,
    limit: usize,
    include_tasks: bool,
    include_archived: bool,
) -> Result<RagContext, ChatError>
```

- Embeds `query` via `emb.embed(query).await`.
- Calls `store.search_with_domain(namespace, vector, limit, include_tasks, include_archived, &[], Some(query), false).await`.
- Uses `outcome.results`.
- Sorts results by `(score.total_cmp(&other.score), id.cmp(&other.id))` before assigning `rank`.
- For each result, builds `snippet`: first 280 Unicode scalar values of `content` (trimmed, append `…` if truncated), using `content.chars().take(280).collect::<String>()`.
- `derivation` is populated from `SearchResult.evidence`:
  - flatten `evidence[*].derivation` for direct/expansion evidence,
  - deduplicate preserving first occurrence,
  - if no evidence, use `["vector"]`.
- `total = results.len()` before truncation to `limit` (already limited by store).
- Applies ReBAC filtering before ranking: include result iff `rebac::allowed(principal, result.payload.user_id, "viewer", &result.payload.metadata["visible_to"])` where absent `visible_to` means unrestricted. (Exact 4-arg signature per ReBAC spec §5.2.)
- Applies egress policy for remote providers: if `provider.is_remote()`, additionally require `result.payload.metadata["eligibility"] == "shareable"`.
- If no survivors, `hits` is empty.

### 5.2 Prompt builder

```rust
pub fn build_prompt(ctx: &RagContext, question: &str, system: Option<&str>, max_context_chars: usize) -> String
```

Output format:

```
System: {system or default}
Context:
[1] (id=mem-abc, score=0.12) <snippet>
[2] (id=mem-def, score=0.34) <snippet>
...
Question: {question}
Answer (cite sources as [n]):
```

- Snippets have newlines collapsed to single spaces.
- Context block is built by iterating hits in rank order and appending whole citation lines until adding the next line would exceed `max_context_chars` (counting the full rendered line). Dropped hits are omitted from the prompt; their ids remain in `ChatResponse.citations` only if included in the prompt. `ChatResponse.context_count` equals the number of hits rendered in the prompt.
- Empty context renders `Context:\nNo relevant context found.\n`.

### 5.3 LLM provider trait

```rust
#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn name(&self) -> &str;
    fn is_remote(&self) -> bool { false }
    async fn answer(&self, prompt: &str) -> Result<String, ChatError>;
}
```

- `OfflineProvider`: deterministic, no network. Returns a string containing the question and number of citations in the prompt; no LLM.
- `OpenAiCompatibleProvider` (always compiled; `reqwest` already a dependency):
  - fields: `base_url: String`, `api_key: Option<String>`, `model: String`, `timeout: Duration`, `client: reqwest::Client`.
  - `base_url` must be validated at construction: require `http://` or `https://`, reject userinfo, reject non-loopback hosts unless `allow_remote_base_url = true` in config, and strip trailing `/`.
  - `api_key`, if present, is read from the environment variable named by config at construction; the env var name is validated against an allowlist (`OPENAI_API_KEY`, `NEUROSTRATA_RAG_API_KEY`) to prevent secret exfiltration. If the env var is unset, construction fails with `ChatError::Config`.
  - POSTs to `{base_url}/v1/chat/completions` with JSON `{ "model": ..., "messages": [{"role":"user","content": prompt}], "temperature": 0 }`.
  - Uses `reqwest::Client::post(...).timeout(timeout).send().await`.
  - Returns `choices[0].message.content`.
  - Errors become `ChatError::Provider`.

### 5.4 Chat orchestration

```rust
pub struct ChatResponse {
    pub answer: String,
    pub citations: Vec<String>,   // memory ids rendered in prompt
    pub context_count: usize,
    pub provider: String,
}

pub async fn ask(
    store: Arc<dyn VectorStore>,
    emb: Arc<dyn Embedder>,
    namespace: &str,
    principal: &str,
    question: &str,
    limit: usize,
    system: Option<&str>,
    max_context_chars: usize,
    provider: Arc<dyn LlmProvider>,
    include_tasks: bool,
    include_archived: bool,
    remote_egress_allowed: bool,
) -> Result<ChatResponse, ChatError>
```

- Retrieves context.
- If provider is remote and `remote_egress_allowed` is false, returns `ChatError::EgressDenied`.
- Builds prompt with `max_context_chars`.
- Calls provider.
- Returns answer + citations (ids rendered in prompt) + context count + provider name.
- If provider fails, propagates `ChatError`.

### 5.5 Privacy

- `retrieve_context` filters hits by ReBAC (`allowed`).
- Remote provider additionally filters by `eligibility == "shareable"`.
- If no shareable/restricted hits, context is empty; prompt says no context.
- Principal resolution:
  - daemon: from session/auth context (ReBAC spec §5.3, GraphQL spec §5.3).
  - MCP: from configured `default_principal` or session principal.
  - CLI: when daemon is running, proxy to daemon; standalone CLI uses configured principal.
- `metadata.visible_to` and ReBAC ACLs are honored.

### 5.6 CLI and daemon

- CLI: `neurostrata-mcp chat ask --namespace <ns> --question "..." [--limit 10] [--provider offline|openai] [--system "..."]`.
- Because LadybugDB is single-writer and locked by the daemon, CLI:
  - checks daemon health via `gate_verify_daemon`,
  - if daemon responsive, proxies to `POST /chat/ask`,
  - otherwise opens store directly (standalone mode) and verifies no daemon lock.
- Daemon: `POST /chat/ask` body `{ namespace, question, limit?, provider?, system? }` → `ChatResponse` JSON. Always registered (not optional).
- MCP tool: `neurostrata_chat_ask(question, namespace?, limit?, system?)` uses configured provider but forces offline provider for MCP calls to avoid secrets in tool calls.

### 5.7 Configuration

Nested in `Config` as:

```rust
pub struct RagChatConfig {
    pub enabled: bool,                 // default true
    pub provider: String,              // default "offline"
    pub base_url: Option<String>,      // for openai-compatible
    pub api_key_env: Option<String>,   // allowlisted env var name
    pub model: String,                 // default "gpt-4o-mini"
    pub max_context_chars: usize,      // default 4000
    pub timeout_secs: u64,             // default 30
    pub allow_remote_base_url: bool,   // default false
    pub remote_egress_allowed: bool,   // default false
    pub default_principal: String,     // default "local-user"
}
```

- `Config.rag_chat: RagChatConfig` with `#[serde(default)]`.
- No flat `rag_chat_*` keys.
- `base_url` allowlist and env var allowlist prevent SSRF/secret exfiltration.
- If `enabled = false`, chat tools return a disabled message but module remains compiled.

### 5.8 Determinism

- Search results tie-broken by `(score, id)`.
- Prompt rendering is deterministic.
- Offline provider output is deterministic.

## 6. Interfaces

- `src/rag_chat.rs`: `RagContext`, `RagHit`, `build_prompt`, `retrieve_context`, `LlmProvider`, `OfflineProvider`, `OpenAiCompatibleProvider`, `ask`, `ChatResponse`, `ChatError`.
- `LadybugStore`/`VectorStore`: uses `search_with_domain`.
- `Embedder`: `embed`.
- CLI: `chat ask` subcommand.
- Daemon: `POST /chat/ask`.
- MCP tool: `neurostrata_chat_ask`.
- Config: `Config.rag_chat`.

## 7. Edge cases

- Empty context: prompt states no context; offline answer notes lack of context.
- Provider timeout: `ChatError::Provider`, no partial answer.
- Namespace not found: empty context, no error.
- Secret handling: API key read from allowlisted env var; never logged.
- UTF-8: truncation at char boundaries.
- Remote egress disabled: `ChatError::EgressDenied`.

## 8. Testing

- Unit: prompt format and citation indices.
- Unit: snippet truncation (UTF-8 safe).
- Unit: offline provider determinism.
- Unit: context char cap (whole-line dropping).
- Unit: tie-breaking.
- Unit: remote egress filtering.
- Integration: retrieve_context uses store results.
- Integration: ReBAC filtering excludes restricted rows.
- Integration: CLI proxies to daemon when running.

## 9. Acceptance

In-house RAG chat client available in default build; deterministic context + citations; pluggable async LLM (offline default); no LLM in core; privacy honored; no core dependency; no stubs.

## 10. Files touched (expected)

- `src/rag_chat.rs`: full implementation.
- `src/config.rs`: nested `RagChatConfig`.
- `src/server.rs`: MCP tool.
- `src/main.rs`: CLI subcommand + daemon proxy.
- `src/daemon.rs`: HTTP endpoint.
- `Cargo.toml`: no feature changes (reqwest already present).