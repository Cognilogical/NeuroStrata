# Digital Twins & Counterfactual Ontology Scenarios

## State Schemas & Telemetry Ingestion

A **digital twin** is a typed, versioned projection of an entity's state, defined by a JSON Schema that declares:

- `state_vars` — typed fields with finite bounds (e.g., `temperature: { type: "number", min: -273.15 }`)
- `reducers` — pure functions that accept an action payload and produce a new state
- `telemetry` — the ingestion channels that feed live observations into the twin

`neurostrata_twin` creates a twin from a schema, ingests telemetry, and exposes `state` and `history` queries. Twins are polymorphic: the same schema can describe a microservice, a physical asset, or an organizational role.

## CoW Scenario Overlays

A **scenario** is a copy-on-write overlay on top of one or more twins:

- `neurostrata_scenario` creates a named scenario from a base twin
- `apply_action` stages a kinetic action against the scenario; the action's `TwinVarPrecondition` bounds are evaluated against the scenario's *projected* state, not the live twin
- `evaluate_sparql` runs read-only SPARQL ASK queries against the scenario's staged post-image
- `diff` compares the scenario against its base twin or another scenario

Scenarios are cheap because they share unchanged state with their base twin. When an action is applied, only the touched state vars are materialized in the scenario's private overlay. This makes counterfactual exploration safe: no scenario can corrupt the live twin until its actions are explicitly promoted via `neurostrata_action`.
