# NeuroStrata README Restructure — Implementation Plan

> For agentic workers: use subagent-driven-development or native; this is documentation-only (no code behavior change), low risk for mistake, native is appropriate.

**Goal:** Rewrite README to marketing-pitch first with dense science moved to sub-pages; create FEATURES.md inventory; preserve cognitive branding.

**Architecture:** Per spec `docs/superpowers/specs/2026-10-23-readme-restructure-design.md`. Sub-page split (architecture/organs/mesh/science/ops/releases); FEATURES.md table per organ; README ≤150 lines (hero + hard-specs + 5-organs TL;DR + quick-start + links).

**Tech Stack:** Markdown / git only.

**Spec:** `docs/superpowers/specs/2026-10-23-readme-restructure-design.md`

**Global Constraints:** Preserve all cognitive-science branding names in sub-page headers (`<Term> — <Subtitle>`); keep biological naming in FEATURES.md col 1; hero must open with problem (not solution-only); hard-specs strip required; no bulk deletions of old docs (add sub-pages, trim README, archive old chunks in new paths — original changelog content preserved in `docs/releases/changelog/`). Feature inventory verified against README lines 1–540.

---

### Task 1: Write FEATURES.md inventory
Files: create `docs/FEATURES.md`.
Produces: feature table with (Organ / Engineering Primitive / Why you care ≤15w / Evidence line).

- [ ] Step 1: Draft table for memory core (SynapticGraph, Engram, Sync Pruning, negative exemplars) using spec §2.
- [ ] Step 2: Draft guard, goals, bridge, mesh, buffer, security, obs sections.
- [ ] Step 3: Verify evidence lines point to real README lines.
- [ ] Step 4: Commit.

### Task 2: Sub-page stub directories + header files
Files: create dirs + stub `.md` with title `Organ — Subtitle` + 2-line "Why you care".
Produces: sub-page skeletons so README links work.

- [ ] Create `docs/architecture/`, `docs/organs/`, `docs/mesh/`, `docs/science/`, `docs/ops/`, `docs/releases/changelog/`.
- [ ] Write helmet stubs (synapticgraph.md, tri-strata.md, prefrontal-cortex.md, central-executive.md, dendritic-bridge.md, thalamic-bus.md, procedural-memory.md, synaptic-mesh.md, why-memory-fails.md, nomenclature.md, security.md, obsidian.md, cli-reference.md).
- [ ] Commit.

### Task 3: Rewrite README.md (≤150 lines)
Files: modify `README.md`; remove moved sections; insert hero + hard-specs + TL;DR + quick-start + link farm.
Produces: new README.

- [ ] Write hero (3 sentences, problem-first) + hard-specs strip.
- [ ] Compress "What's New 1.11" to 1 paragraph callout with link.
- [ ] Move science (line 100–119) out reference to sub-page; trim mermaid caption.
- [ ] Add link farm to sub-pages.
- [ ] Verify length ≤150 lines; commit.

### Task 4: Migrate changelog content to `docs/releases/changelog/`
Files: split current lines 25–57 into per-release files; README keeps only latest summary.
- [ ] Create `docs/releases/changelog/1.11.0.md`, `.9.0.md`, `.8.0.md`, etc.
- [ ] Replace README changelog block with callout + link.
- [ ] Commit.

### Task 5: Push + extract
- [ ] `git pull --rebase`; `git push`; verify `git status` up-to-date.
- [ ] `neurostrata_add_memory` (extract: restructuring decision — biological branding kept with engineering subtitles; separate FEATURES.md inventory; hero opens with problem; hard-specs guard prevents over-marketing).
- [ ] `neurostrata_task_complete` on restructuring task (if tracked).

---

## Review Focus
- Hero reads as hype, not problem-first → fail.
- Sub-page headers drop biological names → fail branding rule.
- FEATURES.md missing plain-English benefit column → fail.
- Old science content lost (not moved to sub-page) → data-loss fail.
- README length >150 lines after edit → fail target.
