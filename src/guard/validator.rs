//! Validator - orchestrates semantic and sandbox evaluation
//!
//! Combines behavioral rule matching with optional sandbox execution
//! to provide comprehensive action validation.

use crate::guard::models::{ValidateVerdict, BehavioralRule};
use crate::guard::semantic::SemanticEvaluator;
use crate::guard::sandbox::SandboxEvaluator;
use crate::traits::{Embedder, VectorStore};
use std::sync::Arc;
use tracing::info;

pub struct GuardValidator {
    semantic: SemanticEvaluator,
    sandbox: SandboxEvaluator,
    namespace: String,
}

impl GuardValidator {
    pub fn new(
        store: Arc<dyn VectorStore>,
        embedder: Arc<dyn Embedder>,
        namespace: &str,
    ) -> Self {
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
        info!("Validating action: {} in namespace: {}", action_type, self.namespace);

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

        let rule_ids: Vec<String> = matching_rules.iter().map(|r| r.id.clone()).collect();

        // Step 2: Check for deterministic rejects
        for rule in &matching_rules {
            if rule.status == "deprecated" {
                continue;
            }

            // Check if trigger pattern matches
            if Self::matches_trigger(&rule.trigger_pattern, action_type, payload) {
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
        }

        // Step 3: Optional sandbox execution
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

        // Step 4: Semantic pass - no rule violations found
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

    #[tokio::test]
    async fn test_matches_trigger() {
        // Test pattern matching
        assert!(GuardValidator::matches_trigger("bash", "bash", "echo hello"));
        assert!(GuardValidator::matches_trigger("rm -rf", "bash", "rm -rf /"));
        assert!(!GuardValidator::matches_trigger("bash", "write_file", "content"));
    }

}
