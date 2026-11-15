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
    pub async fn new(store: Arc<dyn VectorStore>, embedder: Arc<dyn Embedder>, namespace: &str) -> Self {
        let semantic = SemanticEvaluator::new(store, embedder, namespace);
        let sandbox = SandboxEvaluator::new().await;

        Self {
            semantic,
            sandbox,
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
        let matching_rules = match self.semantic.candidate_rules().await {
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
        // The semantic evaluator returns every rule the guard could act on: a
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
                Ok(mut verdict) => {
                    // Extract constraints from the rules that FIRED, for
                    // context. The candidates are a fallback, not the truth:
                    // a sandbox verdict should name the constraints of the
                    // triggering rules, never the whole candidate list.
                    let constraints: Vec<String> = if fired.is_empty() {
                        matching_rules
                            .iter()
                            .filter(|r| r.status != "deprecated")
                            .map(|r| r.constraint_text.clone())
                            .collect()
                    } else {
                        fired.iter().map(|r| r.constraint_text.clone()).collect()
                    };

                    match &mut verdict {
                        ValidateVerdict::SandboxReject { constraints: ref mut c, .. } => {
                            if c.is_empty() {
                                *c = constraints;
                            }
                        }
                        ValidateVerdict::DeterministicReject { constraints: ref mut c, .. } => {
                            if c.is_empty() {
                                *c = constraints;
                            }
                        }
                        other => {
                            // Pass verdicts carry no constraint field: the
                            // triggered rules are logged, never silently
                            // dropped.
                            if !constraints.is_empty() {
                                tracing::debug!(
                                    rules = ?constraints,
                                    "rules that fired alongside sandbox verdict {:?}",
                                    other
                                );
                            }
                        }
                    }

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

    /// AC2 — Check session + active task + worktree state before permitting
    /// mutating actions (write/edit/bash-mutating). Returns the legal
    /// transition message when blocked (Unpreflighted, no task claim,
    /// or BreathRequired state requires a note). The caller supplies how
    /// many mutations have run since the last `task_update` note (the
    /// Breath counter).
    pub fn guard_validate_with_mutations(
        &self,
        session_id: &str,
        active_task_id: Option<&str>,
        worktree: &str,
        action_type: &str,
        is_mutating: bool,
        mutations_since_note: usize,
    ) -> (bool, Option<String>) {
        crate::guard::fsm::evaluate_fsm_guard(
            session_id,
            active_task_id,
            worktree,
            action_type,
            is_mutating,
            mutations_since_note,
        )
    }

    /// Check if a rule's trigger pattern matches the action
    fn matches_trigger(trigger_pattern: &str, action_type: &str, payload: &str) -> bool {
        // An empty (or whitespace-only) trigger matches everything by
        // containment, so it would fire on every action the store returns:
        // a rule with no trigger is not a rule.
        if trigger_pattern.trim().is_empty() {
            return false;
        }

        // A rule fires when its trigger pattern matches the action type (the
        // action type contains the pattern, not the reverse) or matches the
        // action-appropriate target surface of the payload.
        let action_lower = action_type.trim().to_lowercase();
        let branches: Vec<String> = trigger_pattern
            .split('|')
            .filter(|branch| !branch.trim().is_empty())
            .map(|branch| branch.to_lowercase())
            .collect();

        if branches.iter().any(|b| action_lower.contains(b.as_str())) {
            return true;
        }

        if Self::is_file_mutation_action(&action_lower) {
            let (paths, shebangs) = Self::extract_file_mutation_targets_and_shebangs(payload);
            return branches
                .iter()
                .any(|b| Self::file_mutation_matches_branch(b, &paths, &shebangs));
        }

        if Self::is_shell_action(&action_lower) {
            let commands = Self::extract_shell_commands(payload);
            return commands.iter().any(|cmd| {
                let surfaces = Self::unquoted_command_surfaces(cmd);
                branches
                    .iter()
                    .any(|b| Self::shell_surfaces_match_branch(b, &surfaces))
            });
        }

        let payload_lower = payload.to_lowercase();
        branches.iter().any(|b| payload_lower.contains(b.as_str()))
    }

    fn is_file_mutation_action(action_lower: &str) -> bool {
        matches!(
            action_lower,
            "write"
                | "write_file"
                | "create_file"
                | "create"
                | "edit"
                | "multiedit"
                | "edit_file"
                | "str_replace_editor"
                | "patch"
                | "patch_file"
                | "apply_patch"
                | "replace_string_in_file"
                | "insert_edit_into_file"
                | "notebookedit"
        )
    }

    fn is_shell_action(action_lower: &str) -> bool {
        matches!(
            action_lower,
            "bash"
                | "shell"
                | "local_shell"
                | "exec_command"
                | "run_shell_command"
                | "run_command"
                | "execute_command"
                | "terminal"
                | "gh_exec"
                | "exec"
                | "script"
        )
    }

    fn path_matches_extension(path: &str, ext_branch: &str) -> bool {
        let normalized = path
            .trim()
            .trim_matches(|c| c == '"' || c == '\'')
            .replace('\\', "/");
        let filename = normalized
            .rsplit('/')
            .next()
            .unwrap_or(normalized.as_str())
            .to_lowercase();
        filename.ends_with(ext_branch) && filename.len() > ext_branch.len()
    }

    fn extract_patch_targets_and_shebangs(
        patch_text: &str,
        paths: &mut Vec<String>,
        shebangs: &mut Vec<String>,
    ) {
        for line in patch_text.lines() {
            let trimmed = line.trim();
            for prefix in [
                "*** Update File:",
                "*** Add File:",
                "*** Move to:",
                "+++ b/",
                "+++ ",
            ] {
                if let Some(rest) = trimmed.strip_prefix(prefix) {
                    let candidate = rest.trim();
                    if !candidate.is_empty() && candidate != "/dev/null" {
                        paths.push(candidate.to_lowercase());
                    }
                }
            }
            if let Some(added) = line.strip_prefix('+') {
                if !line.starts_with("+++") {
                    let added_trimmed = added.trim();
                    if let Some(rest) = added_trimmed.strip_prefix("#!") {
                        shebangs.push(rest.trim().to_lowercase());
                    }
                }
            }
        }
    }

    fn collect_shebang_from_content(content: &str, shebangs: &mut Vec<String>) {
        if let Some(first_line) = content.lines().find(|l| !l.trim().is_empty()) {
            if let Some(rest) = first_line.trim().strip_prefix("#!") {
                shebangs.push(rest.trim().to_lowercase());
            }
        }
    }

    fn extract_file_mutation_targets_and_shebangs(payload: &str) -> (Vec<String>, Vec<String>) {
        let mut paths = Vec::new();
        let mut shebangs = Vec::new();

        if let Ok(serde_json::Value::Object(map)) =
            serde_json::from_str::<serde_json::Value>(payload)
        {
            const PATH_KEYS: &[&str] = &[
                "path",
                "file_path",
                "filePath",
                "filepath",
                "target_file",
                "targetFile",
                "filename",
                "notebook_path",
                "notebookPath",
            ];
            const CONTENT_KEYS: &[&str] = &[
                "content",
                "newString",
                "new_string",
                "new_str",
                "replacement",
                "insert_text",
            ];
            const PATCH_KEYS: &[&str] = &["patchText", "patch", "diff"];

            for key in PATH_KEYS {
                if let Some( s) = map.get(*key).and_then(|v| v.as_str()) {
                    paths.push(s.trim().to_lowercase());
                }
            }
            for key in CONTENT_KEYS {
                if let Some(s) = map.get(*key).and_then(|v| v.as_str()) {
                    Self::collect_shebang_from_content(s, &mut shebangs);
                }
            }
            for key in PATCH_KEYS {
                if let Some(s) = map.get(*key).and_then(|v| v.as_str()) {
                    Self::extract_patch_targets_and_shebangs(s, &mut paths, &mut shebangs);
                }
            }
            for array_key in ["edits", "files", "changes"] {
                if let Some(serde_json::Value::Array(items)) = map.get(array_key) {
                    for item in items {
                        if let serde_json::Value::Object(sub) = item {
                            for key in PATH_KEYS {
                                if let Some(s) = sub.get(*key).and_then(|v| v.as_str()) {
                                    paths.push(s.trim().to_lowercase());
                                }
                            }
                            for key in CONTENT_KEYS {
                                if let Some(s) = sub.get(*key).and_then(|v| v.as_str()) {
                                    Self::collect_shebang_from_content(s, &mut shebangs);
                                }
                            }
                        }
                    }
                }
            }
            return (paths, shebangs);
        }

        if payload.contains("*** Update File:")
            || payload.contains("*** Add File:")
            || payload.contains("+++ ")
        {
            Self::extract_patch_targets_and_shebangs(payload, &mut paths, &mut shebangs);
            return (paths, shebangs);
        }

        Self::collect_shebang_from_content(payload, &mut shebangs);
        if !payload.contains('\n') && !payload.trim().is_empty() {
            paths.push(payload.trim().to_lowercase());
        }
        (paths, shebangs)
    }

    fn file_mutation_matches_branch(
        branch_lower: &str,
        paths: &[String],
        shebangs: &[String],
    ) -> bool {
        let trimmed = branch_lower.trim();
        if trimmed.is_empty() {
            return false;
        }
        if trimmed.starts_with('.') && trimmed.len() > 1 {
            return paths.iter().any(|p| Self::path_matches_extension(p, trimmed));
        }

        if paths.iter().any(|p| {
            let normalized = p.replace('\\', "/");
            let filename = normalized.rsplit('/').next().unwrap_or(normalized.as_str());
            filename == trimmed
        }) {
            return true;
        }

        shebangs.iter().any(|shebang| {
            let surfaces = Self::unquoted_command_surfaces(shebang);
            Self::shell_surfaces_match_branch(branch_lower, &surfaces)
        })
    }

    fn extract_shell_commands(payload: &str) -> Vec<String> {
        if let Ok(serde_json::Value::Object(map)) =
            serde_json::from_str::<serde_json::Value>(payload)
        {
            let mut cmds = Vec::new();
            for key in ["command", "cmd", "script", "code"] {
                if let Some(s) = map.get(key).and_then(|v| v.as_str()) {
                    if !s.trim().is_empty() {
                        cmds.push(s.to_string());
                    }
                }
            }
            if cmds.is_empty() {
                if let Some(serde_json::Value::Array(args)) = map.get("args") {
                    let joined: Vec<&str> = args.iter().filter_map(|v| v.as_str()).collect();
                    if !joined.is_empty() {
                        cmds.push(joined.join(" "));
                    }
                }
            }
            return cmds;
        }
        vec![payload.to_string()]
    }

    fn split_shell_segments(cmd: &str) -> Vec<String> {
        let mut segments = Vec::new();
        let mut current = String::new();
        let mut chars = cmd.chars().peekable();
        let mut in_single = false;
        let mut in_double = false;
        let mut escaped = false;

        while let Some(c) = chars.next() {
            if escaped {
                current.push(c);
                escaped = false;
                continue;
            }
            if c == '\\' && !in_single {
                current.push(c);
                escaped = true;
                continue;
            }
            if c == '\'' && !in_double {
                in_single = !in_single;
                current.push(c);
                continue;
            }
            if c == '"' && !in_single {
                in_double = !in_double;
                current.push(c);
                continue;
            }
            if !in_single && !in_double {
                if c == ';' || c == '\n' {
                    if !current.trim().is_empty() {
                        segments.push(std::mem::take(&mut current));
                    } else {
                        current.clear();
                    }
                    continue;
                }
                if c == '|' || c == '&' {
                    if chars.peek() == Some(&c) {
                        let _ = chars.next();
                    }
                    if !current.trim().is_empty() {
                        segments.push(std::mem::take(&mut current));
                    } else {
                        current.clear();
                    }
                    continue;
                }
            }
            current.push(c);
        }
        if !current.trim().is_empty() {
            segments.push(current);
        }
        segments
    }

    fn tokenize_shell_segment(segment: &str) -> Vec<(String, bool)> {
        let mut tokens = Vec::new();
        let mut current = String::new();
        let mut was_quoted = false;
        let mut in_token = false;
        let mut chars = segment.chars().peekable();
        let mut in_single = false;
        let mut in_double = false;
        let mut escaped = false;

        while let Some(c) = chars.next() {
            if escaped {
                current.push(c);
                in_token = true;
                escaped = false;
                continue;
            }
            if c == '\\' && !in_single {
                escaped = true;
                in_token = true;
                continue;
            }
            if c == '\'' && !in_double {
                in_single = !in_single;
                was_quoted = true;
                in_token = true;
                continue;
            }
            if c == '"' && !in_single {
                in_double = !in_double;
                was_quoted = true;
                in_token = true;
                continue;
            }
            if !in_single && !in_double && c.is_whitespace() {
                if in_token {
                    tokens.push((std::mem::take(&mut current), was_quoted));
                    was_quoted = false;
                    in_token = false;
                }
                continue;
            }
            current.push(c);
            in_token = true;
        }
        if in_token {
            tokens.push((current, was_quoted));
        }
        tokens
    }

    fn is_env_assignment(token: &str) -> bool {
        if let Some((lhs, _)) = token.split_once('=') {
            !lhs.is_empty()
                && lhs
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !lhs.chars().next().unwrap_or('0').is_ascii_digit()
        } else {
            false
        }
    }

    fn is_command_wrapper(base: &str) -> bool {
        matches!(
            base,
            "sudo"
                | "env"
                | "nohup"
                | "time"
                | "nice"
                | "command"
                | "exec"
                | "xargs"
                | "uv"
                | "poetry"
                | "pipx"
                | "conda"
        )
    }

    fn is_wrapper_subcommand(wrapper: &str, token: &str) -> bool {
        matches!(
            (wrapper, token),
            ("uv" | "poetry" | "pipx" | "conda", "run")
        )
    }

    fn is_shell_interpreter(base: &str) -> bool {
        matches!(base, "sh" | "bash" | "zsh" | "dash" | "ksh" | "fish")
    }

    fn extract_command_substitutions(cmd: &str) -> Vec<String> {
        let mut subs = Vec::new();
        let mut chars = cmd.chars().peekable();
        let mut in_single = false;
        let mut escaped = false;

        while let Some(c) = chars.next() {
            if escaped {
                escaped = false;
                continue;
            }
            if c == '\\' && !in_single {
                escaped = true;
                continue;
            }
            if c == '\'' {
                in_single = !in_single;
                continue;
            }
            if !in_single && c == '$' && chars.peek() == Some(&'(') {
                let _ = chars.next();
                let mut depth = 1usize;
                let mut inner = String::new();
                while let Some(ic) = chars.next() {
                    if ic == '(' {
                        depth += 1;
                    } else if ic == ')' {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    inner.push(ic);
                }
                if !inner.trim().is_empty() {
                    subs.push(inner);
                }
            } else if !in_single && c == '`' {
                let mut inner = String::new();
                let mut bt_escaped = false;
                while let Some(ic) = chars.next() {
                    if bt_escaped {
                        inner.push(ic);
                        bt_escaped = false;
                        continue;
                    }
                    if ic == '\\' {
                        bt_escaped = true;
                        continue;
                    }
                    if ic == '`' {
                        break;
                    }
                    inner.push(ic);
                }
                if !inner.trim().is_empty() {
                    subs.push(inner);
                }
            }
        }
        subs
    }

    fn unquoted_command_surfaces(cmd: &str) -> Vec<Vec<String>> {
        let mut surfaces = Vec::new();
        for sub in Self::extract_command_substitutions(cmd) {
            surfaces.extend(Self::unquoted_command_surfaces(&sub));
        }
        for seg in Self::split_shell_segments(cmd) {
            let trimmed_seg = seg
                .trim()
                .trim_start_matches('(')
                .trim_end_matches(')')
                .trim();
            let tokens = Self::tokenize_shell_segment(trimmed_seg);
            if tokens.is_empty() {
                continue;
            }
            let mut idx = 0;
            while idx < tokens.len() {
                let (ref tok, quoted) = tokens[idx];
                if !quoted && Self::is_env_assignment(tok) {
                    idx += 1;
                    continue;
                }
                let base = tok.rsplit('/').next().unwrap_or(tok.as_str()).to_lowercase();
                if !quoted && Self::is_command_wrapper(&base) {
                    idx += 1;
                    while idx < tokens.len() && !tokens[idx].1 && tokens[idx].0.starts_with('-') {
                        idx += 1;
                    }
                    if idx < tokens.len()
                        && !tokens[idx].1
                        && Self::is_wrapper_subcommand(&base, &tokens[idx].0.to_lowercase())
                    {
                        idx += 1;
                        while idx < tokens.len()
                            && !tokens[idx].1
                            && tokens[idx].0.starts_with('-')
                        {
                            idx += 1;
                        }
                    }
                    continue;
                }
                break;
            }
            if idx >= tokens.len() {
                continue;
            }

            let head_raw = tokens[idx].0.to_lowercase();
            let head_base = head_raw
                .rsplit('/')
                .next()
                .unwrap_or(head_raw.as_str())
                .to_string();

            if Self::is_shell_interpreter(&head_base) {
                let mut j = idx + 1;
                while j < tokens.len() {
                    let (ref flag, flag_quoted) = tokens[j];
                    if !flag_quoted
                        && (flag == "-c"
                            || flag == "--command"
                            || (flag.starts_with('-')
                                && !flag.starts_with("--")
                                && flag.ends_with('c')))
                    {
                        if let Some((script, _)) = tokens.get(j + 1) {
                            surfaces.extend(Self::unquoted_command_surfaces(script));
                        }
                        break;
                    }
                    j += 1;
                }
            } else if head_base == "eval" {
                let rest: Vec<&str> = tokens[idx + 1..].iter().map(|(t, _)| t.as_str()).collect();
                if !rest.is_empty() {
                    surfaces.extend(Self::unquoted_command_surfaces(&rest.join(" ")));
                }
            }

            let mut surface_tokens = vec![head_base];
            for (tok, quoted) in &tokens[idx + 1..] {
                if !*quoted {
                    surface_tokens.push(tok.to_lowercase());
                }
            }
            surfaces.push(surface_tokens);
        }
        surfaces
    }

    fn shell_surfaces_match_branch(branch_lower: &str, surfaces: &[Vec<String>]) -> bool {
        let trimmed = branch_lower.trim();
        if trimmed.is_empty() {
            return false;
        }
        if trimmed.starts_with('.') && trimmed.len() > 1 {
            return surfaces.iter().any(|tokens| {
                tokens
                    .iter()
                    .any(|tok| Self::path_matches_extension(tok, trimmed))
            });
        }
        if trimmed.starts_with('-') || trimmed.starts_with('/') {
            let needle = format!(" {trimmed} ");
            return surfaces.iter().any(|tokens| {
                let padded = format!(" {} ", tokens.join(" "));
                padded.contains(&needle)
            });
        }
        let prefix = format!("{trimmed} ");
        surfaces.iter().any(|tokens| {
            let joined = format!("{} ", tokens.join(" "));
            joined.starts_with(&prefix)
        })
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
        // Every candidate rule in this namespace and `global`.
        self.semantic
            .candidate_rules()
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
        let validator = GuardValidator::new(store.clone(), Arc::new(StubEmbedder), NS).await;
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

        let validator = GuardValidator::new(store.clone(), Arc::new(StubEmbedder), NS).await;
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

    #[tokio::test(flavor = "multi_thread")]
    async fn global_guard_rule_enforced_in_project_namespace_and_increments_global_hit_count() {
        let store = Arc::new(MockStore::new());
        let global_validator =
            GuardValidator::new(store.clone(), Arc::new(StubEmbedder), "global").await;
        global_validator
            .add_rule(rule(
                "global-no-python",
                "python |python3 |pytest |pip install|.py",
                "RULE: Do not use Python without explicit user confirmation.",
                "active",
            ))
            .await
            .expect("seed global rule");

        let proj_validator =
            GuardValidator::new(store.clone(), Arc::new(StubEmbedder), NS).await;
        let (verdict, fired) = proj_validator
            .validate("bash", "python3 script.py", "/tmp", false)
            .await;

        assert_eq!(fired, vec!["global-no-python".to_string()], "global rule must fire in project namespace");
        match verdict {
            ValidateVerdict::DeterministicReject { constraints, .. } => {
                assert_eq!(
                    constraints,
                    vec!["RULE: Do not use Python without explicit user confirmation.".to_string()]
                );
            }
            other => panic!("expected DeterministicReject, got {:?}", other),
        }
        assert_eq!(
            store.hit_count_of("global", "global-no-python"),
            Some(1),
            "hit_count must increment on the global rule row"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pipe_separated_trigger_patterns_match_any_branch_only() {
        let (_store, validator) = validator_with(vec![rule(
            "no-python",
            "python |python3 |pytest |pip install|.py",
            "Never use Python",
            "active",
        )])
        .await;

        let (v1, f1) = validator
            .validate("write", "scripts/helper.py", "/tmp", false)
            .await;
        assert_eq!(f1, vec!["no-python".to_string()], ".py branch should fire");
        assert!(matches!(v1, ValidateVerdict::DeterministicReject { .. }));

        let (v2, f2) = validator
            .validate("bash", "cargo test --lib", "/tmp", false)
            .await;
        assert!(f2.is_empty(), "unrelated command must not fire");
        assert!(matches!(v2, ValidateVerdict::SandboxPassHighFidelity { .. }));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn guard_rules_survive_when_non_rule_memories_exceed_search_limit() {
        let store = Arc::new(MockStore::new());
        // Seed 10 non-rule memories in NS so `store.search(..., limit=3)` only returns non-rules.
        for i in 0..10 {
            store
                .upsert(
                    NS,
                    &format!("fact-{i}"),
                    vec![0.1, 0.2, 0.3, 0.4],
                    crate::traits::MemoryPayload {
                        content: format!("ordinary fact {i}"),
                        user_id: "u".to_string(),
                        memory_type: "fact".to_string(),
                        agent_name: None,
                        location: String::new(),
                        location_lines: String::new(),
                        metadata: serde_json::json!({}),
                    },
                )
                .await
                .unwrap();
        }
        let validator = GuardValidator::new(store.clone(), Arc::new(StubEmbedder), NS).await;
        validator
            .add_rule(rule("rule.buried", "docker run", "use podman", "active"))
            .await
            .unwrap();

        let (verdict, fired) = validator
            .validate("bash", "docker run alpine", "/tmp", false)
            .await;
        assert_eq!(
            fired.len(),
            1,
            "guard_rule must still be matched even when >3 non-rule memories precede it in search()"
        );
        assert!(matches!(verdict, ValidateVerdict::DeterministicReject { .. }));
    }

    fn no_py_trigger() -> String {
        "python |python3 |pytest |pip install|.py".to_string()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_and_edit_json_payloads_ignore_prose_mentions_of_banned_language() {
        let (_store, validator) = validator_with(vec![rule(
            "global-no-python",
            &no_py_trigger(),
            "Never use Python code",
            "active",
        )])
        .await;

        let write_payload = serde_json::json!({
            "path": "/workspace/README.md",
            "content": "Architecture note: Python is banned; do not run python3, pytest, or pip install, and do not commit .py files."
        })
        .to_string();
        let (v_write, f_write) = validator
            .validate("write", &write_payload, "/workspace", false)
            .await;
        assert!(
            f_write.is_empty(),
            "prose inside write content must not fire command/extension rules: {:?}",
            f_write
        );
        assert!(matches!(v_write, ValidateVerdict::SandboxPassHighFidelity { .. }));

        let edit_payload = serde_json::json!({
            "path": "/workspace/src/guard/validator.rs",
            "oldString": "fn old() {}",
            "newString": "// Rejects actual Python scripts (*.py) and python3 invocations without blocking prose."
        })
        .to_string();
        let (v_edit, f_edit) = validator
            .validate("edit", &edit_payload, "/workspace", false)
            .await;
        assert!(
            f_edit.is_empty(),
            "prose inside edit oldString/newString must not fire: {:?}",
            f_edit
        );
        assert!(matches!(v_edit, ValidateVerdict::SandboxPassHighFidelity { .. }));

        let non_py_ext_payload = serde_json::json!({
            "path": "/workspace/docs/no.python.md",
            "content": "markdown doc"
        })
        .to_string();
        let (_v_doc, f_doc) = validator
            .validate("write", &non_py_ext_payload, "/workspace", false)
            .await;
        assert!(
            f_doc.is_empty(),
            "filename containing .py substring before another extension must not fire: {:?}",
            f_doc
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_and_edit_json_payloads_reject_target_py_files_and_shebangs() {
        let (_store, validator) = validator_with(vec![rule(
            "global-no-python",
            &no_py_trigger(),
            "Never use Python code",
            "active",
        )])
        .await;

        let write_py_json = serde_json::json!({
            "path": "/workspace/scripts/helper.py",
            "content": "x = 1\n"
        })
        .to_string();
        let (v1, f1) = validator
            .validate("write", &write_py_json, "/workspace", false)
            .await;
        assert_eq!(f1, vec!["global-no-python".to_string()]);
        assert!(matches!(v1, ValidateVerdict::DeterministicReject { .. }));

        let edit_py_json = serde_json::json!({
            "file_path": "tools/migrate.py",
            "old_string": "a = 1",
            "new_string": "a = 2"
        })
        .to_string();
        let (v2, f2) = validator
            .validate("Edit", &edit_py_json, "/workspace", false)
            .await;
        assert_eq!(f2, vec!["global-no-python".to_string()]);
        assert!(matches!(v2, ValidateVerdict::DeterministicReject { .. }));

        let write_shebang_json = serde_json::json!({
            "path": "/workspace/scripts/run-tool",
            "content": "#!/usr/bin/env python3\nimport sys\n"
        })
        .to_string();
        let (v3, f3) = validator
            .validate("write", &write_shebang_json, "/workspace", false)
            .await;
        assert_eq!(f3, vec!["global-no-python".to_string()]);
        assert!(matches!(v3, ValidateVerdict::DeterministicReject { .. }));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn shell_json_payloads_ignore_prose_descriptions_and_commit_messages_but_reject_invocations() {
        let (_store, validator) = validator_with(vec![rule(
            "global-no-python",
            &no_py_trigger(),
            "Never use Python code",
            "active",
        )])
        .await;

        let commit_payload = serde_json::json!({
            "command": "git commit -m \"fix(guard): allow Python and .py in prose edits\"",
            "description": "Commit Python guard fix for .py prose"
        })
        .to_string();
        let (v_commit, f_commit) = validator
            .validate("shell", &commit_payload, "/workspace", false)
            .await;
        assert!(
            f_commit.is_empty(),
            "git commit message and tool description must not fire command guard: {:?}",
            f_commit
        );
        assert!(matches!(v_commit, ValidateVerdict::SandboxPassHighFidelity { .. }));

        let invoke_payload = serde_json::json!({
            "command": "cargo test && python3 -m http.server",
            "description": "run server"
        })
        .to_string();
        let (v_invoke, f_invoke) = validator
            .validate("shell", &invoke_payload, "/workspace", false)
            .await;
        assert_eq!(f_invoke, vec!["global-no-python".to_string()]);
        assert!(matches!(v_invoke, ValidateVerdict::DeterministicReject { .. }));

        let subshell_payload = serde_json::json!({
            "command": "sh -c \"pip install requests\"",
            "description": "install package in subshell"
        })
        .to_string();
        let (v_sub, f_sub) = validator
            .validate("bash", &subshell_payload, "/workspace", false)
            .await;
        assert_eq!(f_sub, vec!["global-no-python".to_string()]);
        assert!(matches!(v_sub, ValidateVerdict::DeterministicReject { .. }));

        let cmd_sub_payload = serde_json::json!({
            "command": "echo \"$(python3 -c 'print(1)')\"",
            "description": "command substitution"
        })
        .to_string();
        let (v_csub, f_csub) = validator
            .validate("bash", &cmd_sub_payload, "/workspace", false)
            .await;
        assert_eq!(f_csub, vec!["global-no-python".to_string()]);
        assert!(matches!(v_csub, ValidateVerdict::DeterministicReject { .. }));

        let runner_payload = serde_json::json!({
            "command": "uv run pytest",
            "description": "uv runner"
        })
        .to_string();
        let (v_runner, f_runner) = validator
            .validate("bash", &runner_payload, "/workspace", false)
            .await;
        assert_eq!(f_runner, vec!["global-no-python".to_string()]);
        assert!(matches!(v_runner, ValidateVerdict::DeterministicReject { .. }));

        let search_and_test_payload = serde_json::json!({
            "command": "rg python3 src/ && cargo test global_no_python --nocapture",
            "description": "search and cargo test filter"
        })
        .to_string();
        let (v_search, f_search) = validator
            .validate("bash", &search_and_test_payload, "/workspace", false)
            .await;
        assert!(
            f_search.is_empty(),
            "search arguments and cargo test filters must not fire command guard: {:?}",
            f_search
        );
        assert!(matches!(v_search, ValidateVerdict::SandboxPassHighFidelity { .. }));
    }
}
