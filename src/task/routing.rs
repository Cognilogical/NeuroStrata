//! Canonical agent routing contract (design spec §6 R4).
//!
//! This module is pure data plus deterministic evaluation: no store access, no
//! I/O, no clock, no randomness. `agent_routing_contract_json()` is the
//! canonical R4 table; tests check the static prompt sites stay in sync with
//! it (`GRAPH_CONTRACT_TRACKED_SITES` in `tests/search/upgraded_search_test.rs`
//! plus a contract/site parity test) because the harness prompt files are
//! authored prose, not generated from this table. `build_task_graph_context`
//! calls `evaluate_triggers` for task-level triggers. When no condition
//! matches it returns an empty vector: abstention emits no action and no
//! warning (R4.4).

use serde_json::{json, Value};

/// Version of the wire shape emitted by `agent_routing_contract_json`.
pub const CONTRACT_VERSION: &str = "1.0";

/// R4: an AST blast radius at or above this value needs a two-hop expansion.
pub const HIGH_BLAST_THRESHOLD: usize = 15;

/// R4: at least this many staged files (or a PR review scope) needs a diff tour.
pub const DIFF_STAGED_FILE_THRESHOLD: usize = 3;

/// R4: recommended advisor breadth for an explicit strategic request.
pub const STRATEGIC_ADVISE_TOP_K: u64 = 5;

/// Fallback read-only query for a cross-row invariant that bounded traversal
/// cannot answer. Callers may replace it through `TaskContext::invariant_query`.
pub const SPARQL_INVARIANT_QUERY_TEMPLATE: &str = "SELECT ?s ?p ?o WHERE { ?s ?p ?o } LIMIT 25";

/// Single source for `expected_decision`: the canonical contract JSON and
/// `next_action` both render these constants, so they cannot drift apart.
pub const EXPECTED_DECISION_T_PATH: &str =
    "Apply returned governance before editing; an empty lookup routes to T_AMBIGUOUS.";
pub const EXPECTED_DECISION_T_AMBIGUOUS: &str =
    "Pick a localized symbol/path, then re-run the path lookup.";
pub const EXPECTED_DECISION_T_HIGH_BLAST: &str =
    "Expand two hops; re-plan edits around unchanged callers before proceeding.";
pub const EXPECTED_DECISION_T_DIFF: &str =
    "Review by dependency stop; audit unchanged callers and governing rules.";
pub const EXPECTED_DECISION_T_DOMAIN_NEW: &str =
    "Ground new domain terms from briefing then gaps before naming concepts.";
pub const EXPECTED_DECISION_T_STRATEGIC: &str =
    "Judge ranked advice against implemented interfaces; record accepted or rejected items.";
pub const EXPECTED_DECISION_T_INVARIANT: &str =
    "Decide from the read-only result; never mutate memory from this query.";
// Review IMPORTANT-2: the two response-derived seed next-hops are real
// triggers, not UNKNOWN. Their decisions stay neutral: a real edge or bridge is
// evidence to decide from, never proof that the seed is already resolved.
pub const EXPECTED_DECISION_T_SEED_EDGE: &str =
    "A real conceptual-bridge edge is evidence; decide from it, abstain when it adds nothing.";
pub const EXPECTED_DECISION_T_SEED_EXPAND: &str =
    "Expand the edge-bearing seed two hops; decide from real edges, never a fabricated one.";
pub const EXPECTED_DECISION_UNKNOWN: &str =
    "Use the result to decide the next step; abstain when it adds no evidence.";

/// The signals a caller feeds to the router. Every field is input-only; the
/// router never reads the store or infers values it was not given.
///
/// - `known_path`: a concrete file path the agent is about to edit whose
///   governance has not been looked up yet (drives `T_PATH`).
/// - `ambiguous_query`: an unknown path/symbol, or a file lookup that came back
///   empty (drives `T_AMBIGUOUS`).
/// - `blast_radius_count`: largest observed `ast_summary.blast_radius_count`.
/// - `blast_id`: seed ID for the expansion; falls back to `known_path` because
///   `neurostrata_related` accepts a qualified file path as a seed.
/// - `staged_file_count`: files in the current scope; `0` means scope absent.
/// - `diff_text`: caller-supplied unified diff for `diff_tour` (the tool's
///   declared input); absent means the emitted `diff_text` is empty and must be
///   filled by the caller before dispatch.
/// - `git_ref`: scope hint only, rendered in `reason`; `diff_tour` declares no
///   `git_ref` argument. Defaults to `"HEAD"` when absent.
/// - `task_type`: task row type (`task`/`bug`/`feature`/`epic`).
/// - `design_review`, `strategic`, `cross_row_invariant`: explicit caller flags.
/// - `invariant_query`: optional SPARQL replacement for the template above.
#[derive(Debug, Clone)]
pub struct TaskContext {
    pub namespace: String,
    pub known_path: Option<String>,
    pub ambiguous_query: Option<String>,
    pub blast_radius_count: usize,
    pub blast_id: Option<String>,
    pub staged_file_count: usize,
    pub diff_text: Option<String>,
    pub git_ref: Option<String>,
    pub task_type: String,
    pub design_review: bool,
    pub strategic: bool,
    pub cross_row_invariant: bool,
    pub invariant_query: Option<String>,
}

/// One deterministic route. `id` and `tool` are static so a contract test can
/// pin them byte-for-byte; `args` are ready to paste into an MCP tool call.
#[derive(Debug, Clone)]
pub struct RouteTrigger {
    pub id: &'static str,
    pub tool: &'static str,
    pub args: Value,
    pub reason: String,
}

/// Canonical R4 table. `required_args` and `args` are rendered together so a
/// missing key is a contract error rather than a runtime surprise.
pub fn agent_routing_contract_json() -> Value {
    json!({
        "contract_version": CONTRACT_VERSION,
        "abstention": "no trigger matches -> no action and no warning",
        "triggers": [
            {
                "trigger_id": "T_PATH",
                "condition": "known path before edit",
                "tool": "neurostrata_memories_for_file",
                "required_args": ["namespace", "path", "include_ast_edges"],
                "args": {
                    "namespace": "<namespace>",
                    "path": "<path>",
                    "include_ast_edges": true
                },
                "follow_up_operations": [],
                "expected_decision": EXPECTED_DECISION_T_PATH
            },
            {
                "trigger_id": "T_AMBIGUOUS",
                "condition": "unknown path/symbol or empty file lookup",
                "tool": "neurostrata_locate",
                "required_args": ["namespace", "query"],
                "args": {
                    "namespace": "<namespace>",
                    "query": "<symbol-or-path>"
                },
                "follow_up_operations": [],
                "expected_decision": EXPECTED_DECISION_T_AMBIGUOUS
            },
            {
                "trigger_id": "T_HIGH_BLAST",
                "condition": "blast_radius_count >= 15",
                "tool": "neurostrata_related",
                "required_args": ["namespace", "id", "max_hops"],
                "args": {
                    "namespace": "<namespace>",
                    "id": "<symbol-or-file-id>",
                    "max_hops": 2,
                    "followed_trigger_id": "T_HIGH_BLAST"
                },
                "follow_up_operations": [],
                "expected_decision": EXPECTED_DECISION_T_HIGH_BLAST
            },
            {
                "trigger_id": "T_DIFF",
                "condition": ">=3 staged files / PR review",
                "tool": "neurostrata_diff_tour",
                "required_args": ["namespace", "diff_text"],
                "args": {
                    "namespace": "<namespace>",
                    "diff_text": "<unified-diff>"
                },
                "follow_up_operations": [],
                "expected_decision": EXPECTED_DECISION_T_DIFF
            },
            {
                "trigger_id": "T_DOMAIN_NEW",
                "condition": "task_type feature/epic or design review",
                "tool": "neurostrata_taxonomy",
                "required_args": ["namespace", "operation"],
                "args": {
                    "namespace": "<namespace>",
                    "operation": "briefing"
                },
                "follow_up_operations": ["gaps"],
                "expected_decision": EXPECTED_DECISION_T_DOMAIN_NEW
            },
            {
                "trigger_id": "T_STRATEGIC",
                "condition": "explicit strategic-improvement request",
                "tool": "neurostrata_taxonomy",
                "required_args": ["namespace", "operation", "top_k"],
                "args": {
                    "namespace": "<namespace>",
                    "operation": "advise",
                    "top_k": STRATEGIC_ADVISE_TOP_K
                },
                "follow_up_operations": [],
                "expected_decision": EXPECTED_DECISION_T_STRATEGIC
            },
            {
                "trigger_id": "T_INVARIANT",
                "condition": "cross-row invariant not answered by bounded traversal",
                "tool": "neurostrata_sparql_query",
                "required_args": ["namespace", "query"],
                "args": {
                    "namespace": "<namespace>",
                    "query": SPARQL_INVARIANT_QUERY_TEMPLATE
                },
                "follow_up_operations": [],
                "expected_decision": EXPECTED_DECISION_T_INVARIANT
            }
        ]
    })
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|v| !v.trim().is_empty())
}

fn is_new_domain_task(task_type: &str) -> bool {
    let normalized = task_type.trim().to_ascii_lowercase();
    normalized == "feature" || normalized == "epic"
}

/// Evaluate the R4 table against caller-supplied signals. Trigger order is the
/// table order so prompt builders emit a stable sequence.
pub fn evaluate_triggers(ctx: TaskContext) -> Vec<RouteTrigger> {
    let mut routes = Vec::new();
    let namespace = ctx.namespace.clone();

    if let Some(path) = non_empty(ctx.known_path.as_deref()) {
        routes.push(RouteTrigger {
            id: "T_PATH",
            tool: "neurostrata_memories_for_file",
            args: json!({
                "namespace": namespace,
                "path": path,
                "include_ast_edges": true
            }),
            reason: format!("Known path '{path}' before edit: look up governing memories first."),
        });
    }

    if let Some(query) = non_empty(ctx.ambiguous_query.as_deref()) {
        routes.push(RouteTrigger {
            id: "T_AMBIGUOUS",
            tool: "neurostrata_locate",
            args: json!({ "namespace": namespace, "query": query }),
            reason: format!(
                "Unknown path/symbol or empty lookup '{query}': localize before editing."
            ),
        });
    }

    if ctx.blast_radius_count >= HIGH_BLAST_THRESHOLD {
        // A file path is also a valid `related` seed, so a caller that only
        // knows the path still gets a schema-valid two-hop expansion.
        if let Some(id) =
            non_empty(ctx.blast_id.as_deref()).or_else(|| non_empty(ctx.known_path.as_deref()))
        {
            routes.push(RouteTrigger {
                id: "T_HIGH_BLAST",
                tool: "neurostrata_related",
                args: json!({
                    "namespace": namespace,
                    "id": id,
                    "max_hops": 2,
                    "followed_trigger_id": "T_HIGH_BLAST"
                }),
                reason: format!(
                    "Blast radius {} >= {HIGH_BLAST_THRESHOLD}: expand two hops from '{id}'.",
                    ctx.blast_radius_count
                ),
            });
        }
    }

    if ctx.staged_file_count >= DIFF_STAGED_FILE_THRESHOLD {
        let git_ref = non_empty(ctx.git_ref.as_deref()).unwrap_or("HEAD");
        let diff_text = ctx.diff_text.clone().unwrap_or_default();
        routes.push(RouteTrigger {
            id: "T_DIFF",
            tool: "neurostrata_diff_tour",
            args: json!({ "namespace": namespace, "diff_text": diff_text }),
            reason: format!(
                "{} staged files >= {DIFF_STAGED_FILE_THRESHOLD}: take the dependency-ordered diff tour (scope ref '{git_ref}'; diff_tour consumes diff_text, not a ref).",
                ctx.staged_file_count
            ),
        });
    }

    if is_new_domain_task(&ctx.task_type) || ctx.design_review {
        routes.push(RouteTrigger {
            id: "T_DOMAIN_NEW",
            tool: "neurostrata_taxonomy",
            args: json!({ "namespace": namespace, "operation": "briefing" }),
            reason: format!(
                "Task type '{}' or design review: read the taxonomy briefing, then gaps.",
                ctx.task_type
            ),
        });
    }

    if ctx.strategic {
        routes.push(RouteTrigger {
            id: "T_STRATEGIC",
            tool: "neurostrata_taxonomy",
            args: json!({
                "namespace": namespace,
                "operation": "advise",
                "top_k": STRATEGIC_ADVISE_TOP_K
            }),
            reason:
                "Explicit strategic-improvement request: ask the SME advisor for ranked advice."
                    .to_string(),
        });
    }

    if ctx.cross_row_invariant {
        let query = non_empty(ctx.invariant_query.as_deref())
            .unwrap_or(SPARQL_INVARIANT_QUERY_TEMPLATE)
            .to_string();
        routes.push(RouteTrigger {
            id: "T_INVARIANT",
            tool: "neurostrata_sparql_query",
            args: json!({ "namespace": namespace, "query": query }),
            reason: "Cross-row invariant outside bounded traversal: use a read-only SPARQL query."
                .to_string(),
        });
    }

    routes
}

/// FNV-1a over the emitted args plus returned IDs. Stable across runs and
/// machines, so an action ID can be correlated with the receipt it produced.
fn stable_digest(args: &Value, ids: &[String]) -> String {
    let mut payload = args.to_string();
    for id in ids {
        payload.push('\u{1f}');
        payload.push_str(id);
    }
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in payload.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Pure action-id formula, shared with dispatch-time proof generation.
///
/// The ledger stores digests only, so depth/seed of a prescribed call cannot be
/// recovered from a receipt. A follow-up dispatch recomputes this id from the
/// same arg shape + ids as the prescription; their equality is the proof.
pub fn action_id(trigger_id: &str, args: &Value, ids: &[String]) -> String {
    format!("{trigger_id}:{}", stable_digest(args, ids))
}

/// Resolve the single-source expected-decision constant for a trigger ID.
pub fn expected_decision(trigger_id: &str) -> &'static str {
    match trigger_id {
        "T_PATH" => EXPECTED_DECISION_T_PATH,
        "T_AMBIGUOUS" => EXPECTED_DECISION_T_AMBIGUOUS,
        "T_HIGH_BLAST" => EXPECTED_DECISION_T_HIGH_BLAST,
        "T_DIFF" => EXPECTED_DECISION_T_DIFF,
        "T_DOMAIN_NEW" => EXPECTED_DECISION_T_DOMAIN_NEW,
        "T_STRATEGIC" => EXPECTED_DECISION_T_STRATEGIC,
        "T_INVARIANT" => EXPECTED_DECISION_T_INVARIANT,
        "T_SEED_EDGE" => EXPECTED_DECISION_T_SEED_EDGE,
        "T_SEED_EXPAND" => EXPECTED_DECISION_T_SEED_EXPAND,
        _ => EXPECTED_DECISION_UNKNOWN,
    }
}

/// Materialize a trigger as the additive §5 `next_action` shape.
///
/// `ids` are IDs returned by a prior lookup. They stabilize `action_id` across
/// identical triggers on different seeds and supply `id` for `related` when
/// the trigger itself did not carry one. Callers cap the resulting list at
/// three actions per response (§5).
pub fn next_action(trigger: &RouteTrigger, ids: &[String]) -> Value {
    let mut args = trigger.args.clone();
    if trigger.tool == "neurostrata_related" {
        let missing_id = args
            .get("id")
            .and_then(Value::as_str)
            .map_or(true, str::is_empty);
        if missing_id {
            if let Some(seed) = ids.iter().find(|id| !id.is_empty()) {
                args["id"] = Value::String(seed.clone());
            }
        }
    }
    let action_id = action_id(trigger.id, &args, ids);
    json!({
        "action_id": action_id,
        "trigger_id": trigger.id,
        "tool": trigger.tool,
        "args": args,
        "reason": &trigger.reason,
        "expected_decision": expected_decision(trigger.id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing_action_id_matches_prescription_shape() {
        let ns = "NeuroStrata";
        let seed = "mem-seed-42";
        let trigger = RouteTrigger {
            id: "T_HIGH_BLAST",
            tool: "neurostrata_related",
            args: json!({ "namespace": ns, "id": seed, "max_hops": 2 }),
            reason: String::new(),
        };
        let prescribed = next_action(&trigger, &[seed.to_string()]);
        let proof_args = json!({ "namespace": ns, "id": seed, "max_hops": 2 });
        assert_eq!(
            prescribed["action_id"],
            action_id("T_HIGH_BLAST", &proof_args, &[seed.to_string()]),
            "dispatch proof must equal the prescribed action id byte-for-byte"
        );
        assert_ne!(
            prescribed["action_id"],
            action_id(
                "T_HIGH_BLAST",
                &json!({ "namespace": ns, "id": seed, "max_hops": 1 }),
                &[seed.to_string()]
            ),
            "a one-hop call must not reproduce the prescribed proof"
        );
        assert_ne!(
            prescribed["action_id"],
            action_id("T_HIGH_BLAST", &proof_args, &["other-seed".to_string()]),
            "a different seed must not reproduce the prescribed proof"
        );
    }

    // Review IMPORTANT-2: response-derived seed next-hops are real triggers and
    // must render an honest neutral decision, never EXPECTED_DECISION_UNKNOWN.
    #[test]
    fn seed_triggers_have_honest_expected_decisions() {
        for id in ["T_SEED_EDGE", "T_SEED_EXPAND"] {
            let decision = expected_decision(id);
            assert_ne!(
                decision, EXPECTED_DECISION_UNKNOWN,
                "{id} must not render EXPECTED_DECISION_UNKNOWN"
            );
            assert!(!decision.is_empty(), "{id} decision must be non-empty");
        }
        // The seven canonical triggers and the honest fallback are unchanged.
        assert_eq!(
            expected_decision("T_HIGH_BLAST"),
            EXPECTED_DECISION_T_HIGH_BLAST
        );
        assert_eq!(
            expected_decision("T_NOT_A_TRIGGER"),
            EXPECTED_DECISION_UNKNOWN
        );
    }
}
