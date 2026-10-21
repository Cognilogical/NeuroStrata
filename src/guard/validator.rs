//! Validator - orchestrates semantic and sandbox evaluation
//!
//! Combines behavioral rule matching with optional sandbox execution
//! to provide comprehensive action validation.

use crate::guard::models::{BehavioralRule, ValidateVerdict};
use crate::guard::sandbox::SandboxEvaluator;
use crate::guard::semantic::SemanticEvaluator;
use crate::traits::{Embedder, VectorStore};
use std::sync::Arc;
use tracing::info;

pub struct GuardValidator {
    semantic: SemanticEvaluator,
    sandbox: SandboxEvaluator,
    namespace: String,
}

impl GuardValidator {
    pub fn new(store: Arc<dyn VectorStore>, embedder: Arc<dyn Embedder>, namespace: &str) -> Self {
        let semantic = SemanticEvaluator::new(store, embedder, namespace);
        let sandbox = SandboxEvaluator::new();

        Self {
            semantic,
            sandbox: tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(sandbox)
            }),
            namespace: namespace.to_string(),
        }
    }

    /// Validate an action against behavioral rules and optionally sandbox
    ///
    /// # Arguments
    /// * `action_type` - Type of action (bash, write_file, etc.)
    /// * `payload` - The actual command or content
    /// * `cwd` - Current working directory for sandbox execution
    /// * `use_sandbox` - Whether to execute in sandbox after semantic check
    ///
    /// # Returns
    /// ValidateVerdict indicating pass/reject with reasons and constraints
    pub async fn validate(
        &self,
        action_type: &str,
        payload: &str,
        cwd: &str,
        use_sandbox: bool,
    ) -> (ValidateVerdict, Vec<String>) {
        info!(
            "Validating action: {} in namespace: {}",
            action_type, self.namespace
        );

        // Step 1: Semantic evaluation - find matching behavioral rules
        let matching_rules = match self.semantic.evaluate(payload).await {
            Ok(rules) => rules,
            Err(e) => {
                tracing::error!("Semantic evaluation failed: {}", e);
                return (
                    ValidateVerdict::DeterministicReject {
                        reasons: vec![format!("Semantic evaluation error: {}", e)],
                        constraints: vec![],
                    },
                    vec![],
                );
            }
        };

        // Step 2: Work out which of the candidate rules actually FIRED.
        //
        // The semantic evaluator returns the top-N vector neighbours: a
        // candidate list, not a verdict. A rule fires when it is live (not
        // deprecated) and its trigger pattern matches the action. Recording
        // the candidates as "triggered" made the persisted signal a lie -- it
        // named rules that never fired, on passes as well as on rejects.
        let fired: Vec<&BehavioralRule> = matching_rules
            .iter()
            .filter(|rule| rule.status != "deprecated")
            .filter(|rule| Self::matches_trigger(&rule.trigger_pattern, action_type, payload))
            .collect();

        let rule_ids: Vec<String> = fired.iter().map(|rule| rule.id.clone()).collect();

        // Step 3: Record the fires.
        //
        // `hit_count` is the count of how often a rule fired, so it moves
        // here and nowhere else. A failed write is logged, not fatal: the
        // verdict is the guard's job and the counter is its bookkeeping, and
        // a bookkeeping failure must not turn a pass into a rejection.
        for rule in &fired {
            if let Err(e) = self.semantic.increment_hit_count(&rule.id).await {
                tracing::warn!("Failed to record the hit for rule {}: {}", rule.id, e);
            }
        }

        // Step 4: Check for deterministic rejects. The FIRST rule that fired
        // decides, in the same order the rules came back in.
        if let Some(rule) = fired.first() {
            return (
                ValidateVerdict::DeterministicReject {
                    reasons: vec![format!(
                        "Action violates rule {}: {}",
                        rule.id, rule.constraint_text
                    )],
                    constraints: vec![rule.constraint_text.clone()],
                },
                rule_ids,
            );
        }

        // Step 5: Optional sandbox execution
        if use_sandbox {
            info!("Executing sandbox evaluation for action: {}", action_type);

            let sandbox_result = self.sandbox.evaluate(action_type, payload, cwd).await;

            match sandbox_result {
                Ok(verdict) => {
                    // Extract constraints from rules for context
                    let constraints: Vec<String> = matching_rules
                        .iter()
                        .filter(|r| r.status != "deprecated")
                        .map(|r| r.constraint_text.clone())
                        .collect();

                    return (verdict, rule_ids);
                }
                Err(e) => {
                    tracing::error!("Sandbox evaluation failed: {}", e);
                    return (
                        ValidateVerdict::DeterministicReject {
                            reasons: vec![format!("Sandbox evaluation error: {}", e)],
                            constraints: vec![],
                        },
                        rule_ids,
                    );
                }
            }
        }

        // Step 6: Semantic pass - no rule violations found
        (
            ValidateVerdict::SandboxPassHighFidelity {
                notes: vec![format!(
                    "Semantic validation passed with {} matching rules",
                    matching_rules.len()
                )],
            },
            rule_ids,
        )
    }

    /// Check if a rule's trigger pattern matches the action
    fn matches_trigger(trigger_pattern: &str, action_type: &str, payload: &str) -> bool {
        // Simple pattern matching - can be enhanced with regex or more sophisticated matching
        let pattern_lower = trigger_pattern.to_lowercase();
        let action_lower = action_type.to_lowercase();
        let payload_lower = payload.to_lowercase();

        // Check if trigger matches action type or appears in payload
        pattern_lower.contains(&action_lower) || payload_lower.contains(&pattern_lower)
    }

    /// Add a new behavioral rule to the guard namespace
    pub async fn add_rule(&self, rule: BehavioralRule) -> Result<(), String> {
        self.semantic
            .add_constraint(rule)
            .await
            .map_err(|e| format!("Failed to add rule: {}", e))
    }

    /// List all rules in the guard namespace
    pub async fn list_rules(&self) -> Result<Vec<BehavioralRule>, String> {
        // Use a broad query to get all rules
        self.semantic
            .match_constraints("rule constraint behavioral", 100)
            .await
            .map_err(|e| format!("Failed to list rules: {}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guard::models::BehavioralRule;
    use crate::guard::testkit::{MockStore, StubEmbedder};

    const NS: &str = "guard-test";

    fn rule(id: &str, trigger: &str, constraint: &str, status: &str) -> BehavioralRule {
        BehavioralRule {
            id: id.to_string(),
            version: 1,
            rule_class: "safety".to_string(),
            trigger_pattern: trigger.to_string(),
            constraint_text: constraint.to_string(),
            hit_count: 0,
            status: status.to_string(),
        }
    }

    /// A validator over an in-memory store, seeded with the given rules.
    ///
    /// The rules go in through `add_rule`, so they carry the metadata the
    /// semantic evaluator requires -- the same path production writes.
    async fn validator_with(rules: Vec<BehavioralRule>) -> (Arc<MockStore>, GuardValidator) {
        let store = Arc::new(MockStore::new());
        let validator = GuardValidator::new(store.clone(), Arc::new(StubEmbedder), NS);
        for r in rules {
            validator.add_rule(r).await.expect("seed the rule");
        }
        (store, validator)
    }

    #[tokio::test]
    async fn test_matches_trigger() {
        // Test pattern matching
        assert!(GuardValidator::matches_trigger(
            "bash",
            "bash",
            "echo hello"
        ));
        assert!(GuardValidator::matches_trigger(
            "rm -rf", "bash", "rm -rf /"
        ));
        assert!(!GuardValidator::matches_trigger(
            "bash",
            "write_file",
            "content"
        ));
    }

    /// BUG-5: the recorded signal has to name the rules that FIRED. The
    /// semantic evaluator returns the top-3 vector neighbours, which are
    /// candidates, not fires -- a rule that merely looked similar did not
    /// trigger, and recording it as triggered makes the signal a lie.
    #[tokio::test(flavor = "multi_thread")]
    async fn rule_ids_triggered_names_only_the_rules_that_fired() {
        let (_store, validator) = validator_with(vec![
            rule("rule.fires", "rm -rf", "never rm -rf", "active"),
            rule("rule.silent", "curl", "never curl", "active"),
        ])
        .await;

        // Both rules are vector candidates; neither trigger matches this
        // action, so nothing fired.
        let (verdict, fired) = validator
            .validate("bash", "echo hello", "/tmp", false)
            .await;
        assert!(
            matches!(verdict, ValidateVerdict::SandboxPassHighFidelity { .. }),
            "an action no rule catches passes"
        );
        assert!(
            fired.is_empty(),
            "a rule that did not fire is not triggered: {:?}",
            fired
        );

        // Now the first rule fires and the second still does not.
        let (verdict, fired) = validator
            .validate("bash", "rm -rf /tmp/x", "/tmp", false)
            .await;
        assert!(matches!(
            verdict,
            ValidateVerdict::DeterministicReject { .. }
        ));
        assert_eq!(
            fired,
            vec!["rule.fires".to_string()],
            "the rule that did not fire is not in the signal"
        );
    }

    /// BUG-5: a deprecated rule is not evaluated at all, so it cannot fire
    /// and cannot be named in the signal.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_deprecated_rule_is_never_listed_as_triggered() {
        let (_store, validator) = validator_with(vec![rule(
            "rule.retired",
            "rm -rf",
            "never rm -rf",
            "deprecated",
        )])
        .await;

        let (verdict, fired) = validator
            .validate("bash", "rm -rf /tmp/x", "/tmp", false)
            .await;
        assert!(
            matches!(verdict, ValidateVerdict::SandboxPassHighFidelity { .. }),
            "a deprecated rule does not reject"
        );
        assert!(
            fired.is_empty(),
            "a deprecated rule is not a trigger: {:?}",
            fired
        );
    }

    /// BUG-4: `hit_count` is the count of how often a rule actually fired. It
    /// was read as a live signal and never written, so every consumer was
    /// reading a constant.
    #[tokio::test(flavor = "multi_thread")]
    async fn hit_count_counts_the_firings_and_nothing_else() {
        let (store, validator) =
            validator_with(vec![rule("rule.fires", "rm -rf", "never rm -rf", "active")]).await;

        assert_eq!(
            store.hit_count_of(NS, "rule.fires"),
            Some(0),
            "a new rule starts at zero"
        );

        // A rule that does not fire does not move the count.
        let _ = validator
            .validate("bash", "echo hello", "/tmp", false)
            .await;
        assert_eq!(
            store.hit_count_of(NS, "rule.fires"),
            Some(0),
            "a rule that did not fire did not hit"
        );

        // A firing rule moves it, once per firing.
        let _ = validator
            .validate("bash", "rm -rf /tmp/x", "/tmp", false)
            .await;
        assert_eq!(store.hit_count_of(NS, "rule.fires"), Some(1));
        let _ = validator
            .validate("bash", "rm -rf /tmp/y", "/tmp", false)
            .await;
        assert_eq!(store.hit_count_of(NS, "rule.fires"), Some(2));
    }

    /// The bump rewrites the row, so the row's other metadata has to survive
    /// it -- a counter that costs the rule its trigger pattern is not a fix.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_hit_count_bump_preserves_the_rest_of_the_rule() {
        let (store, validator) =
            validator_with(vec![rule("rule.fires", "rm -rf", "never rm -rf", "active")]).await;

        let _ = validator
            .validate("bash", "rm -rf /tmp/x", "/tmp", false)
            .await;

        let metadata = store.metadata_of(NS, "rule.fires").expect("the rule row");
        assert_eq!(metadata["hit_count"], serde_json::json!(1));
        assert_eq!(metadata["trigger_pattern"], serde_json::json!("rm -rf"));
        assert_eq!(metadata["status"], serde_json::json!("active"));
        assert_eq!(metadata["rule_class"], serde_json::json!("safety"));
        assert_eq!(metadata["id"], serde_json::json!("rule.fires"));
    }

    /// The mock proves the logic; the real store proves the write. A counter
    /// that only survives in an in-memory row would still leave every consumer
    /// reading a constant.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_hit_count_is_persisted_in_the_real_store() {
        let dir = std::env::temp_dir().join(format!("guard-hit-count-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&dir, 4).expect("open temp database"));
        store.init(NS).await.expect("create the schema");

        let validator = GuardValidator::new(store.clone(), Arc::new(StubEmbedder), NS);
        validator
            .add_rule(rule("rule.fires", "rm -rf", "never rm -rf", "active"))
            .await
            .expect("seed the rule");

        let _ = validator
            .validate("bash", "rm -rf /tmp/x", "/tmp", false)
            .await;

        let (_, payload) = store
            .get(NS, "rule.fires")
            .await
            .expect("read the rule back")
            .expect("the rule row");
        assert_eq!(payload.metadata["hit_count"], serde_json::json!(1));
        assert_eq!(
            payload.metadata["trigger_pattern"],
            serde_json::json!("rm -rf"),
            "the bump kept the rule's own metadata"
        );
    }

    /// The recorded signal changes; the verdict does not. A rule that fires
    /// rejects with that rule's own constraint, exactly as before.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_verdict_is_unchanged_by_the_signal_fix() {
        let (_store, validator) = validator_with(vec![
            rule("rule.fires", "rm -rf", "never rm -rf", "active"),
            rule("rule.other", "curl", "never curl", "active"),
        ])
        .await;

        let (verdict, _fired) = validator
            .validate("bash", "rm -rf /tmp/x", "/tmp", false)
            .await;
        match verdict {
            ValidateVerdict::DeterministicReject {
                reasons,
                constraints,
            } => {
                assert_eq!(
                    reasons,
                    vec!["Action violates rule rule.fires: never rm -rf".to_string()]
                );
                assert_eq!(constraints, vec!["never rm -rf".to_string()]);
            }
            other => panic!("expected a deterministic reject, got {:?}", other),
        }
    }
}
