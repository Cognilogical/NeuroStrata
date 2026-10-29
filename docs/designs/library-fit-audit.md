# Library Fit Audit: NeuroStrata vs. OxiRS and Rust Crates

Task: `neurostrata-fsle`

Date: 2026-10-29

Argon review: `REQUEST_CHANGES` received 2026-10-29; this revision addresses
every concrete finding in that review.

## 1. Purpose

Identify every NeuroStrata subsystem that is currently hand-written and map it to
an available Rust library, with emphasis on the OxiRS/Oxigraph stack. The rule
applied throughout: if a library already provides the capability, NeuroStrata
must adapt to that library rather than reimplement it.

This audit is a planning and ownership document. It does not change runtime
behavior. Implementation remains tracked by the feature tasks named in
Section 6 and by the approved spec tracker in
`docs/superpowers/spec-review-tracker.md`.

## 2. Locked library stack

Source of truth: memory `b487cc09-4ac7-49cb-899c-4fdeda6f5301` (fact),
memory `d8e992a2-30db-4c23-b087-db3cd31e20ac` (rule), and the approved
specs under `docs/superpowers/specs/`.

| Capability | Library | Version / notes |
|---|---|---|
| RDF terms, triples, quads, datasets | `oxrdf` | `0.3.x`, feature `rdf-12` |
| Turtle / TriG / N-Triples / N-Quads / RDF-star parse + serialize | `oxttl` | `0.2.x`, feature `rdf-12` |
| JSON-LD / vocabulary files | `serde_json` | approved specs lock deterministic `serde_json`; `oxjsonld` is not a dependency |
| Content hashing / canonical slugs | `sha2` | already present |
| Snapshot / mesh compression | `zstd` | `0.14.3` |
| SPARQL parse | `spargebra` | `0.4.x`, feature `sparql-12` |
| SPARQL optimization | `sparopt` | `0.3.x`, feature `sparql-12` |
| SPARQL evaluation | `spareval` | `0.2.x`, feature `sparql-12` |
| SPARQL result formats | `sparesults` | `0.3.x`, feature `sparql-12` |
| Graph algorithms backup | `petgraph` | `0.8.x`, optional |
| OWL 2 RL reasoner | `reasonable` | evaluation only, not a default dependency |
| Community detection | `leiden-rs` | optional feature `community`, `default-features = false`, feature `petgraph` |
| Embeddings | `fastembed` | `7.x` |
| ONNX runtime | `ort` | optional feature `gliner` / `learning-laya` |
| Tokenizers | `tokenizers` | optional feature `gliner` |
| Numeric arrays | `ndarray` | optional feature `gliner` / `kge` |
| GraphQL parsing | `graphql-parser` | `0.4.x` |
| Async HTTP | `reqwest` | already present |
| Signatures | `openssl` | unconditional crate; `vendored` only on non-Apple targets |
| Parser front-ends | `tree-sitter` | already present |
| RDF store | `lbug` | already present |
| Full-text search (optional) | `tantivy` | optional feature `tantivy`; approved spec `neurostrata-ljzz` |
| WASM browser edge engine | `wasm-bindgen` | workspace crate `crates/neurostrata-wasm`, approved spec `neurostrata-6hjs` |

## 3. Explicitly rejected libraries

| Library | Disposition | Reason |
|---|---|---|
| `oxirs-core` | rejected | pulls `ctor` pre-main rustls hooks and `scirs2` bloat |
| `scirs2` / `scirs2-*` | rejected | heavy ML chain and rustls hooks |
| `horned-owl` | rejected | LGPL and tableau reasoner scope |
| `oxiarc-zstd`, `ruzstd` | rejected | duplicate zstd implementation |
| `oxirs-arq` | rejected | tied to `oxirs-core` ASTs |
| `oxirs-federate` | rejected | hard-depends on `oxirs-core`, `oxirs-arq`, `oxirs-vec`, `scirs2-core` |
| `oxirs-star` | rejected for direct use | heavier than `oxrdf` + `oxttl` with `rdf-12` |
| `oxirs-shacl` | rejected for direct use | direct dependency hygiene; SHACL evaluation is in-house over `oxrdf` + `oxttl` |
| `oxirs-embed` | rejected | carries the `scirs2` chain; KGE stays in-house on `ndarray` + `rand` |
| `async-graphql` | rejected | curated read API uses `graphql-parser` only |
| `base64`, `bs58` | in-house by spec | DID signing implements these directly |
| `rudof`, `shacl-rust`, `shacl-rs` | rejected | no third-party SHACL crate |

Notes:

- `tantivy` is **not** rejected. It is an approved optional track behind
  `--features tantivy` (`neurostrata-ljzz`). The default lexical engine remains
  in-house and `std`-only for WASM reuse.
- `reasonable` and `sophia_reasoner` are evaluation-only and are not dependencies
  unless a benchmark proves determinism and restricted-profile compliance.

## 4. Module ownership and library fit

### 4.1 RDF and semantic-layer modules

| Module | Current implementation | Recommended library | Notes |
|---|---|---|---|
| `src/rdf/canonical.rs` (entity-slug part) | `canonicalize_topology`, `canonicalize_known_ids_alias` | `sha2` + `oxrdf` IRI normalization | Keep this entity-slug canonicalization here |
| `src/rdf/dataset_canonical.rs` (new) | hand-written `memory_to_nquads` and escaping | `oxrdf` + `oxttl` + `sha2` | Migration spec: move hand-written N-Quads here; do not keep it in `canonical.rs` |
| `src/rdf/projection.rs` | hand-written `Quad` / `QuadObject` | `oxrdf` dataset and quad types | Replace in-house quad model |
| `src/rdf/shacl.rs` | hand-written SHACL Core checks | `oxttl` + `oxrdf`, evaluation in-house | Spec rejects third-party SHACL crates |
| `src/rdf/ontology.rs` | in-house domain/alias DAG and closure | `oxrdf` terms + in-tree BFS; `petgraph` optional | `reasonable` / `sophia_reasoner` evaluation only |
| `src/rdf/types.rs` | hand-written projection types | `oxrdf` adapter | LadybugDB remains primary store |
| `src/rdf/namespace.rs` | hand-written IRI helpers | `oxrdf` IRI helpers | Update per canonical RDF and RDF-star specs |
| `src/rdf/provenance.rs` (new) | not yet present | `oxrdf` + `oxttl` with `rdf-12` | RDF-star provenance module expected by `neurostrata-as9v` |
| `src/rdf/federation.rs` (new) | not yet present | `spargebra` + `spareval` + `sparesults` + `reqwest` | Do not confuse with `src/synaptic_mesh/federation.rs` |

### 4.2 Other core modules

| Module | Current implementation | Recommended library | Notes |
|---|---|---|---|
| `src/domain.rs` | in-house slug and alias logic | `sha2` + `oxrdf` IRI normalization | Audit follow-ups `neurostrata-97a9` (ns-97a9), `neurostrata-hs23` (ns-hs23) |
| `src/graph_read/related.rs` | in-house bounded traversal | in-tree BFS + `QueryBudget`; `petgraph` backup | Audit follow-up `neurostrata-l6ml` (ns-l6ml) |
| `src/ontology_grounding.rs` | in-house JSON vocabulary handling | `serde_json`; optional `oxrdf` | Implementation `neurostrata-3pqo` done; audit `neurostrata-tv6u` |
| `src/mention.rs` | deterministic in-house mention detection | standard library only | Open implementation tasks `neurostrata-n1k9`, `neurostrata-or1h` |
| `src/embed.rs` | local embedding provider abstraction | `fastembed` | Already integrated |
| `src/judgment/laya_onnx.rs` | retired ONNX provider, no fake confidence | `ort` + `tokenizers` + `ndarray` | Feature-gated; tracked by `neurostrata-fygv`, `neurostrata-8idy` |
| `src/learning/*` | Laya self-improvement, calibration, splits | standard library + `serde_json` | This is not KGE; KGE is a separate `src/kge.rs` |
| `src/kge.rs` (new) | not yet present | `ndarray` + `rand` | KGE spec requires in-house TransE; `oxirs-embed` rejected |
| `src/synaptic_mesh/federation.rs` | 17-line `/list` HTTP helper | `reqwest` | Not the SPARQL federation implementation |
| `src/parser/ingest.rs` | tree-sitter AST extraction | `tree-sitter` | Already integrated |
| `src/store/ladybug.rs` | LadybugDB graph store | `lbug` | Primary store; do not replace |
| `src/fts/mod.rs` | in-house BM25-style lexical engine | standard library only | Intentionally `std`-only for WASM reuse |
| `src/context/*` | in-house context DTOs and service | standard library only | No external library specified |
| `src/events/*` | in-house event bus | standard library + `tokio` | No external library specified |
| `src/guard/*` | in-house guard validator | standard library only | No external library specified |
| `src/task/*` | in-house task state machine | standard library only | No external library specified |
| `src/registry.rs` | in-house registry and watermark logic | standard library only | Uses `sha2` where hashing is needed |
| `src/server.rs` | in-house MCP server orchestration | `axum`, `tokio`, `serde_json` | Already integrated |
| `src/crash.rs` | signal and file syscalls | `libc` | Already integrated |
| `src/config.rs` | configuration loading | `toml` | Already integrated |
| `src/tools/*` | in-house MCP tool handlers | standard library only | No external library specified |

### 4.3 Additional live modules

| Module | Recommended library | Notes |
|---|---|---|
| `src/search.rs` | standard library + store traits | Search orchestration; no external library specified |
| `src/daemon.rs` | `tokio` + `lbug` | Daemon lifecycle and store ownership |
| `src/handlers/*` | `serde_json` + store traits | MCP handler implementations |
| `src/receipts/*` | `serde_json` + store traits | Receipt persistence |
| `src/retention/gate.rs` | standard library only | Retention policy gate |
| `src/diagnostic/classify.rs` | standard library only | Diagnostic classification |
| `src/model_downloader.rs` | `reqwest` + `sha2` | Supply-chain model downloader |
| `src/secrets.rs` | `regex` | Secret scanning |
| `src/source_context.rs` | standard library only | Source anchor handling |
| `src/recall_metrics.rs` | standard library + `tracing` | Recall metrics |
| `src/attempt.rs` | standard library only | Attempt tracking |
| `src/buffer.rs` | standard library + filesystem | Rolling session buffer |
| `src/ingest_jobs.rs` | standard library + `serde_json` | Ingest job state |
| `src/setup.rs` | standard library + repository inspection | Setup detection |
| `src/traits.rs` | standard library only | Shared traits |

## 5. Cargo.toml dependency plan

### 5.1 Add to core dependencies

```toml
oxrdf = { version = "0.3", features = ["rdf-12"] }
oxttl = { version = "0.2", features = ["rdf-12"] }
zstd = "0.14"
spargebra = { version = "0.4", features = ["sparql-12"] }
sparopt = { version = "0.3", features = ["sparql-12"] }
spareval = { version = "0.2", features = ["sparql-12"] }
sparesults = { version = "0.3", features = ["sparql-12"] }
graphql-parser = "0.4"
```

### 5.2 Add as optional dependencies

```toml
petgraph = { version = "0.8", optional = true }
leiden-rs = { version = "0.8", default-features = false, features = ["petgraph"], optional = true }
rand = { version = "0.10", optional = true }
tantivy = { version = "0.27", optional = true }
```

`reasonable` remains evaluation-only and is not added to `Cargo.toml` unless a
benchmark proves determinism and restricted-profile compliance.

### 5.3 Feature flags

```toml
[features]
default = []
learning-laya = []
gliner = ["dep:ort", "dep:tokenizers", "dep:ndarray"]
community = ["dep:leiden-rs", "dep:petgraph"]
kge = ["dep:ndarray", "dep:rand"]
owl-petgraph = ["dep:petgraph"]
tantivy = ["dep:tantivy"]
digital-twin = []
```

### 5.4 Workspace and target-specific dependency corrections

- Add `crates/neurostrata-wasm` as a workspace member. That crate uses
  `wasm-bindgen = "0.2"` and must not depend on LadybugDB, `fastembed`, `ort`,
  `openssl`, `cxx`, `tree-sitter`, `tokio`, `reqwest`, `axum`, `uuid`, or
  `chrono`.
- Make `openssl` available on all targets. Keep the crate unconditional and
  retain `vendored` only for non-Apple targets:
  ```toml
  [dependencies]
  openssl = "0.10"

  [target.'cfg(not(target_vendor = "apple"))'.dependencies]
  openssl = { version = "0.10", features = ["vendored"] }
  ```
- Keep `fastembed = "7"` as the current integrated version; older `0.3`
  references in historical specs are stale.
- Keep `ort`, `tokenizers`, and `ndarray` optional and feature-gated.

## 6. Tracked implementation tasks

The following open tasks are the implementation vehicles for this audit. Closed
tasks are not listed as open vehicles; where a feature has a completed
implementation task, the open implementation task is named instead.

| Library area | Open implementation tasks | Related audit / dependency tasks |
|---|---|---|
| RDF canonical model | `neurostrata-770f`, `neurostrata-ykwg` | `neurostrata-4gb2` is done; RDF-star follow-up is `neurostrata-as9v` (ns-as9v) |
| OWL / RDFS reasoning | `neurostrata-duzn`, `neurostrata-2xe9` |  |
| SPARQL | `neurostrata-gq46`, `neurostrata-0j1s`, `neurostrata-qb37` |  |
| SHACL | `neurostrata-sgj4`, `neurostrata-097r` |  |
| Federation | `neurostrata-ahix` | `neurostrata-k8tc` is done |
| Community detection | `neurostrata-knq3` |  |
| KGE | `neurostrata-9c4w`, `neurostrata-qd0t` |  |
| GLiNER | `neurostrata-fygv`, `neurostrata-clh3` | audit tasks `neurostrata-g7xv`, `neurostrata-kks3` |
| GraphQL | `neurostrata-ect5` |  |
| Streaming delta | `neurostrata-7rgp` |  |
| Compression / anomaly | `neurostrata-g6mw` |  |
| DID signing | `neurostrata-39v2` | `neurostrata-epzj` is done |
| Canonical slugs | `neurostrata-k0yq` | implementation largely landed in `neurostrata-fui0`; audit task `neurostrata-97a9` |
| Domain DAG / aliases |  | implementation landed in `neurostrata-fui0`; audit task `neurostrata-hs23` |
| Ontology grounding |  | implementation `neurostrata-3pqo` done; audit task `neurostrata-tv6u` |
| Entity / mention linking | `neurostrata-n1k9`, `neurostrata-or1h` |  |
| Full-text Tantivy (optional) | `neurostrata-ljzz` |  |
| WASM browser edge engine | `neurostrata-6hjs` |  |
| Digital twin | `neurostrata-jk91` |  |
| ReBAC | `neurostrata-azjv` |  |
| GraphRAG hybrid search | `neurostrata-6x0q` |  |
| Time-series metrics | `neurostrata-ejiy` |  |
| Multimodal validation | `neurostrata-y062` |  |
| RAG chat client | `neurostrata-miuu` |  |
| Storage hygiene | `neurostrata-rimc` |  |

## 7. Conclusion

NeuroStrata should adopt `oxrdf`, `oxttl`, and the Oxigraph SPARQL stack
(`spargebra`, `sparopt`, `spareval`, `sparesults`) as the canonical RDF and
query layer. It should keep LadybugDB as the primary store, `fastembed` for
embeddings, and in-house deterministic code where the specs explicitly reject
external libraries (SHACL evaluation, base64/bs58, exact-name search, and the
`std`-only lexical engine).

The highest-value next implementation work is the RDF canonical migration
(`neurostrata-770f` / `neurostrata-ykwg`), followed by SPARQL
(`neurostrata-gq46` / `neurostrata-0j1s`) and OWL reasoning
(`neurostrata-duzn`). Those tasks should replace hand-written RDF and reasoning
code with the libraries named above rather than extending the current
approximations.
