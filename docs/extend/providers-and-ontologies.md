# Custom Providers, Ontologies & Action Packs

## Custom `Embedder` & `VectorStore` Implementations

The `Embedder` trait is the pluggable interface for embedding generation. Implementations must provide:

- `fn embed_text(&self, text: &str) -> Vec<f32>` — embed a single string
- `fn embed_batch(&self, texts: &[&str]) -> Vec<Vec<f32>>` — batch embedding

### Degradable batch embedding

`embed_batch` is all-or-nothing: one refused input fails the batch. A provider that can refuse a *single* input (an oversized or malformed item answered with `400`/`413`) instead implements the optional seam

- `async fn embed_batch_degradable(&self, texts: &[String]) -> Result<BatchEmbedOutcome>` — a batch embed that may degrade item by item

`BatchEmbedOutcome` carries one entry per input, in input order: `embeddings` holds `Some(vector)` for every item that landed and `None` for every item the provider refused, and `warnings` holds the reasons. The default implementation of the seam delegates to `embed_batch` and never degrades, so an existing embedder keeps working unchanged; a provider that overrides it reports the refusal instead of costing the whole generation. A caller must not store a placeholder for a refused item: a zero vector would poison every cosine distance it is later compared against.

The remote provider additionally bounds each request (at most 64 inputs and at most 32 KiB of text) and halves a request the provider refuses down to the offending items, so one bad input costs one warning rather than the batch.

The `VectorStore` trait is the pluggable interface for vector persistence and similarity search. Implementations must provide:

- `fn insert(&mut self, id: &str, vector: &[f32]) -> Result<()>` — upsert a vector
- `fn search(&self, query: &[f32], limit: usize) -> Result<Vec<(String, f32)>>` — nearest-neighbor search
- `fn delete(&mut self, id: &str) -> Result<()>` — remove a vector

Both traits are generic over storage backend. The default implementation uses a local in-memory store; custom implementations can target GPU-accelerated indexes or distributed stores.

## Custom `JudgmentProvider` Adapters

The `JudgmentProvider` trait is the pluggable interface for domain-specific judgment logic. Implementations must provide:

- `fn judge(&self, context: &JudgmentContext) -> JudgmentResult` — return a typed judgment

Judgment providers are used in retrieval ranking, entity resolution, and action validation. They can be stacked in a pipeline where multiple providers contribute to the final decision.

## Domain Ontologies & Kinetic Action Packs

A **kinetic action pack** is a named collection of `ActionTypeDefinition` JSON schemas that can be registered with the `neurostrata_action` tool. A pack defines:

- `slug` — unique action type identifier
- `required_role` — minimum ReBAC role required to apply the action
- `parameters` — typed parameter schema with finite bounds
- `submission_criteria` — preconditions for submission
- `effects` — the staged writes (`SupersedeMemory`, `UpsertMemory`, `SetTwinState`, `LinkEdge`, `TransitionTask`)

Action packs are domain-scoped: a pack for a physical-asset domain can coexist with a pack for a code-review domain in the same namespace without conflict. The `neurostrata_action define` command validates an `ActionTypeDefinition` and stores it as a `memory_type = "action_type"` row, refreshing the namespace's `OntologyIndex`.

## Kernel Data Ports: `TaskStore` & `OntologyStore`

The central agentic kernel consumes the serving layer through two narrow async ports (`Send + Sync` public traits in `src/traits.rs`, spec §3.1), so the kernel can be wired to any backend that implements them:

- `TaskStore` — read-only task surface for the kernel's executive port. `load_task_row(namespace, task_id) -> Result<Option<MemoryPayload>>`; `list_task_rows(namespace) -> Result<Vec<TaskSummaryRow>>`; `count_extracted_memories_for_task(namespace, task_id) -> Result<usize>` (the Lock 2 extraction count over `metadata.extracted_from`, excluding the task row itself).
- `OntologyStore` — typed memory listing and scoped writes for the kernel's ontology port. `list_memories_by_types(namespace, memory_types) -> Result<Vec<(String, MemoryPayload)>>` and `get_memory_by_id(namespace, id) -> Result<Option<MemoryPayload>>` for reads, `put_scoped_memory(namespace, id, payload) -> Result<()>` for scoped writes.

Both ports are implemented by `LadybugStore` (the default backend); the kernel holds them as `Arc<dyn ...>` trait objects rather than reaching into the concrete store, and custom storage backends in the kernel's `VectorStore` path implement the same contracts.
