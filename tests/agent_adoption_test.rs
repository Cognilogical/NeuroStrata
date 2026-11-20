//! Agent graph/ontology adoption contract tests.
//!
//! The crate ships a binary only (see `Cargo.toml`), so this test compiles the
//! crate-free `task::routing` module from its real path -- the same pattern
//! `tests/graph_read/memories_for_file_test.rs` uses for its ranking helpers.

#[path = "../src/task/routing.rs"]
mod routing;

use routing::{agent_routing_contract_json, evaluate_triggers, TaskContext};
use serde_json::Value;

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

    let expected = [
        ("T_PATH", "neurostrata_memories_for_file"),
        ("T_AMBIGUOUS", "neurostrata_locate"),
        ("T_HIGH_BLAST", "neurostrata_related"),
        ("T_DIFF", "neurostrata_diff_tour"),
        ("T_DOMAIN_NEW", "neurostrata_taxonomy"),
        ("T_STRATEGIC", "neurostrata_taxonomy"),
        ("T_INVARIANT", "neurostrata_sparql_query"),
    ];
    for (trigger_id, tool) in expected {
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
