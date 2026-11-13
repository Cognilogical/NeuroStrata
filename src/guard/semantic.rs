//! Semantic evaluator over the behavioral rules stored in LadyBugDB.
//!
//! Validates actions against behavioral rules stored as memories in the 'guard' namespace.
//! Each rule is embedded and stored with memory_type="guard_rule". Embedding a rule when
//! it is written keeps it searchable by the general search surfaces; the guard itself
//! reads rules by listing them (see `candidate_rules`), not by vector search.

use crate::guard::models::BehavioralRule;
use crate::traits::{Embedder, MemoryPayload, SearchResult, VectorStore};
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::sync::Arc;

// There is deliberately no similarity or distance floor here.
//
// Composite Rec #4 (docs/designs/2026-10-24-composite-improvement-recommendations.md)
// asked for a calibrated max-distance cutoff so that a loosely related rule,
// returned only because vector search returns neighbours rather than matches,
// could not turn a validate call into a false rejection. The floor that was
// added for it (`MIN_CONSTRAINT_SIMILARITY`, 0.25) was compared against the
// store's L2 *distance*, so it dropped the closest rules and kept the farthest.
//
// Calibrating a cutoff against a snapshot of a real store, with its real
// embedder and file-edit payloads taken from its git history (measurements in
// PR #106), showed it cannot work as asked: a cutoff in the correct direction
// at 0.75 admitted almost no candidate (0.00 and 0.10 rules per call for the
// two payload forms tried), and rules that govern a file were not closer to
// the payload than other rules. It also protected nothing, because a rule
// fires only when its own `trigger_pattern` occurs in the payload (the other
// half of Rec #4, kept), so a loosely related candidate cannot cause a false
// rejection. This file now lists the candidates instead of ranking them; see
// `candidate_rules`.

/// The memory-type families the guard treats as rules. `guard_rule` is what
/// `add_constraint` writes; `rule` is the vocabulary's generic rule type, so
/// rules seeded through the normal memory surface stay visible to the guard.
const RULE_MEMORY_TYPES: [&str; 2] = ["rule", "guard_rule"];

/// The most rules one evaluation will enforce.
///
/// The listing has no page limit of its own, so this is the guard's stated
/// ceiling: above it the evaluation is refused rather than silently truncated,
/// because enforcing a subset would be a policy hole. 5,000 is far above any
/// namespace measured here (3,000 is the sizing probe) and well below the
/// point where a single validation pass stops being bounded.
pub(crate) const MAX_GUARD_RULES: usize = 5_000;

/// Refuse to enforce more than [`MAX_GUARD_RULES`] rules.
fn enforce_rule_cap(namespace: &str, count: usize) -> Result<()> {
    if count > MAX_GUARD_RULES {
        anyhow::bail!(
            "refusing to enforce '{namespace}': {count} rules exceed the {MAX_GUARD_RULES}-rule cap; retire rules or split the namespace"
        );
    }
    Ok(())
}

/// Convert listed rows to enforceable rules and apply the cap to that set.
///
/// The order matters: the listing returns every `rule`/`guard_rule` row, and
/// most `rule` memories are not guard rules (they lack the guard fields).
/// Capping the raw rows would let ordinary memory growth disable validation.
fn rules_within_cap(namespace: &str, rows: Vec<SearchResult>) -> Result<Vec<BehavioralRule>> {
    let rules = rules_from_rows(rows);
    enforce_rule_cap(namespace, rules.len())?;
    Ok(rules)
}

pub struct SemanticEvaluator {
    store: Arc<dyn VectorStore>,
    embedder: Arc<dyn Embedder>,
    namespace: String,
}

impl SemanticEvaluator {
    pub fn new(store: Arc<dyn VectorStore>, embedder: Arc<dyn Embedder>, namespace: &str) -> Self {
        Self {
            store,
            embedder,
            namespace: namespace.to_string(),
        }
    }

    /// Add a behavioral rule to the guard namespace
    pub async fn add_constraint(&self, rule: BehavioralRule) -> Result<()> {
        let text_to_embed = format!("{} {}", rule.trigger_pattern, rule.constraint_text);
        let vector = self.embedder.embed(&text_to_embed).await?;

        let payload = MemoryPayload {
            content: rule.constraint_text.clone(),
            user_id: "guard".to_string(),
            memory_type: "guard_rule".to_string(),
            agent_name: Some("neurostrata-guard".to_string()),
            location: String::new(),
            location_lines: String::new(),
            metadata: serde_json::json!({
                "id": rule.id,
                "version": rule.version,
                "rule_class": rule.rule_class,
                "trigger_pattern": rule.trigger_pattern,
                "hit_count": rule.hit_count,
                "status": rule.status,
            }),
        };

        self.store
            .upsert(&self.namespace, &rule.id, vector, payload)
            .await
            .context("Failed to store guard rule")?;

        Ok(())
    }

    /// Every rule the guard could act on, from this namespace and `global`.
    ///
    /// The candidates are the rule rows themselves, not the nearest vector
    /// neighbours of the payload.
    ///
    /// Why: whether a rule fires is decided by its `trigger_pattern` alone
    /// (`GuardValidator::matches_trigger`). How close the rule's text sits to
    /// the payload never reaches a verdict; it only decided whether the rule
    /// was offered to the trigger match at all. Firing can only grow with the
    /// candidate set, so any nearest-neighbour cut (the top few rows, or a
    /// distance floor) can only lose enforcement: a rule outside it did not
    /// fire even when its trigger was in the payload. Nearness is also a poor
    /// ranking here. On a snapshot of a real store, rules that govern a file
    /// were no closer to a payload about that file than other rules
    /// (measurements in PR #106), so a cut did not preferentially keep the
    /// relevant ones.
    ///
    /// Behaviour change: a rule the old cut left out, whose trigger occurs in
    /// the payload, now fires. That is the enforcement the rule asked for, but
    /// a store holding such rules will see rejections it did not see before.
    ///
    /// Cost: one listing per rule type per namespace. It grows with the number
    /// of `rule` and `guard_rule` rows, not with the size of the namespace, and
    /// it projects no embedding column, unlike the general `search` call this
    /// replaces, which ranks every row in the namespace by distance.
    ///
    /// The guard applies its own row filter (`is_live_rule_row`, then the
    /// guard fields in `rules_from_rows`) instead of passing the store's
    /// row-class flags, as that `search` call did with `include_tasks` and
    /// `include_archived` both true. A change to the general search path's
    /// contract therefore cannot silently change which rules the guard is able
    /// to see. Archived rows are still candidates, as they were before: that
    /// call asked for them.
    ///
    /// A store failure listing this namespace is an error, so the validator
    /// rejects instead of passing with fewer rules than exist. A failure
    /// listing `global` is logged and the namespace's own rules still apply.
    pub async fn candidate_rules(&self) -> Result<Vec<BehavioralRule>> {
        let now = chrono::Utc::now().timestamp();
        let own = self.namespace.as_str();
        let mut namespaces = vec![own];
        if own != "global" {
            namespaces.push("global");
        }

        let mut rows: Vec<SearchResult> = Vec::new();
        let mut seen_ids: HashSet<String> = HashSet::new();
        for namespace in namespaces {
            for memory_type in RULE_MEMORY_TYPES {
                let listed = match self.store.list(namespace, None, Some(memory_type)).await {
                    Ok(listed) => listed,
                    Err(e) if namespace == own => {
                        return Err(e.context(format!(
                            "could not list {memory_type} rows in '{namespace}'"
                        )));
                    }
                    Err(e) => {
                        tracing::warn!("guard could not list {memory_type} rows in '{namespace}': {e}");
                        continue;
                    }
                };
                for row in listed {
                    // The replaced `search` call dropped retired, conflicting
                    // and registry rows before returning them, so `rule` rows
                    // keep that behaviour here. `guard_rule` rows were always
                    // taken from a plain listing with no such filter, so a
                    // retired `guard_rule` still fires. That is unchanged and
                    // is a separate decision from this one.
                    if memory_type == "rule" && !is_live_rule_row(&row.payload.metadata, now) {
                        continue;
                    }
                    if seen_ids.insert(row.id.clone()) {
                        rows.push(row);
                    }
                }
            }
        }

        // Count the rules the guard can enforce, not every listed row: a
        // namespace with many generic `rule` memories (or any other row type
        // the listing returns) must not trip the cap when few are enforceable.
        rules_within_cap(own, rows)
    }

    /// Record one firing of a rule: bump its persisted `hit_count` by one.
    ///
    /// `hit_count` is the only record of how often a rule actually fired, and
    /// it was read as a live signal while nothing ever wrote it -- so every
    /// consumer of the field was reading a constant zero.
    ///
    /// The bump reads the row, changes this one field, and writes the row
    /// back with the vector it already had: the rule keeps its embedding and
    /// its identity, and only the counter moves.
    pub async fn increment_hit_count(&self, rule_id: &str) -> Result<()> {
        let (target_ns, _vector, payload) =
            if let Some((v, p)) = self.store.get(&self.namespace, rule_id).await? {
                (self.namespace.as_str(), v, p)
            } else if self.namespace != "global" {
                let Some((v, p)) = self.store.get("global", rule_id).await? else {
                    return Ok(());
                };
                ("global", v, p)
            } else {
                // The rule is gone. There is nothing left to count, and inventing
                // a row for a rule that no longer exists would resurrect it.
                return Ok(());
            };

        if !payload.metadata.is_object() {
            anyhow::bail!("rule {rule_id} carries metadata that is not an object");
        }

        // The increment is one write turn in the store, so two validators
        // firing the same rule serialize instead of overwriting each other's
        // read (the old get/modify/upsert pair lost updates). The resolution
        // above only names the namespace; the counter itself is atomic.
        self.store
            .increment_metadata_counter(target_ns, rule_id, "hit_count")
            .await
            .context("Failed to record the rule hit")?;

        Ok(())
    }
}

/// Whether a `rule` row is live: not retired, not an unresolved mesh conflict
/// and not a registry row. These are the row classes the general search drops
/// before a row can be returned, which the guard's earlier `search` call
/// relied on and a plain listing does not do.
fn is_live_rule_row(metadata: &serde_json::Value, now: i64) -> bool {
    !crate::store::ladybug::is_retired_at(metadata, now)
        && !crate::synaptic_mesh::conflict::is_conflicting_metadata(metadata)
        && !crate::registry::is_registry_row(metadata)
}

/// Turn listed rows into guard rules, dropping every row that cannot prove it
/// is a rule: wrong memory type, or missing any of the guard fields. A pure
/// pass over the rows so the filtering is testable without a store.
pub(crate) fn rules_from_rows(rows: Vec<SearchResult>) -> Vec<BehavioralRule> {
    rows.into_iter()
        .filter(|result| RULE_MEMORY_TYPES.contains(&result.payload.memory_type.as_str()))
        .filter_map(|result| {
            // A rule row whose metadata is incomplete is not a rule: it is
            // dropped, and the drop is logged so a corrupt row is visible
            // instead of silently ignored.
            try_parse_rule_row(&result).or_else(|| {
                tracing::warn!(
                    "guard rule row {} missing required metadata field",
                    result.id
                );
                None
            })
        })
        .collect()
}

/// Extract the rule fields from one search row's metadata. `None` means the
/// row is missing at least one required field (`id`, `version`, `rule_class`,
/// `trigger_pattern`, `hit_count`, `status`).
fn try_parse_rule_row(result: &crate::traits::SearchResult) -> Option<BehavioralRule> {
    let metadata = &result.payload.metadata;
    Some(BehavioralRule {
        id: metadata.get("id")?.as_str()?.to_string(),
        version: metadata.get("version")?.as_u64()? as u32,
        rule_class: metadata.get("rule_class")?.as_str()?.to_string(),
        trigger_pattern: metadata.get("trigger_pattern")?.as_str()?.to_string(),
        hit_count: metadata.get("hit_count")?.as_u64()? as u32,
        status: metadata.get("status")?.as_str()?.to_string(),
        constraint_text: result.payload.content.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::{MemoryPayload, SearchResult};
    use serde_json::json;

    fn rule_row(id: &str, score: f32, memory_type: &str) -> SearchResult {
        SearchResult {
            id: id.to_string(),
            score,
            payload: MemoryPayload {
                content: "always use podman, never docker".to_string(),
                user_id: "guard".to_string(),
                memory_type: memory_type.to_string(),
                agent_name: Some("neurostrata-guard".to_string()),
                location: String::new(),
                location_lines: String::new(),
                metadata: json!({
                    "id": id,
                    "version": 1,
                    "rule_class": "blocking",
                    "trigger_pattern": "docker",
                    "hit_count": 0,
                    "status": "active",
                }),
            },
            evidence: None,
        }
    }

    #[tokio::test]
    async fn test_guard_rule_storage() {
        use crate::guard::models::BehavioralRule;
        use crate::guard::testkit::{MockStore, StubEmbedder};
        use crate::traits::{Embedder, VectorStore};

        let store: Arc<dyn VectorStore> = Arc::new(MockStore::new());
        let embedder: Arc<dyn Embedder> = Arc::new(StubEmbedder);
        let matcher = SemanticEvaluator::new(store, embedder, "guard");

        let rule = BehavioralRule {
            id: "rule-podman".to_string(),
            version: 1,
            rule_class: "blocking".to_string(),
            trigger_pattern: "docker".to_string(),
            constraint_text: "always use podman, never docker".to_string(),
            hit_count: 0,
            status: "active".to_string(),
        };
        matcher
            .add_constraint(rule)
            .await
            .expect("storing a guard rule must succeed");

        let rules = matcher
            .candidate_rules()
            .await
            .expect("listing candidate rules must succeed");

        assert!(
            rules
                .iter()
                .any(|r| r.id == "rule-podman"
                    && r.constraint_text == "always use podman, never docker"),
            "stored guard rule must be a candidate, got: {rules:?}"
        );
    }

    async fn seed(
        store: &Arc<dyn VectorStore>,
        namespace: &str,
        id: &str,
        memory_type: &str,
        metadata: serde_json::Value,
    ) {
        let mut payload = rule_row(id, 0.0, memory_type).payload;
        payload.metadata = metadata;
        store
            .upsert(namespace, id, vec![0.0; 4], payload)
            .await
            .expect("seeding a row must succeed");
    }

    fn guard_fields(id: &str, trigger: &str) -> serde_json::Value {
        json!({
            "id": id, "version": 1, "rule_class": "blocking",
            "trigger_pattern": trigger, "hit_count": 0, "status": "active",
        })
    }

    async fn candidates(store: Arc<dyn VectorStore>, namespace: &str) -> Vec<String> {
        use crate::guard::testkit::StubEmbedder;
        let mut ids: Vec<String> = SemanticEvaluator::new(store, Arc::new(StubEmbedder), namespace)
            .candidate_rules()
            .await
            .expect("listing candidate rules must succeed")
            .into_iter()
            .map(|r| r.id)
            .collect();
        ids.sort();
        ids
    }

    /// The reason candidates are listed rather than ranked: a `rule` row that
    /// carries guard fields must be offered to the trigger match however many
    /// other rules there are and however far its text is from the payload. A
    /// vector cut (top 3, or a distance floor) offered it only by luck.
    #[tokio::test]
    async fn a_rule_row_with_guard_fields_is_a_candidate_among_many_other_rules() {
        use crate::guard::testkit::MockStore;
        let store: Arc<dyn VectorStore> = Arc::new(MockStore::new());
        for i in 0..40 {
            seed(&store, "guard", &format!("plain-{i}"), "rule", json!({})).await;
        }
        seed(&store, "guard", "seeded-rule", "rule", guard_fields("seeded-rule", "docker")).await;
        seed(&store, "guard", "listed-guard", "guard_rule", guard_fields("listed-guard", "rm -rf")).await;

        assert_eq!(
            candidates(store, "guard").await,
            vec!["listed-guard".to_string(), "seeded-rule".to_string()],
            "rows without guard fields are skipped, every row with them is offered"
        );
    }

    /// Rows the replaced `search` call dropped before it could return them
    /// stay out: a retired rule must not start firing because the guard now
    /// lists instead of searching.
    #[tokio::test]
    async fn retired_conflicting_and_registry_rule_rows_are_not_candidates() {
        use crate::guard::testkit::MockStore;
        let store: Arc<dyn VectorStore> = Arc::new(MockStore::new());
        let with = |extra: serde_json::Value| {
            let mut m = guard_fields("x", "docker");
            for (k, v) in extra.as_object().unwrap() {
                m[k] = v.clone();
            }
            m
        };
        seed(&store, "guard", "live", "rule", with(json!({}))).await;
        seed(&store, "guard", "retired", "rule", with(json!({ "valid_to": 1 }))).await;
        seed(&store, "guard", "conflicting", "rule", with(json!({ "mesh": { "conflict": true } }))).await;

        let ids = candidates(store, "guard").await;
        assert_eq!(ids, vec!["x".to_string()], "only the live row's guard id remains: {ids:?}");
    }

    #[tokio::test]
    async fn global_rules_apply_in_every_namespace_without_repeating() {
        use crate::guard::testkit::MockStore;
        let store: Arc<dyn VectorStore> = Arc::new(MockStore::new());
        seed(&store, "global", "g-rule", "rule", guard_fields("g-rule", "docker")).await;
        seed(&store, "global", "g-guard", "guard_rule", guard_fields("g-guard", "rm -rf")).await;
        seed(&store, "proj", "p-rule", "rule", guard_fields("p-rule", "force")).await;

        assert_eq!(
            candidates(store.clone(), "proj").await,
            vec!["g-guard".to_string(), "g-rule".to_string(), "p-rule".to_string()]
        );
        assert_eq!(
            candidates(store, "global").await,
            vec!["g-guard".to_string(), "g-rule".to_string()],
            "the global namespace is listed once, not twice"
        );
    }

    /// The guard's stated ceiling: at the cap it enforces, above it it
    /// refuses rather than truncating the rule set.
    #[test]
    fn listing_cap_is_honored() {
        assert!(enforce_rule_cap("probe", MAX_GUARD_RULES).is_ok());
        let err = enforce_rule_cap("probe", MAX_GUARD_RULES + 1)
            .expect_err("over the cap must refuse");
        let message = err.to_string();
        assert!(message.contains("exceed the"), "{message}");
        assert!(message.contains(&MAX_GUARD_RULES.to_string()), "{message}");
    }

    /// The cap counts parsed guard rules, not listed rows: ordinary `rule`
    /// memories without guard fields must not disable the guard.
    #[test]
    fn listing_cap_counts_parsed_rules_not_rows() {
        // One enforceable rule plus far more generic rows than the cap.
        let mut rows: Vec<SearchResult> = Vec::new();
        let mut generic = rule_row("generic", 0.0, "rule");
        generic.payload.metadata = json!({"note": "not a guard rule"});
        rows.push(rule_row("enforceable", 0.0, "guard_rule"));
        rows.extend(std::iter::repeat_n(generic, MAX_GUARD_RULES + 10));

        let rules = rules_within_cap("probe", rows)
            .expect("many non-guard rules must not trip the cap");
        assert_eq!(rules.len(), 1, "only the guard-shaped row survives conversion");
    }

    /// Above the cap of actually enforceable rules the guard still refuses.
    #[test]
    fn listing_cap_refuses_above_the_cap_of_parsed_rules() {
        let rows: Vec<SearchResult> = (0..=MAX_GUARD_RULES)
            .map(|i| rule_row(&format!("rule-{i}"), 0.0, "guard_rule"))
            .collect();
        let err = rules_within_cap("probe", rows).expect_err("over the cap must refuse");
        assert!(err.to_string().contains("exceed the"), "{err}");
    }

    /// Concurrent firings of the same rule must not lose increments: the
    /// store's read-modify-write is one write turn, not a separate get and
    /// upsert.
    #[tokio::test]
    async fn concurrent_hit_counts_are_not_lost() {
        use crate::guard::testkit::StubEmbedder;
        use crate::traits::{Embedder, VectorStore};

        let dir = std::env::temp_dir().join(format!("ns-hit-count-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> = Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&dir, 4).expect("open temp database"),
        );
        store.init("guard").await.expect("init");
        let payload = MemoryPayload {
            content: "always use podman".to_string(),
            user_id: "guard".to_string(),
            memory_type: "guard_rule".to_string(),
            agent_name: Some("neurostrata-guard".to_string()),
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({
                "id": "rule-concurrent",
                "version": 1,
                "rule_class": "blocking",
                "trigger_pattern": "docker",
                "hit_count": 0,
                "status": "active",
            }),
        };
        store
            .upsert("guard", "rule-concurrent", vec![0.0; 4], payload)
            .await
            .expect("seed the rule");

        let embedder: Arc<dyn Embedder> = Arc::new(StubEmbedder);
        let evaluator = Arc::new(SemanticEvaluator::new(store.clone(), embedder, "guard"));
        let mut handles = Vec::new();
        for _ in 0..25 {
            let evaluator = evaluator.clone();
            handles.push(tokio::spawn(async move {
                evaluator.increment_hit_count("rule-concurrent").await
            }));
        }
        for handle in handles {
            handle.await.expect("task joins").expect("increment succeeds");
        }

        let (_, payload) = store
            .get("guard", "rule-concurrent")
            .await
            .expect("read")
            .expect("the rule survives");
        assert_eq!(
            payload.metadata["hit_count"],
            json!(25),
            "every concurrent firing is counted exactly once"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Sizing probe: 3,000 rules must still be listed and converted well
    /// inside the 750 ms budget the guard-3000 suite names.
    #[tokio::test]
    async fn three_thousand_rules_within_budget() {
        use crate::guard::testkit::StubEmbedder;
        use crate::traits::{Embedder, VectorStore};

        let dir = std::env::temp_dir().join(format!("ns-guard-3000-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> = Arc::new(
            crate::store::ladybug::LadybugStore::for_testing(&dir, 4).expect("open temp database"),
        );
        store.init("guard").await.expect("init");
        let embedder: Arc<dyn Embedder> = Arc::new(StubEmbedder);
        let evaluator = SemanticEvaluator::new(store.clone(), embedder, "guard");

        for i in 0..3_000u32 {
            let payload = MemoryPayload {
                content: format!("rule {i}: always use podman"),
                user_id: "guard".to_string(),
                memory_type: "guard_rule".to_string(),
                agent_name: Some("neurostrata-guard".to_string()),
                location: String::new(),
                location_lines: String::new(),
                metadata: json!({
                    "id": format!("rule-{i}"),
                    "version": 1,
                    "rule_class": "blocking",
                    "trigger_pattern": "docker",
                    "hit_count": 0,
                    "status": "active",
                }),
            };
            store
                .upsert("guard", &format!("rule-{i}"), vec![0.0; 4], payload)
                .await
                .expect("seed a rule");
        }

        let started = std::time::Instant::now();
        let rules = evaluator.candidate_rules().await.expect("list 3,000 rules");
        let elapsed = started.elapsed();
        assert_eq!(rules.len(), 3_000, "every rule is listed");
        assert!(
            elapsed < std::time::Duration::from_millis(750),
            "3,000-rule listing took {elapsed:?}, budget 750 ms"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Row conversion keeps rule types with complete guard fields and nothing
    /// else, and does not look at the score: there is no distance floor.
    #[test]
    fn rows_convert_by_type_and_fields_not_by_score() {
        let rows = vec![
            rule_row("near", 0.0, "guard_rule"),
            rule_row("far", 9.0, "guard_rule"),
            rule_row("not-a-rule", 0.0, "fact"),
        ];

        let rules = rules_from_rows(rows);

        let ids: Vec<&str> = rules.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["near", "far"], "score never decides: {ids:?}");
        assert_eq!(rules[0].trigger_pattern, "docker");
        assert_eq!(rules[0].status, "active");
    }
}
