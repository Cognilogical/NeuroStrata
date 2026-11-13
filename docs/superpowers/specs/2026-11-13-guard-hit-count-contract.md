# Guard hit_count atomicity and rule cap — contract (WP2.4)

Status: Phase 2 / WP2.4. Findings: "#114 remainder and guard" — the
read-modify-write `hit_count`, the unbounded rule listing, and the missing
sizing probe.

## Contracts

### C1 — a hit increment is one write turn

`VectorStore::increment_metadata_counter` atomically increments an integer
metadata counter. The default implementation is a read-modify-write for
stores without a writer lock; `LadybugStore` overrides it with one
`write_with_deadline` turn: read the row's metadata, increment, write the
metadata back. The store's single-writer lock (and the database file lock
across processes) serialises concurrent increments, so no firing is lost.

`SemanticEvaluator::increment_hit_count` still resolves the rule's namespace
(local, then `global`) with a read, then calls the atomic increment. A
concurrent test fires one rule 25 times and requires exactly 25.

### C2 — the rule listing has a stated cap

The guard enforces at most `MAX_GUARD_RULES = 5_000` rules per evaluation.
Above the cap `candidate_rules` refuses with an error naming the count and the
cap instead of silently truncating the rule set (a subset would be a policy
hole). The cap is a decision, not a page size: the listing is still one pass
per rule type, and 3,000 rules is the sizing probe.

### C3 — sizing probe

`three_thousand_rules_within_budget` seeds 3,000 `guard_rule` rows, lists them
through `candidate_rules`, and requires under 750 ms on a test store. This is
the budget the `guard-3000` suite names.

### C4 — metadata/content decoupling ruling (deferred)

Metadata remains a JSON document on the `Memory` row. The engine has no JSON
column type, so decoupling it into columns would be a schema migration for a
field whose hot writer (`hit_count`) is now a single-writer-turn increment.
The full-document rewrite that motivated decoupling is avoided for the counter
by C1; the remaining metadata writers (`set_metadata`) are already
authorization-gated. Decoupling is deferred unless a writer needs per-field
CAS beyond counters.

## Tests

| Test | Contract |
|---|---|
| `concurrent_hit_counts_are_not_lost` | C1 |
| `listing_cap_is_honored` | C2 |
| `three_thousand_rules_within_budget` | C3 |

## Acceptance

- 25 concurrent firings produce `hit_count == 25`.
- The cap refuses above 5,000 and permits at 5,000.
- 3,000 rules list under the budget.
- Mutation: removing the `LadybugStore` override (default read-modify-write)
  counts 1 of 25 firings; disabling the cap test lets the over-cap case pass.
