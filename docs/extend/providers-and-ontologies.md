# Custom Providers, Ontologies & Action Packs

## Custom `Embedder` & `VectorStore` Implementations

The `Embedder` trait is the pluggable interface for embedding generation. Implementations must provide:

- `fn embed_text(&self, text: &str) -> Vec<f32>` — embed a single string
- `fn embed_batch(&self, texts: &[&str]) -> Vec<Vec<f32>>` — batch embedding

### Degradable batch embedding

`embed_batch` is all-or-nothing: one refused input fails the batch. A provider that can refuse a _single_ input (an oversized or malformed item answered with `400`/`413`) instead implements the optional seam

- `async fn embed_batch_degradable(&self, texts: &[String]) -> Result<BatchEmbedOutcome>` — a batch embed that may degrade item by item

`BatchEmbedOutcome` carries one entry per input, in input order: `embeddings` holds `Some(vector)` for every item that landed and `None` for every item the provider refused, and `warnings` holds the reasons. The default implementation of the seam delegates to `embed_batch` and never degrades, so an existing embedder keeps working unchanged; a provider that overrides it reports the refusal instead of costing the whole generation. A caller must not store a placeholder for a refused item: a zero vector would poison every cosine distance it is later compared against.

The remote provider additionally bounds each request (at most 64 inputs and at most 32 KiB of text) and halves a request the provider refuses down to the offending items, so one bad input costs one warning rather than the batch.

The `VectorStore` trait is the pluggable interface for vector persistence and similarity search. Implementations must provide:

- `fn insert(&mut self, id: &str, vector: &[f32]) -> Result<()>` — upsert a vector
- `fn search(&self, query: &[f32], limit: usize) -> Result<Vec<(String, f32)>>` — nearest-neighbor search
- `fn delete(&mut self, id: &str) -> Result<()>` — remove a vector

`get_many(namespaces, ids)` returns the payloads of the rows with those ids in those namespaces, without their vectors, and leaves out ids with no row. Its default reads them one by one with `get`; a backend that can answer in one query should override it, as `LadybugStore` does. `get_snapshot` uses it to leave retired rules out of the session file's task block.

Stores that project edges into a graph additionally implement the optional seam

- `async fn reconcile_declared_edges(&self, namespace: &str, id: &str, rel_type: &str, keep: &[String]) -> Result<usize>` — retire the edges this store materialised from a previous declaration of `rel_type` whose target is not in `keep`

The default implementation is a no-op, so a vector-only backend is unaffected. `LadybugStore` implements it as a delete over edges stamped `extractor = 'upsert'`; mention-detector and hand-authored edges are never candidates, and an empty `keep` retires every declared edge. The vault importer calls it before each head's upsert so a removed WikiLink cannot survive as a stale `RELATES_TO` relationship; ordinary upserts that do not declare a link set are never reconciled implicitly.

A second optional seam rebinds edges without rewriting the head:

- `async fn rebind_declared_edges(&self, namespace: &str, id: &str, rel_type: &str, declared: &[String], keep: &[String]) -> Result<usize>` — materialise any missing outgoing `rel_type` edges to `keep`, then reconcile the rest; `declared` is the importing note's declaration list, positionally aligned with `keep`

The default is again a no-op. `LadybugStore` creates edges under the same `extractor = 'upsert'` provenance as an upsert would, stamps each replacement edge's `metadata.edge_sources` entry through the same builder `upsert` uses (declaration index, declared and resolved targets, evidence id), and then runs `reconcile_declared_edges`. Content, identity metadata and history stay untouched; only the citation lists change. The vault importer uses it only for an unchanged declarer whose preferred exact target arrived in that import, so a byte-identical note can follow a newly named target without minting a content version and without losing the live edge's reference.

### Reserved ingest namespaces

A namespace whose name begins with `crate::traits::INTERNAL_INGEST_STAGE_PREFIX` (`__neurostrata_ingest_stage__`) belongs to the internal staging machinery. `is_reserved_ingest_destination(namespace)` reports whether a destination is reserved, and directory ingest refuses one before it normalizes a node set, probes the directory (including a `dry_run` preview), embeds anything or opens a staging generation. The ingest-jobs layer, SCIP ingest and bounded classification apply the same predicate before they canonicalize a path, read the index or validate a batch, and the CLI applies it before it can clear stored rows. A staging namespace is therefore never a valid project destination, and refusing it early costs no work.

Three further optional seams govern staged publications. `async fn publish_generation_cas(&self, namespace, source_key, expected, target, pending) -> Result<Option<u64>>` must compare the active pointer with `expected` and flip it in one write turn, returning the generation actually displaced (`None` when a newer publication landed first, so neither a stale publisher nor a stale roll-back replaces the newer tree) and applying `pending` as the relink marker on the resulting pointer. A default read-then-publish is deliberately absent because it can overwrite a concurrent publish; a backend without compare-and-swap must override this seam. `async fn relink_committed_for(&self, namespace, source_key, generation) -> Result<bool>` reports whether that generation's relink committed, so a caller that only saw a write timeout can tell a completed relink from a missing one; the default is `false`. `async fn recover_pending_relink(&self, namespace, source_key) -> Result<usize>` finishes a relink left pending by a crash after publication but before edge materialisation; the default is a no-op, and `LadybugStore` replays the declarations against the active generation. These defaults are no-ops, refusals or best-effort, so a vector-only backend keeps working unchanged without gaining a check-then-publish race.

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
