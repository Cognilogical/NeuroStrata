# Design: External API Embedding Providers

**Status:** Verdict + design (co-architect submission, 2026-10-08)
**Scope:** Add remote (OpenAI-compatible) embedding provider alongside local fastembed; evaluate local model default.

## 1. Verdict

**Build it, scoped down. Do NOT build the general abstraction.**

The `Embedder` trait (`src/traits.rs:88`) already anticipates this ("Remote (Ollama), or Cloud (OpenAI)"). Cost to a solo dev is negligible (~$0.10 lifetime at text-embedding-3-small rates). Value is real: weak-hardware users, multilingual quality, offloading ingest bursts. But the proposal as scoped contains 4 entropy hazards — killed below.

### Killed (complexity without benefit)

| Feature | Verdict | Reason |
|---|---|---|
| Embedding response cache | ✗ | DB already stores vectors per memory. A cache duplicates the store with worse invalidation semantics. |
| Configurable retry policy | ✗ | Fixed policy (30s timeout, 1 retry on 429/5xx, 1s backoff). Knobs nobody tunes = entropy. |
| Silent fallback API→local | ✗ **hard** | Switches vector space mid-DB (e.g. 1536-dim OpenAI into 768-dim store). Corrupts search silently. Fail loudly; the existing `emergency_cache.jsonl` resilience pattern already absorbs outages. |
| Provider enum / registry (OpenAI vs Cohere vs …) | ✗ | One OpenAI-compatible client covers OpenAI, Cohere (`/v1` compat), vLLM, Ollama (`/v1`), LiteLLM, LocalAI. Per-provider code is pure entropy until a concrete incompatibility appears. |
| Batch embedding API | ✗ (phase 3) | Trait is single-text. Don't widen without a measured ingest bottleneck. |

### Kept

- Single OpenAI-compatible `RemoteEmbedder` implementing the existing trait unchanged.
- Dimension guard: API dims ≠ DB dims → refuse to start. Never pad; request target width from API when supported (`dimensions` param, Matryoshka).
- API-key-presence = enabled (matches `judgment_api_key` pattern).

## 2. Architecture

### Config: extend `embedders.json`, do NOT create a new section

Laws of physics: `~/.config/neurostrata/embedders.json` is already the "which embedder" registry, and `NEUROSTRATA_MODEL` is already the selector (`src/embed.rs:87-95`). A second config site duplicates both. Extend the existing entry schema (backward compatible via serde defaults):

```json
[
  { "model_name": "NomicEmbedTextV15", "dimensions": 768 },
  {
    "model_name": "openai-3-small",
    "provider": "openai-compatible",
    "base_url": "https://api.openai.com/v1",
    "api_model": "text-embedding-3-small",
    "api_key_env": "OPENAI_API_KEY",
    "dimensions": 768
  }
]
```

Rules:
- `provider` absent → local fastembed (all existing files keep working).
- `provider` present → remote. Key read from env var named by `api_key_env`; also accept literal `api_key` field for parity with `judgment_api_key`. Env preferred: keys in plaintext config are a wart we shouldn't replicate.
- `dimensions` remains declared, never probed: `configured_dimensions()` (`src/embed.rs:102`) must stay model-load-free — backup/restore depend on it (`src/main.rs:584,612`).
- Selection unchanged: `NEUROSTRATA_MODEL` name match, else first entry.

### Trait: unchanged

```rust
async fn embed(&self, text: &str) -> Result<Vec<f32>>;
fn dimensions(&self) -> usize;
```

`RemoteEmbedder` (new, ~120 LOC): reqwest async client — I/O-bound, so **no `spawn_blocking`** (that's CPU-inference-only, `src/embed.rs:151-155`). Timeout 30s, single retry on 429/5xx.

### Factory

New `embed::build_embedder() -> Result<Arc<dyn Embedder>>` reads the configured entry: remote → `RemoteEmbedder`, else → `FastEmbedder`. Replaces 3 call sites (`src/main.rs:452,640` + daemon path). Everything downstream consumes `Arc<dyn Embedder>` already — zero ripple.

### Error handling

1. Startup: remote entry whose `dimensions` ≠ store width → hard error naming both widths and the fix.
2. Runtime API failure → `embed()` returns Err. Daemon already degrades (memories to append-only cache, per resilience rule). No silent local substitution, ever — vector-space contamination is unrecoverable without full re-embed.
3. Truncation: `MAX_EMBED_TOKENS` is local-only. Remote: send as-is, let API error surface (OpenAI truncates at 8191; nomic-v2-moe at 512 — document per-entry `max_tokens` field as optional override, phase 2).

## 3. Local model recommendation

**Keep `NomicEmbedTextV15` as default now; upgrade to `NomicEmbedTextV2MoE` in phase 2, gated on two checks.**

| Candidate | Verdict |
|---|---|
| nomic-embed-text-v2-moe (768d, Matryoshka, 305M active) | ✓ phase 2. Same 768 width → no schema migration; multilingual; fastembed ≥5.1 supports it (on v7 — verify enum exists in impl). |
| BGE-M3 (1024d) | ✗ default. Breaks width compat with every existing DB. Allow via `embedders.json` for new installs. |
| Qwen3-Embedding-0.6B (1024d) | ✗ default, same reason. Not in fastembed model list at last check. |
| all-MiniLM-L6-v2 (384d) | ✗. Quality regression; 768→384 breaks width. |

**Trap: same-width ≠ same vector space.** Swapping v1.5→v2-moe keeps width but changes vectors for identical text; querying a v1.5-built DB with v2-moe embeddings silently degrades retrieval. Therefore phase 2 requires the **model stamp**:

- Record `(model_name, dimensions)` in DB metadata at creation (LadybugDB metadata table).
- Startup check: width mismatch → hard error; name mismatch (same width) → loud warning + recommend `reembed`.
- New `neurostrata-mcp reembed` command: streams memories, re-embeds with current embedder, rewrites vectors. This is also the general migration path for 768→1024 moves.

## 4. Migration strategy

1. Existing DBs (768d, v1.5, no stamp): stamp is absent → infer legacy `(NomicEmbedTextV15, 768)`, write it, continue. Zero user action.
2. User opts into remote model at different width → hard error at startup with three options: (a) pick a width-matching API model/`dimensions` param, (b) `reembed` to convert, (c) new DB path.
3. LadybugDB width is set at `LadybugStore::new` (`src/store/ladybug.rs:128`); re-embed in place requires store support for vector update — verify `upsert` overwrites vectors (it does per trait docstring) so reembed = read → embed → upsert, same width only. Width change = new DB + backfill, not in-place.

## 5. Implementation plan

**Phase 1 (this feature, ~200 LOC + tests):**
1. Extend `AcceptableEmbedder` with optional remote fields (serde defaults).
2. `RemoteEmbedder` struct + `impl Embedder` (reqwest, 30s timeout, 1 retry).
3. `embed::build_embedder()` factory; swap 3 call sites.
4. Startup dimension guard in factory.
5. Tests: stub HTTP server (wiremock or axum) — happy path, retry-on-500, dim-mismatch refusal, absent-key → local.

**Phase 2 (separate bead):** model stamp in DB metadata; `reembed` command; default bump to V2MoE after stamp ships (never before — uncontrolled silent drift otherwise).

**Phase 3 (only on demand):** `embed_batch` trait method, per-entry `max_tokens`, Cohere `input_type` quirks if a real user hits them.

## 6. Acceptance criteria

- `embedders.json` with a remote entry + env key set → daemon embeds via API; `NEUROSTRATA_MODEL` selects by name.
- No key / no provider field → byte-identical current behavior (local v1.5, existing DBs open).
- Width mismatch → process exits with actionable error, zero writes.
- API down at runtime → error propagates; no local fallback; daemon resilience path engages.
- `cargo test` green incl. new remote tests; no new mandatory config fields.
