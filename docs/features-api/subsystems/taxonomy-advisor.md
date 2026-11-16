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

`neurostrata_taxonomy` supports the operations `set_thesis`, `upsert_concept`, `tree`, `briefing`, `compare`, `gaps`, and `advise`, plus `validate_taxonomy_dag` for DAG validity. `set_thesis` writes the domain thesis row; `upsert_concept` inserts or updates a concept; `advise` runs the SME Frontier Advisor; `validate_taxonomy_dag` rejects cycles and dangling `skos:broader` edges.

`neurostrata_taxonomy(operation: "advise", top_k: 5)` returns the top 5 recommendations. `thesis` and `concept` rows default to `eligibility: "private"` and are filtered by `rebac::can_read_memory` across all operations. Auto-briefing in `neurostrata_get_snapshot` (`<= 800` chars) and `subagent_prompt_injection` (`<= 1200` chars) includes only unrestricted (`is_public_access_policy`) rows. Both first look for `thesis` and `concept` rows by type; a namespace with neither gets no taxonomy section and skips the full namespace and edge read.