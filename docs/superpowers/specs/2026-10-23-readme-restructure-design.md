# NeuroStrata README Restructure — Design Spec

Date: 2026-10-23. Path: Architectural. Agents consulted: k3-arch (inventory, sub-page split), gemini-arch (hero/opening, competitive claim, naming, over-marketing guard).

## 1. Intent (what success looks like)
README sells the project in ≤3 sentences. Dense science / feature detail moves to sub-pages. Cognitive branding preserved; translated to plain-English benefits in FEATURES.md and per-sub-page subtitles.

## 2. Feature Inventory (engineering, not marketing)
Source: K3-arch output, verified against README lines.

Memory core: SynapticGraph, LadybugDB embedded (no external DB), Dual-Track Bi-Temporal Graph (vector distance + bounded graph expansion, no BM25/RRF — see line 17 correction), Engram (vector row with `{file,lines}` pointer), Eidetic Recall (boot-time top-5 snapshot), Synaptic Pruning (access-count only, never auto-delete), negative exemplars (`metadata.attempt`, 1.11).
Tri-Strata: Global / Domain / Task namespaces (line 122); domain registry (1.11) refuses unknown slug with remedy; snapshot names registered domains + bounded strap.
Prefrontal Cortex: semantic rule match vs `guard_rule` namespace, deterministic + sandbox verdict vocabulary, Podman dry-run (5s, `--network=none`, read-only, fails closed), hostile-path canonicalization.
Central Executive: state machine 4-state / 8-transition, Zero-Action Start, exclusive claim (fails loudly), done-funnel (Lock 2 — `task_complete` refuses until `EXTRACTED_FROM` edge), SAS pre-push gate (`task gate --strict`), wiring panel (auto/reminder + verified subtraction), retired-row semantics (1.10, supersession chain bounded + cycle-safe, task_list hides history by default).
Dendritic Bridge: `embedders.json` array strict JSON, declared bridge only when both `base_url` + `api_key_env` present, graceful local `fastembed` (NomicEmbedTextV15 / BGEBaseENV15, 768d), explicit refusal on missing key (no silent width-swap).
Thalamic Bus: in-proc `Arc<ThalamicBus>`, `ThalamicPulse` {Created,Superseded,Archived,GuardedActionFired}, 3 subscribers (EpisodicPointerEcho, ExportFreshnessDirty, GuardEventLog), drop-oldest, 250ms timeout, `RecursionToken`; export-freshness gate (`check_export_freshness` refuses export on `dirty:true`).
Procedural Memory: `memory_type:"procedure"`, trigger, `remaining_fires`, `valid_to`, rehearsal counters, `procedure_perform` stamps firing + episodic pointer, snapshot `procedures_due`.
SynapticMesh (1.9): durable WAL outbox → wire-format (`synaptic-mesh-record/v1`); git transport; validated pull (schema + namespace + id/filename + lifecycle merge table, conflicts preserved loudly); private rows sovereign; procedure definitions travel without counters; permanence disclosure.
Episodic Buffer: `append_log` with `### 🔄 Topic Switch` markers, 500KB rollover, retention config, 2-pass grep+read recovery.
Source-Anchor Registry (1.10): `source_id` → `.NeuroStrata` marker, multi-checkout/worktree support, VCS-agnostic provenance (provider-neutral `locations[].source_context`).
Security: loopback-only daemon (`127.0.0.1:34343`, stdio JSON-RPC), secret scrubbing (high-entropy rejection / redaction loop), Cypher injection escaping, no destructive MCP tools (delete/edit/move CLI/GUI only), OWASP Top-10 for LLM Apps.
Observability / UX: Obsidian plugin (direct DB + spatial canvas, right-click prune), crash breadcrumbs (`crash-<pid>-<ts>.txt` + backtrace), CLI subcommands.

## 3. Competitive Differentiation (3 claims with evidence links)
Claim 1 — Only system coupling task lifecycle to memory consolidation at binary/git level (done-funnel + SAS gate). Evidence: README §Central Executive (line 246), `neurostrata_task_complete` single-funnel, Lock 2 (line 89). Mem0/Zep/Letta have no guard/organs; they are passive stores.
Claim 2 — Pre-execution behavioral guard as a memory namespace (rules = ordinary `guard_rule` Engrams; semantic match + optional Podman sandbox). Evidence: §Prefrontal Cortex (line 211), verdict vocabulary (224), hostile-path (236).
Claim 3 — Zero-config, single-binary, embedded-vendor (LadybugDB + in-proc fastembed), loopback-only, secret-scrubbing, and git-native team mesh (SynapticMesh — no broker, validated merge, conflicts preserved). Evidence: §Zero-Overhead (61), §Security (515), §SynapticMesh (335).

## 4. Plain-English Benefit Translation (FEATURES.md plan)
Separate `docs/FEATURES.md`: one table per organ with (Engineering primitive / What it is / What it means for you). Keeps cognitive names in column 1, plain-English payoff in column 3. Sub-pages (docs/organs/*.md) open with same pattern: `<Biological Term> — <Engineering Subtitle>` header, then 2-line "Why you care" before science.

## 5. Sub-Page Mapping (K3-arch)
docs/architecture/synapticgraph.md — Dual-Track, Engram, retrieval, correction (line 17 erratum reference).
docs/architecture/tri-strata.md — namespaces, registry, spatial anchoring / Pointer-Wiki.
docs/organs/prefrontal-cortex.md — guard, verdicts, sandbox, teaching.
docs/organs/central-executive.md — lifecycle, wiring, SAS gate, retirement.
docs/organs/dendritic-bridge.md — bridge declaration, resolution, minimal-entropy law.
docs/organs/thalamic-bus.md — pulses, subscribers, backpressure, freshness.
docs/organs/procedural-memory.md — procedure type, rehearsal, snapshot strap.
docs/mesh/synaptic-mesh.md — git mesh, quickstart, permanence disclosure.
docs/science/why-memory-fails.md — 4 failures + NeuroStrata fixes (lines 101–119).
docs/science/nomenclature.md — bio↔engineering table (71–97), trimmed for web.
docs/ops/security.md — OWASP, loopback, secret scrubbing.
docs/ops/obsidian.md — plugin setup.
docs/ops/upgrading.md + crashes.md — lines 483–513.
docs/releases/changelog/ — per-release (current lines 25–57 split out; README keeps only "Latest (1.11.0)" + link).

## 6. README Restructuring Plan
Target length: ≤150 lines. Structure:

Line block 1 — Badge/header + banner (keep existing, trim). Line block 2 — Hero (3 sentences, per Gemini-arch): "Agents lose context and forget rules across sessions. NeuroStrata is one Rust binary that gives your AI a permanent, enforceable memory — with a conscience that blocks bad actions and a goal-system that refuses to finish empty-handed." This opens with problem, not hype.
Line block 3 — Hard-specs strip (mitigate over-marketing): embedded LadybugDB · stdio-only MCP · loopback daemon · zero cloud / zero Docker · single binary · secret-scrubbed · Cypher-escaped. One line per.
Line block 4 — "The five organs" TL;DR diagram reference (keep mermaid from 126, but shorter caption). Each organ = 1 sentence + link.
Line block 5 — Quick-start (install / build / hooks install — keep current 355–409 but compressed).
Line block 6 — "See the deep docs" link farm to sub-pages.
Line block 7 — References + release pointer (trimmed from 529–539).

What moves out: full science (§100) → docs/science/; full nomenclature (§71) → docs/science/nomenclature.md; full organs (§211–329, 335) → docs/organs/; full changelog (§25–57) → docs/releases/; full CLI ref (§433–474) → docs/ops/cli-reference.md; full security (§515) → docs/ops/security.md.

Preserved in README: the 1.11.0 headline paragraph (line 27) moved to a brief "Latest" callout (1 paragraph max); the three "What's New" blocks become a single "What's new in 1.11.0" line with link to changelog.

## 7. Naming Convention (per Gemini-arch)
Sub-page headers: `<Biological Term> — <Plain-English Subtitle>` (e.g., "Prefrontal Cortex — Podman-Sandboxed Behavioral Guard"). Keeps branding as section glyphs (🛡️ etc. optional), indexes by engineering intent.

## 8. FEATURES.md Spec (separate inventory)
File: docs/FEATURES.md. Structure: table with columns (Organ / Engineering Primitive / What it does for you / Evidence line). One section per organ (memory, guard, goals, bridge, mesh, buffer). Plain-English column must answer "why do I care?" in ≤15 words. Keep biological names in column 1 (branding) — do not rename to generic module names.

## 9. Spec Self-Review (done pre-user-review)
- Placeholder scan: no TBDs.
- Internal consistency: sub-page list matches inventory; competitive claims reference real lines; hero matches gemini-arch recommendation; over-marketing guard (hard-specs strip) present.
- Scope: focused on README + FEATURES.md + sub-page split; does not expand feature code.
- Ambiguity: "plain English" defined as 15-word benefit column; "organs" defined as the 5 modules from section 2.

## 10. User Review Gate (required before implementation)
This spec must be reviewed and approved before any `README.md` edit or `FEATURES.md` creation. Implementation will then follow: (a) write FEATURES.md, (b) create sub-page dirs + stub files, (c) rewrite README.md, (d) commit, (e) push.
