# Custom Providers, Ontologies & Action Packs

## Custom `Embedder` & `VectorStore` Implementations

The `Embedder` trait is the pluggable interface for embedding generation. Implementations must provide:

- `fn embed_text(&self, text: &str) -> Vec<f32>` — embed a single string
- `fn embed_batch(&self, texts: &[&str]) -> Vec<Vec<f32>>` — batch embedding

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

The central agentic kernel consumes the serving layer through two narrow async ports, so the kernel can be wired to any backend that implements them:

- `TaskStore` (`src/traits.rs`) — task-row access for the kernel's executive port: `load_task_row(namespace, task_id)`, `list_task_rows(namespace)`, and `count_extracted_memories_for_task(namespace, task_id)` (the Lock 2 extraction count over `metadata.extracted_from`, excluding the task row itself).
- `OntologyStore` (`src/traits.rs`) — typed memory listing and scoped writes for the kernel's ontology port: `list_memories_by_types(namespace, memory_types)` and `get_memory_by_id(namespace, id)` for reads, `put_scoped_memory(namespace, id, payload)` for scoped writes.

Both ports are implemented by `LadybugStore`; the kernel holds them as `Arc<dyn ...>` trait objects rather than reaching into the concrete store.
