# Architectural Review Report — NeuroStrata README.md

**Subject:** NeuroStrata README.md
**Date:** 2026-04-20
**Mode:** Document Review
**Panel:** Context Master (Gemini 3 Pro) · The Architect (Claude Sonnet 4.6) · Security Sentinel (OpenAI o4) · Product Visionary (GPT-5.2) · Creative Strategist (GPT-5.3-Codex) · The Optimizer (GPT-5.3-Codex) · The Naysayer (Claude Sonnet 4.6)

---

## Final Recommendation: Request Changes

The panel unanimously agrees that the NeuroStrata architecture presents a highly innovative, conceptually sound, and differentiated approach to AI memory. The Tri-Strata model and Pointer-Wiki architecture perfectly address the "Lost in the Middle" context degradation problem. However, there is a critical, universally flagged contradiction regarding the networking protocol boundaries (REST vs. MQTT), an inaccurate architecture diagram, and a lack of documented trust boundaries for the UI connection that must be resolved before this documentation is published.

---

## Findings Summary

| Severity | Count |
|----------|-------|
| Critical | 1     |
| Major    | 3     |
| Minor    | 3     |
| Info     | 1     |

---

## Critical Issues (Must Address)

### C-01: Direct Protocol Contradiction ("No REST" vs. Local REST Server)
- **Severity:** Critical
- **Source:** All Panel Members (Unanimous)
- **Description:** Line 17 unambiguously claims "We completely stripped out HTTP/REST. NeuroStrata communicates purely via... MQTT over WebSockets for UI clients." However, Line 139 states the binary is "simultaneously spinning up an HTTP REST Server (localhost:8005)" for external UIs. These are mutually exclusive claims. This contradiction destroys trust in the documentation, creates integration ambiguity for plugin authors, and obscures the actual network attack surface.
- **Recommendation:** Make a definitive architectural decision and align the document. If HTTP/REST exists for UIs, remove the "No REST APIs" marketing claim and accurately describe the dual-protocol split (stdio for agents, HTTP/REST for UIs). If it is truly MQTT-over-WS, remove the REST Server reference entirely.

---

## Major Issues (Should Address)

### M-01: Undocumented UI Connection Trust Boundary
- **Severity:** Major
- **Source:** Security Sentinel, The Architect, Product Visionary, The Naysayer
- **Description:** The README describes external UIs (like Obsidian) connecting via WebSockets or REST to visualize and mutate memory, but provides zero documentation on the security model. There is no mention of authentication (e.g., session tokens), origin validation, or whether the server binds exclusively to `127.0.0.1`. In a system that allows autonomous deletion of shared global memory, an exposed unauthenticated local port is a severe A05 (misconfig) / A01 (access-control) risk.
- **Recommendation:** Add a concise "UI Connection & Trust Boundary" section explicitly defining the default bind address (must be localhost), any required authentication tokens, and CORS/Origin policies.

### M-02: Mermaid Diagram Misrepresents Data-Plane Ownership
- **Severity:** Major
- **Source:** The Architect, The Optimizer, The Naysayer
- **Description:** The architecture diagram (lines 68-109) depicts the Obsidian GUI connecting directly to LanceDB ("Queries Vector Data") and PointerWiki. This bypasses the Rust server entirely, contradicting the text which states UIs connect via the server's WebSockets/REST. This misrepresents the actual data flow and implies a dangerous security topology where clients have raw database access.
- **Recommendation:** Update the Mermaid diagram. Route the Obsidian connections through the `NeuroStrataServer` subgraph (or explicitly through the MQTT/REST router) with a labeled protocol edge, and remove the direct Obsidian-to-LanceDB arrow.

### M-03: Episodic Buffer Lacks Operator Safety & Retention Policies
- **Severity:** Major
- **Source:** Creative Strategist, The Optimizer, Product Visionary
- **Description:** While the 500KB rolling log mechanism is practical, there is no documented user control over it. Users do not know the maximum retention horizon, how to pause/opt-out of logging, or how to handle accidental inclusion of sensitive PII or secrets (A09 logging concerns).
- **Recommendation:** Add an "Episodic Buffer Policy" block detailing default retention limits, how users can disable or pause logging via configuration, and a warning to avoid pasting raw secrets into the conversational context.

---

## Minor Suggestions (Nice to Have)

### m-01: Autonomous Deletion Requires Human-in-the-Loop Clarity
- **Severity:** Minor
- **Source:** The Naysayer
- **Description:** The README states agents autonomously call `neurostrata_delete_memory` to prune hallucinations. Because the Global Stratum is shared, an LLM hallucinating that a valid rule is invalid could cause irreversible data loss across all projects.
- **Recommendation:** Clarify if deletions are "soft" (tombstoned via the bi-temporal audit trail) or hard. If hard, recommend a human confirmation step or `flag_for_review` workflow.

### m-02: Biological Nomenclature Obscures Onboarding
- **Severity:** Minor
- **Source:** Product Visionary, Creative Strategist
- **Description:** Terms like "Engrams", "Synaptic Pruning", and "Eidetic Recall" are highly memorable but lack immediate operational definitions.
- **Recommendation:** Add a brief "Biology ↔ Engineering Rosetta" glossary early in the document (e.g., Engram = vector row, Pruning = score decay, Eidetic Recall = boot-time snapshot).

### m-03: Lack of Quantifiable Performance Targets
- **Severity:** Minor
- **Source:** The Optimizer
- **Description:** The document makes strong claims about "zero-overhead" and token reduction but provides no baseline metrics.
- **Recommendation:** Include 2-3 measurable claims (e.g., "expected 60% reduction in context tokens vs full-file RAG") to satisfy engineering evaluation.

---

## Informational Notes

### I-01: Stale Technology References in Downstream Docs
- **Source:** The Architect
- **Description:** The `docs/UI_GUIDE.md` (referenced in the README) contains stale mentions of a "Go Server" and "Qdrant", contradicting the Rust/LanceDB architecture.
- **Recommendation:** Update the UI_GUIDE.md to align with the Rust/LanceDB reality.

---

## What Was Done Well

- The **Pointer-Wiki Architecture** is universally praised by the panel as a genuinely clever, elegant solution to context window bloat and the "Lost in the Middle" phenomenon.
- The **Tri-Strata Model** (Global/Domain/Task) provides an incredibly clean, intuitive mental model for separating cognitive loads and preventing semantic cross-contamination.
- Grounding the architecture in established cognitive science literature (Brooks, Tulving, O'Keefe & Nadel) elevates the document from marketing copy to principled engineering design.
- The **Dual-Track Bi-Temporal Graph Memory** ensures an immutable audit trail, which is a massive operational advantage over standard ephemeral RAG systems.
- The "local-first but cloud-ready" positioning strikes the perfect balance for enterprise adoption.

---

## Blind Voting Results (If Applicable)

None needed. The panel was unanimous on the critical protocol contradiction.

---

## Panel Breakdown

### The Architect (Claude Sonnet 4.6)
- **Recommendation:** Approve with Conditions
- **Summary:** The Tri-Strata model and Pointer-Wiki abstraction are superb. However, the REST/MQTT contradiction and the inaccurate Mermaid diagram must be fixed to provide a clear API contract.
- **Findings:** 1 Critical, 1 Major, 1 Minor

### Security Sentinel (OpenAI o4)
- **Recommendation:** Request Changes
- **Summary:** The architecture is strong, but the lack of a documented trust boundary for the UI WebSockets/REST server and the ambiguous protocol exposure present unacceptable A01/A05 integration risks.
- **Findings:** 0 Critical, 3 Major, 1 Minor

### Product Visionary (GPT-5.2)
- **Recommendation:** Request Changes
- **Summary:** The value proposition is massive, but the contradictory networking story and lack of a clear "Aha" onboarding path or glossary for the biological terms will slow user activation.
- **Findings:** 0 Critical, 4 Major, 2 Minor

### Creative Strategist (GPT-5.3-Codex)
- **Recommendation:** Request Changes
- **Summary:** The biological analogies and pointer-based context are brilliant differentiators. However, operator controls for the Episodic Buffer and the UI WebSocket contract need UX-grade clarity.
- **Findings:** 0 Critical, 3 Major, 2 Minor

### The Optimizer (GPT-5.3-Codex)
- **Recommendation:** Request Changes
- **Summary:** The cost/latency reduction strategy is highly sound, but the ambiguous protocol hot-paths and lack of explicit resource governance/retention policies make performance modeling impossible.
- **Findings:** 0 Critical, 3 Major, 2 Minor

### The Naysayer (Claude Sonnet 4.6)
- **Recommendation:** Request Changes
- **Summary:** The 'No REST' marketing claim is a direct contradiction of the actual implementation described later. This, combined with the lack of a defined security boundary for the UI server and unsafe autonomous deletion capabilities, requires immediate correction.
- **Findings:** 1 Critical, 4 Major, 3 Minor

---

## Dissenting Opinions

Panel was unanimous.

---

## Action Items

- [ ] Resolve the HTTP/REST vs. MQTT protocol contradiction in the text.
- [ ] Update the Mermaid diagram to route Obsidian through the Server/Router rather than directly to LanceDB.
- [ ] Add a "UI Connection & Trust Boundary" section specifying the localhost bind address and auth model.
- [ ] Add an "Episodic Buffer Policy" section defining retention limits and opt-out controls.
- [ ] Add a brief "Biology ↔ Engineering" glossary.

---

*Generated by ARC-7 Panel · 2026-04-20*