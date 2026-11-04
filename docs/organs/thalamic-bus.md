# Thalamic Bus — In-Process Typed Event Bus & Freshness Gate

> **Why you care:** Every memory write, task transition, and AST ingestion triggers background bookkeeping automatically — keeping indexes normalized, enrichment telemetry accurate, and preventing stale graph exports.

---

## 1. Event-Driven Cognitive Coordination (`src/events/`)

In the biological brain, the thalamus routes sensory and motor signals between cortical regions. In NeuroStrata, the **Thalamic Bus** is a bounded, non-blocking, in-process pub/sub event bus that decouples fast MCP write handlers from background maintenance subscribers.

```text
MCP / CLI Mutation (add_memory, supersede, ingest, task_complete)
       │
       ▼
┌────────────────────────────────────────────────────────┐
│              Thalamic Bus (Typed Pulse)                │
└──────┬─────────────────────┬────────────────────┬──────┘
       │                     │                    │
       ▼                     ▼                    ▼
Post-Ingest Edge      Freshness Flag       Enrichment & Access
  Normalizer          (`dirty: true`)       Telemetry Tracker
```

---

## 2. Core Subscribers & Automated Bookkeeping

1. **Post-Ingest Edge Normalization (`src/events/subscribers/post_ingest_normalize.rs`):**
   When `neurostrata_ingest_directory` finishes indexing source files, the subscriber normalizes relative `governs` and `related_to` targets into canonical `<namespace>::<path>` graph edges and binds previously unanchored rules to newly discovered AST nodes.
2. **Export-Freshness Gate (`freshness_flag`):**
   Every mutation in a namespace stamps `metadata.dirty: true` on that namespace's `freshness_flag` row. When `neurostrata-mcp export-graph` runs, it checks the freshness state so operators and UI consumers always know whether an exported graph snapshot reflects the latest writes.
3. **Access & Salience Telemetry:**
   Retrieving memories via `search_memory`, `memories_for_file`, or `append_log` recall emits asynchronous access pulses that increment `access_count` without adding write latency to the read path.

---

## 3. Operator Telemetry (`bus-metrics`)

Operators can inspect the live health and throughput of the Thalamic Bus at any time via the CLI or HTTP `/bus/metrics` endpoint:

```bash
neurostrata-mcp bus-metrics
```

The response reports:
- Total pulses published per event type.
- Total pulses delivered to subscribers.
- Queue depth and dropped-pulse counters (a non-zero dropped count alerts operators if background subscribers fall behind burst write traffic).
