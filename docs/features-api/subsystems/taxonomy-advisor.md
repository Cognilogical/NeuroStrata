# Subject-Matter Domain Expertise & SKOS Taxonomy Advisor

## SKOS Concept Graph & Disjointness

NeuroStrata's domain expertise layer is built on W3C SKOS (Simple Knowledge Organization System). Concepts form a directed graph where:

- `skos:broader` / `skos:narrower` express subsumption
- `skos:related` links sibling concepts across branches
- `skos:disjointWith` enforces mutual exclusivity — a concept cannot be both A and B

The `neurostrata_taxonomy` tool maintains this graph and enforces DAG validity: every `skos:broader` edge must point to a parent, and `skos:disjointWith` edges must not create cycles.

## SME Frontier Advisor (`advise`)

The Frontier Advisor answers strategic, architectural, or domain-improvement questions by:

1. Querying the SKOS concept graph for relevant concepts
2. Running `public_safe` external research agenda queries (`websearch`, `context7`)
3. Persisting newly discovered literature and SPDX license-verified `comparable_implementations` via `upsert_concept`
4. Grounding all recommendations in canonical project entities (`implemented_interfaces`, `linked_action_types`, governing rules/facts, `locations`) and domain literature

`neurostrata_taxonomy` supports the operations `set_thesis`, `upsert_concept`, `tree`, `briefing`, `compare`, `gaps`, and `advise`, plus `validate_taxonomy_dag` for DAG validity. `set_thesis` writes the domain thesis row using the namespace-qualified row ID `thesis-<namespace_token>--v<version>` (`thesis_row_id` / `taxonomy_namespace_token`); `upsert_concept` inserts or updates a concept using `concept-<namespace_token>--<slug>-v<version>` (`concept_row_id`) so multiple namespaces in a shared LadybugStore never collide on row IDs; `advise` runs the SME Frontier Advisor; `validate_taxonomy_dag` rejects cycles and dangling `skos:broader` edges. When shareable `thesis` or `concept` rows are exported across SynapticMesh, independent foreign-authored creations that derive the same deterministic mesh UUID are preserved in the `.conflicts` overlay rather than overwriting the existing mesh record.

Concept rows may carry two backward-compatible annotation fields: `not_definition` (explicit negative scope boundary) and `deprecation` (`{ status: "deprecated", superseded_by, reason, effective_at }`). When `status` is `deprecated`, read surfaces derive a deterministic banner `[DEPRECATED — SUPERSEDED BY <id>: <reason>]`; a missing or malformed `reason` renders as an empty slot, never an invented explanation. `briefing` and `compare` expose `not_definition`, `deprecation`, and `deprecation_banner` on each returned hydrated concept; `gaps` and `advise` add an advisory `deprecation_and_boundaries` list; and the `neurostrata_get_snapshot` taxonomy strip names deprecated/bounded concepts. These annotations are advisory — they never block a push — and ReBAC/privacy filtering is unchanged (auto-briefing still includes only unrestricted rows). The same structured presentation is attached to memories returned by search, `memories_for_file`, `related`/recall, snapshot, and task views.

Snapshot and task views derive taxonomy visibility from the existing ReBAC policy (`is_public_access_policy` / `rebac::is_restricted`) rather than from raw row eligibility. Concept and thesis counts in `neurostrata_get_snapshot` and `neurostrata_task_validate` are filtered through that predicate, so a caller sees only rows the policy exposes; a namespace with no visible rows reports a zero count and the exact no-visible-concept hint. This keeps advisory ontology signals consistent with `neurostrata_taxonomy` reads and never broadens access.

`neurostrata_taxonomy(operation: "advise", top_k: 5)` returns the top 5 recommendations. `thesis` and `concept` rows default to `eligibility: "private"` and are filtered by `rebac::can_read_memory` across all operations. Auto-briefing in `neurostrata_get_snapshot` (`<= 800` chars) and `subagent_prompt_injection` (`<= 1200` chars) includes only unrestricted (`is_public_access_policy`) rows. Both first probe `"concept"` and `"thesis"` rows by type for active, non-conflicting, unrestricted entries: a namespace with no visible taxonomy rows returns immediately without listing the rest of the namespace or its edges, a thesis-only namespace renders directly from the probed thesis rows without a full namespace scan, a concept-present namespace short-circuits after the first typed `"concept"` probe, and any probe error conservatively falls back to the full read.