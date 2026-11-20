//! Agent graph/ontology adoption contract tests.
//!
//! The crate ships a binary only (see `Cargo.toml`), so this test compiles the
//! crate-free `task::routing` module from its real path -- the same pattern
//! `tests/graph_read/memories_for_file_test.rs` uses for its ranking helpers.

#[path = "../src/graph_read/memories_for_file.rs"]
mod memories_for_file;
#[path = "../src/task/routing.rs"]
mod routing;
#[path = "../src/task/snapshot_readiness.rs"]
mod snapshot_readiness;

use routing::{agent_routing_contract_json, evaluate_triggers, TaskContext};
use serde_json::Value;

const TRIGGER_TOOLS: [(&str, &str); 7] = [
    ("T_PATH", "neurostrata_memories_for_file"),
    ("T_AMBIGUOUS", "neurostrata_locate"),
    ("T_HIGH_BLAST", "neurostrata_related"),
    ("T_DIFF", "neurostrata_diff_tour"),
    ("T_DOMAIN_NEW", "neurostrata_taxonomy"),
    ("T_STRATEGIC", "neurostrata_taxonomy"),
    ("T_INVARIANT", "neurostrata_sparql_query"),
];
const TRIGGER_IDS: [&str; 7] = [
    "T_PATH",
    "T_AMBIGUOUS",
    "T_HIGH_BLAST",
    "T_DIFF",
    "T_DOMAIN_NEW",
    "T_STRATEGIC",
    "T_INVARIANT",
];

/// Spec §6 R4: exactly seven triggers, each naming a real MCP tool and
/// carrying schema-valid arguments for that tool.
#[test]
fn routing_contract_has_all_seven_triggers() {
    let contract = agent_routing_contract_json();
    let triggers = contract
        .get("triggers")
        .and_then(Value::as_array)
        .expect("contract must expose a triggers array");
    assert_eq!(triggers.len(), 7, "R4 defines exactly seven triggers");

    for (trigger_id, tool) in TRIGGER_TOOLS {
        let trigger = triggers
            .iter()
            .find(|t| t.get("trigger_id").and_then(Value::as_str) == Some(trigger_id))
            .unwrap_or_else(|| panic!("missing trigger {trigger_id} in canonical contract"));
        assert_eq!(
            trigger.get("tool").and_then(Value::as_str),
            Some(tool),
            "{trigger_id} must route to {tool}"
        );

        let required = trigger
            .get("required_args")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("{trigger_id} must declare required_args"));
        let args = trigger
            .get("args")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("{trigger_id} must declare an args object"));
        for key in required {
            let key = key
                .as_str()
                .unwrap_or_else(|| panic!("{trigger_id} required_args must be strings"));
            assert!(
                args.contains_key(key),
                "{trigger_id} args missing required key {key}"
            );
        }
        assert!(
            args.contains_key("namespace"),
            "{trigger_id} args must carry a namespace"
        );
    }

    // Exact values from the R4 table that downstream tasks depend on.
    let by_id = |id: &str| -> &Value {
        triggers
            .iter()
            .find(|t| t.get("trigger_id").and_then(Value::as_str) == Some(id))
            .unwrap()
    };
    assert_eq!(by_id("T_PATH")["args"]["include_ast_edges"], true);
    assert_eq!(by_id("T_HIGH_BLAST")["args"]["max_hops"], 2);
    assert_eq!(by_id("T_DOMAIN_NEW")["args"]["operation"], "briefing");
    assert_eq!(by_id("T_DOMAIN_NEW")["follow_up_operations"][0], "gaps");
    assert_eq!(by_id("T_STRATEGIC")["args"]["operation"], "advise");
    assert_eq!(by_id("T_STRATEGIC")["args"]["top_k"], 5);
}

/// Review Focus item 5: a routine single-file, non-strategic edit must not
/// manufacture graph/ontology work. No matching trigger means abstention with
/// no action and no warning.
#[test]
fn routing_abstains_on_routine_single_file_edit() {
    let context = TaskContext {
        namespace: "NeuroStrata".to_string(),
        known_path: None,
        ambiguous_query: None,
        blast_radius_count: 0,
        blast_id: None,
        staged_file_count: 1,
        diff_text: None,
        git_ref: None,
        task_type: "bug".to_string(),
        design_review: false,
        strategic: false,
        cross_row_invariant: false,
        invariant_query: None,
    };
    let triggers = evaluate_triggers(context);
    assert!(
        triggers.is_empty(),
        "routine single-file edit must abstain, got {triggers:?}"
    );
}

/// Every signal set emits the seven R4 triggers in table order.
#[test]
fn routing_evaluates_all_triggers_in_table_order() {
    let context = TaskContext {
        namespace: "NeuroStrata".to_string(),
        known_path: Some("src/task/routing.rs".to_string()),
        ambiguous_query: Some("resolve_session_root_dir".to_string()),
        blast_radius_count: 15,
        blast_id: Some("mem-42".to_string()),
        staged_file_count: 3,
        diff_text: Some("diff --git a/x.rs b/x.rs\n".to_string()),
        git_ref: Some("fix/routing".to_string()),
        task_type: "feature".to_string(),
        design_review: false,
        strategic: true,
        cross_row_invariant: true,
        invariant_query: None,
    };
    let ids: Vec<&str> = evaluate_triggers(context).iter().map(|t| t.id).collect();
    assert_eq!(
        ids,
        vec![
            "T_PATH",
            "T_AMBIGUOUS",
            "T_HIGH_BLAST",
            "T_DIFF",
            "T_DOMAIN_NEW",
            "T_STRATEGIC",
            "T_INVARIANT",
        ]
    );
}

/// §5 next_action shape is additive and stable; returned IDs bind id-based
/// tools when the trigger did not carry a seed.
#[test]
fn routing_next_action_shape_is_deterministic_and_binds_ids() {
    use routing::{next_action, RouteTrigger};
    use serde_json::json;

    let trigger = RouteTrigger {
        id: "T_HIGH_BLAST",
        tool: "neurostrata_related",
        args: json!({ "namespace": "NeuroStrata", "id": "", "max_hops": 2 }),
        reason: "blast radius".to_string(),
    };
    let ids = vec!["mem-42".to_string()];
    let action = next_action(&trigger, &ids);

    let mut keys: Vec<&str> = action
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "action_id",
            "args",
            "expected_decision",
            "reason",
            "tool",
            "trigger_id",
        ]
    );
    assert_eq!(action["trigger_id"], "T_HIGH_BLAST");
    assert_eq!(action["tool"], "neurostrata_related");
    assert_eq!(action["args"]["id"], "mem-42");
    assert!(action["action_id"]
        .as_str()
        .unwrap()
        .starts_with("T_HIGH_BLAST:"));
    assert!(action["expected_decision"].as_str().unwrap().len() > 10);
    assert_eq!(action, next_action(&trigger, &ids));
    assert_ne!(
        action["action_id"],
        next_action(&trigger, &["mem-99".to_string()])["action_id"]
    );
}

/// Finding 1: T_DIFF args must contain only properties declared by the live
/// `neurostrata_diff_tour` inputSchema. `git_ref` is a reason-only scope hint.
#[test]
fn routing_diff_trigger_args_match_diff_tour_schema() {
    const DECLARED: &[&str] = &[
        "namespace",
        "diff_text",
        "changed_spans",
        "project_root",
        "max_blast_radius_callers",
        "principal",
    ];

    let contract = agent_routing_contract_json();
    let template = contract["triggers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["trigger_id"] == "T_DIFF")
        .unwrap();
    let template_args = template["args"].as_object().unwrap();
    for key in template_args.keys() {
        assert!(
            DECLARED.contains(&key.as_str()),
            "contract T_DIFF leaks undeclared arg {key}"
        );
    }
    assert!(template_args.contains_key("diff_text"));
    let required: Vec<&str> = template["required_args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(required.contains(&"diff_text"));
    assert!(!required.contains(&"git_ref"));

    let diff = "diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n".to_string();
    let context = TaskContext {
        namespace: "NeuroStrata".to_string(),
        known_path: None,
        ambiguous_query: None,
        blast_radius_count: 0,
        blast_id: None,
        staged_file_count: 3,
        diff_text: Some(diff.clone()),
        git_ref: Some("fix/routing".to_string()),
        task_type: "task".to_string(),
        design_review: false,
        strategic: false,
        cross_row_invariant: false,
        invariant_query: None,
    };
    let emitted = evaluate_triggers(context)
        .into_iter()
        .find(|t| t.id == "T_DIFF")
        .expect("T_DIFF must fire at three staged files");
    for key in emitted.args.as_object().unwrap().keys() {
        assert!(
            DECLARED.contains(&key.as_str()),
            "emitted T_DIFF leaks undeclared arg {key}"
        );
    }
    assert_eq!(emitted.args["diff_text"], diff);
    assert!(emitted.args.get("git_ref").is_none());
    assert!(emitted.reason.contains("fix/routing"));
}

/// Finding 2: `expected_decision` has one source. Both the resolver and the
/// emitted `next_action` must match the canonical contract for every trigger.
#[test]
fn routing_expected_decision_single_source_matches_next_action() {
    let contract = agent_routing_contract_json();
    let triggers = contract["triggers"].as_array().unwrap();
    for id in TRIGGER_IDS {
        let entry = triggers
            .iter()
            .find(|t| t["trigger_id"] == id)
            .unwrap_or_else(|| panic!("missing trigger {id}"));
        let expected = entry["expected_decision"].as_str().unwrap();
        assert_eq!(
            routing::expected_decision(id),
            expected,
            "{id}: resolver drifted from contract"
        );
        let trigger = routing::RouteTrigger {
            id,
            tool: "neurostrata_locate",
            args: serde_json::json!({ "namespace": "NeuroStrata" }),
            reason: "parity check".to_string(),
        };
        assert_eq!(
            routing::next_action(&trigger, &[])["expected_decision"],
            expected,
            "{id}: next_action drifted from contract"
        );
    }
}

/// Controller drift guard: the crate-free escalation module duplicates values
/// from `routing` because it cannot import crate code. Pin every duplicate to
/// the canonical contract so a future edit cannot silently diverge.
#[test]
fn memories_for_file_escalation_constants_match_routing_contract() {
    let contract = agent_routing_contract_json();
    let triggers = contract["triggers"].as_array().unwrap();
    let trigger = |id: &str| -> &Value {
        triggers
            .iter()
            .find(|t| t["trigger_id"] == id)
            .unwrap_or_else(|| panic!("missing trigger {id}"))
    };

    // Thresholds (and the §5 action cap) have a single numeric source.
    assert_eq!(
        memories_for_file::HIGH_BLAST_THRESHOLD,
        routing::HIGH_BLAST_THRESHOLD
    );
    assert_eq!(memories_for_file::HIGH_BLAST_THRESHOLD, 15);
    assert_eq!(
        memories_for_file::DIFF_STAGED_FILE_THRESHOLD,
        routing::DIFF_STAGED_FILE_THRESHOLD
    );
    assert_eq!(memories_for_file::DIFF_STAGED_FILE_THRESHOLD, 3);
    assert_eq!(memories_for_file::MAX_NEXT_ACTIONS, 3);
    assert_eq!(trigger("T_HIGH_BLAST")["args"]["max_hops"], 2);

    // Expected-decision strings: three canonical triggers plus the seed-edge
    // fallback, which deliberately resolves to routing's unknown case.
    for (id, local) in [
        ("T_AMBIGUOUS", memories_for_file::EXPECTED_AMBIGUOUS),
        ("T_HIGH_BLAST", memories_for_file::EXPECTED_HIGH_BLAST),
        ("T_DIFF", memories_for_file::EXPECTED_DIFF),
    ] {
        assert_eq!(
            local,
            trigger(id)["expected_decision"].as_str().unwrap(),
            "{id}: duplicated expected_decision drifted"
        );
    }
    assert_eq!(
        memories_for_file::EXPECTED_SEED_EDGE,
        routing::EXPECTED_DECISION_UNKNOWN
    );
}

/// Spec §6 R5 / Review Focus item 3: ReBAC-restricted taxonomy rows must never
/// reach readiness totals. Seed one restricted + one unrestricted concept; the
/// report may count only the unrestricted one and must not name the restricted
/// slug anywhere in the rendered section.
#[test]
fn snapshot_readiness_excludes_restricted_taxonomy() {
    use snapshot_readiness::{
        compute_readiness, render_readiness_section, ReadinessConcept, ReadinessInput,
        MAX_RECOMMENDED_READS, SECTION_HEADING,
    };

    let input = ReadinessInput {
        concepts: vec![
            ReadinessConcept {
                slug: "restricted-secret".to_string(),
                restricted: true,
                epistemic_status: "verified".to_string(),
            },
            ReadinessConcept {
                slug: "public-anchor".to_string(),
                restricted: false,
                epistemic_status: "seed".to_string(),
            },
        ],
        graph_tool_names: vec!["neurostrata_memories_for_file".to_string()],
        has_thesis: false,
    };

    let report = compute_readiness(&input);
    assert_eq!(
        report.taxonomy_concepts_count, 1,
        "only the unrestricted concept may be counted"
    );
    assert_eq!(
        report.ungrounded_concepts_count, 1,
        "the unrestricted seed concept is ungrounded"
    );
    assert_eq!(report.active_task_graph_tool_diversity, 1);
    assert!(report.recommended_reads.len() <= MAX_RECOMMENDED_READS);

    let section = render_readiness_section(&input);
    assert!(section.contains(SECTION_HEADING));
    assert!(section.contains("taxonomy_concepts_count: 1"));
    assert!(section.contains("ungrounded_concepts_count: 1"));
    assert!(section.contains("active_task_graph_tool_diversity: 1"));
    assert!(
        !section.contains("restricted-secret"),
        "restricted slug leaked into snapshot readiness"
    );
}

/// R5.2: zero thesis/concept rows emit exactly the prescribed hint line.
#[test]
fn snapshot_readiness_zero_concept_hint_is_exact() {
    use snapshot_readiness::{render_readiness_section, ReadinessInput, ZERO_CONCEPT_HINT};

    let empty = ReadinessInput {
        concepts: vec![],
        graph_tool_names: vec![],
        has_thesis: false,
    };
    let section = render_readiness_section(&empty);
    assert!(
        section.lines().any(|line| line == ZERO_CONCEPT_HINT),
        "zero-concept hint must appear verbatim as its own line; got:\n{section}"
    );
    assert!(section.contains("taxonomy_concepts_count: 0"));

    // A thesis with no concepts still reports zero concepts, but the
    // set_thesis hint only applies when there is no thesis either.
    let thesis_only = ReadinessInput {
        concepts: vec![],
        graph_tool_names: vec![],
        has_thesis: true,
    };
    assert!(!render_readiness_section(&thesis_only).contains(ZERO_CONCEPT_HINT));
}

/// R5.1: graph tool diversity is distinct-count, and recommended_reads is
/// capped at three and deterministic.
#[test]
fn snapshot_readiness_diversity_distinct_and_reads_capped() {
    use snapshot_readiness::{
        compute_readiness, render_readiness_section, ReadinessConcept, ReadinessInput,
        MAX_RECOMMENDED_READS,
    };

    let input = ReadinessInput {
        concepts: vec![
            ReadinessConcept {
                slug: "a".to_string(),
                restricted: false,
                epistemic_status: "seed".to_string(),
            },
            ReadinessConcept {
                slug: "a".to_string(),
                restricted: false,
                epistemic_status: "verified".to_string(),
            },
            ReadinessConcept {
                slug: "b".to_string(),
                restricted: false,
                epistemic_status: "verified".to_string(),
            },
        ],
        graph_tool_names: vec![
            "neurostrata_related".to_string(),
            "neurostrata_related".to_string(),
            "neurostrata_sparql_query".to_string(),
            "  ".to_string(),
        ],
        has_thesis: true,
    };
    let report = compute_readiness(&input);
    assert_eq!(report.taxonomy_concepts_count, 2, "concepts dedupe by slug");
    assert_eq!(
        report.active_task_graph_tool_diversity, 2,
        "tool names dedupe"
    );
    assert_eq!(report.recommended_reads.len(), 2, "briefing + gaps");
    assert!(report.recommended_reads.len() <= MAX_RECOMMENDED_READS);
    assert_eq!(
        render_readiness_section(&input),
        render_readiness_section(&input)
    );

    // Low tool diversity adds a third read and the cap holds at exactly three.
    let low_diversity = ReadinessInput {
        graph_tool_names: vec!["neurostrata_related".to_string()],
        ..input
    };
    let capped = compute_readiness(&low_diversity);
    assert_eq!(capped.recommended_reads.len(), MAX_RECOMMENDED_READS);
    assert!(capped.recommended_reads.iter().any(|r| r.contains("gaps")));
    assert!(capped
        .recommended_reads
        .iter()
        .any(|r| r.contains("memories_for_file")));
}

/// Fix round 1 / finding 1: legacy `queries[]` rows are excluded from quality
/// metrics (spec R6). A legacy-only ledger, or one whose attributed events
/// carry no allowlisted graph tool, must yield diversity 0 even when legacy
/// rows name allowlisted tools or synthetic `pre-commit` rows exist.
#[test]
fn snapshot_readiness_legacy_ledger_rows_never_count() {
    use snapshot_readiness::{active_task_ids, graph_tools_from_ledger};

    let active = active_task_ids(&[("t1".to_string(), "in_progress".to_string())]);
    let legacy_only = serde_json::json!({
        "queries": [
            { "tool": "neurostrata_related", "task_id": "t1" },
            { "tool": "neurostrata_sparql_query", "task_id": "t1" },
            { "tool": "pre-commit", "query": "x.rs" }
        ]
    });
    assert!(
        graph_tools_from_ledger(&legacy_only, &active).is_empty(),
        "legacy queries[] must never contribute to diversity"
    );

    let non_graph_events = serde_json::json!({
        "events": [{ "task_id": "t1", "tool": "neurostrata_add_memory" }],
        "queries": [{ "tool": "neurostrata_related" }]
    });
    assert!(
        graph_tools_from_ledger(&non_graph_events, &active).is_empty(),
        "no fallback to legacy rows when attributed events yield no graph tool"
    );
}

/// Fix round 1 / finding 2: only open/in_progress/blocked tasks are in flight.
/// A done task's attributed receipt must not count; unknown statuses neither.
#[test]
fn snapshot_readiness_done_task_receipts_excluded() {
    use snapshot_readiness::{active_task_ids, graph_tools_from_ledger, ACTIVE_TASK_STATUSES};

    assert_eq!(ACTIVE_TASK_STATUSES, &["open", "in_progress", "blocked"]);
    let ids = active_task_ids(&[
        ("done-task".to_string(), "done".to_string()),
        ("open-task".to_string(), "open".to_string()),
        ("inprog-task".to_string(), "in_progress".to_string()),
        ("blocked-task".to_string(), "blocked".to_string()),
        ("weird-task".to_string(), "archived".to_string()),
    ]);
    assert!(!ids.contains("done-task"));
    assert!(!ids.contains("weird-task"));
    assert!(ids.contains("open-task"));
    assert!(ids.contains("inprog-task"));
    assert!(ids.contains("blocked-task"));

    let ledger = serde_json::json!({
        "events": [
            { "task_id": "done-task", "tool": "neurostrata_related" },
            { "task_id": "inprog-task", "tool": "neurostrata_sparql_query" },
            { "task_id": "inprog-task", "tool": "neurostrata_related" },
            { "tool": "neurostrata_locate" }
        ]
    });
    assert_eq!(
        graph_tools_from_ledger(&ledger, &ids),
        vec!["neurostrata_related", "neurostrata_sparql_query"],
        "done-task and unattributed receipts must not count"
    );
}
