# Plan 3: Incremental & Doc Ingestion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make ingestion incremental (F2), parse markdown into embedded doc sections (F3), and add a dry-run preview mode (F8).

**Architecture:** Add a `DocSection` ingest item type and markdown section parser in run_producer; consumer upserts sections as `memory_type: "doc_section"` with CONTAINS edges. Add `dry_run`/`dry_run_mode` params to ingest_directory and handle_ingest_directory; estimate mode walks + counts without writes. Add file-hash-based skip for incremental ingest: pre-compute previous generation's file hashes and skip unchanged files (report counts).

**Tech Stack:** Rust (cargo test), pulldown-cmark (or manual heading split).

**Spec:** docs/superpowers/specs/2026-10-27-neurostrata-feature-specs-design.md (F2, F3, F8)

## Global Constraints
- JSON only; no LLM.
- Dry-run performs zero writes and zero downloads.
- Markdown sections use memory_type "doc_section" (non-structural).

---

### Task 1: F3 markdown section parsing

**Files:** Modify src/parser/ingest.rs.

- [ ] Add `IngestItem::DocSections { node_path, sections: Vec<DocSection>, file_sha256 }`.
- [ ] Parse markdown by heading; extract backticked symbols, file paths, links.
- [ ] Consumer embeds each section text and upserts as memory_type "doc_section" with ID `path#section:slug@sha256`, contained_by = [file node id].
- [ ] Tests: markdown file produces doc_section nodes; search finds backticked symbol.

### Task 2: F8 dry-run ingest

**Files:** Modify src/parser/ingest.rs, src/ingest_jobs.rs (optional), src/server.rs (handle_ingest_directory + schema).

- [ ] Add `dry_run: bool` and `dry_run_mode: String` to ingest_directory signature (or a separate dry_run_directory function).
- [ ] Estimate mode: walk tree, count files/symbols/embeddings, secret hits; no writes.
- [ ] Presort mode: junk detection, duplicate files by hash, PII pattern warnings, proposed groups.
- [ ] Add params to handle_ingest_directory and tool schema.
- [ ] Tests: dry-run writes nothing.

### Task 3: F2 incremental ingest

**Files:** Modify src/parser/ingest.rs.

- [ ] Pre-compute previous generation file hashes.
- [ ] Skip files whose hash matches; report counts (skipped/changed/added/removed/embeddings reused).
- [ ] Carry forward unchanged nodes into new generation.
- [ ] Tests: re-ingest unchanged tree produces zero new embeddings.

## Execution Handoff
Plan complete. Execute natively? (recommended)
"", "path">
/home/kenton/Documents/NeuroStrata/docs/superpowers/plans/2026-10-27-plan3-ingest-docs.md