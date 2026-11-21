//! Agent graph/ontology adoption contract tests.
//!
//! The crate ships a binary only (see `Cargo.toml`), so this test compiles the
//! crate-free `task::routing` module from its real path -- the same pattern
//! `tests/graph_read/memories_for_file_test.rs` uses for its ranking helpers.

#[path = "../src/graph_read/memories_for_file.rs"]
mod memories_for_file;
#[path = "../src/task/graph_advisories.rs"]
mod graph_advisories;
#[path = "../src/task/routing.rs"]
mod routing;
#[path = "../src/task/snapshot_readiness.rs"]
mod snapshot_readiness;
#[path = "../src/task/tool_receipts.rs"]
mod tool_receipts;

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
            { "task_id": "done-task", "tool": "neurostrata_related", "status": "success" },
            { "task_id": "inprog-task", "tool": "neurostrata_sparql_query", "status": "success" },
            { "task_id": "inprog-task", "tool": "neurostrata_related", "status": "success" },
            { "tool": "neurostrata_locate", "status": "success" }
        ]
    });
    assert_eq!(
        graph_tools_from_ledger(&ledger, &ids),
        vec!["neurostrata_related", "neurostrata_sparql_query"],
        "done-task and unattributed receipts must not count"
    );
}

/// Task 13 P1-3: failed/unsupported receipts are not genuine coverage, and a
/// replayed non-empty `event_id` is one lookup, not two.
#[test]
fn snapshot_readiness_failed_and_replayed_events_excluded() {
    use snapshot_readiness::{active_task_ids, graph_tools_from_ledger};

    let active = active_task_ids(&[("t1".to_string(), "in_progress".to_string())]);
    let ledger = serde_json::json!({
        "events": [
            { "event_id": "e1", "task_id": "t1", "tool": "neurostrata_related", "status": "failed" },
            { "event_id": "e2", "task_id": "t1", "tool": "neurostrata_locate", "status": "unsupported" },
            { "event_id": "e3", "task_id": "t1", "tool": "neurostrata_sparql_query", "status": "empty" },
            { "event_id": "e3", "task_id": "t1", "tool": "neurostrata_sparql_query", "status": "empty" },
            { "event_id": "e4", "task_id": "t1", "tool": "neurostrata_related", "status": "success" }
        ]
    });
    assert_eq!(
        graph_tools_from_ledger(&ledger, &active),
        vec!["neurostrata_related", "neurostrata_sparql_query"],
        "failed/unsupported excluded; replayed e3 counted once"
    );
}

/// Spec §6 R6.1 / Review Focus item 4: the pre-commit hook must never fabricate
/// `tool:"pre-commit"` lookup rows for staged files that had no real graph
/// query. Run the real hook in a throwaway repo with one staged file and no
/// ledger entries, then inspect the ledger it leaves behind.
#[test]
fn precommit_no_longer_fabricates_ledger_rows() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join(".git/hooks")).unwrap();
    std::fs::create_dir_all(repo.join("src")).unwrap();

    let run = |args: &[&str]| {
        let out = std::process::Command::new(args[0])
            .args(&args[1..])
            .current_dir(&repo)
            .output()
            .expect("run command");
        assert!(
            out.status.success(),
            "{} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["git", "init", "-q"]);
    run(&["git", "config", "user.email", "test@example.com"]);
    run(&["git", "config", "user.name", "Test"]);
    std::fs::write(repo.join("src/main.rs"), "fn main() {}\n").unwrap();
    run(&["git", "add", "src/main.rs"]);

    let hook_path = repo.join(".git/hooks/pre-commit");
    std::fs::write(&hook_path, include_str!("../scripts/hooks/pre-commit")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&hook_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&hook_path, perms).unwrap();
    }

    let out = std::process::Command::new("bash")
        .arg(&hook_path)
        .current_dir(&repo)
        .output()
        .expect("run pre-commit hook");
    assert!(
        out.status.success(),
        "hook failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let ledger_path = repo.join(".NeuroStrata/session_queries.json");
    let ledger: Value = std::fs::read_to_string(&ledger_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let no_fabricated_rows = ledger
        .get("queries")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter().all(|row| {
                row.get("tool").and_then(Value::as_str) != Some("pre-commit")
            })
        })
        .unwrap_or(true);
    assert!(
        no_fabricated_rows,
        "pre-commit hook fabricated ledger rows: {ledger}"
    );
}

/// Build a fully-populated receipt from raw query/content text. The raw text
/// must never survive into the serialized receipt — only digests do.
fn sample_receipt(query: &str, content: &str) -> tool_receipts::ReceiptFields {
    use tool_receipts::{ReceiptFields, ReceiptStatus};
    ReceiptFields::new(
        "evt-1".to_string(),
        1_700_000_000,
        "ses-1".to_string(),
        "t1".to_string(),
        "NeuroStrata".to_string(),
        "neurostrata_memories_for_file".to_string(),
        "memories_for_file".to_string(),
        "T_PATH".to_string(),
        "graph-first".to_string(),
        true,
        ReceiptStatus::Success,
        12,
        2,
        vec!["m1".to_string(), "m2".to_string()],
        Vec::new(),
        Vec::new(),
        query,
        content,
    )
}

/// Spec §6 R6.2: a receipt carries digests only, never raw query/content.
#[test]
fn receipt_fields_are_digest_only() {
    let raw_query = "SELECT * WHERE { ?s ?p ?o }";
    let raw_content = "top-secret-memory-content";
    let v = sample_receipt(raw_query, raw_content).to_value();
    let obj = v.as_object().expect("receipt must be a JSON object");
    assert!(!obj.contains_key("query"), "raw query must never be stored");
    assert!(!obj.contains_key("content"), "raw content must never be stored");
    let qd = tool_receipts::digest(raw_query);
    let cd = tool_receipts::digest(raw_content);
    assert_eq!(v["query_digest"].as_str(), Some(qd.as_str()));
    assert_eq!(v["content_digest"].as_str(), Some(cd.as_str()));
    assert!(
        !v.to_string().contains(raw_content),
        "serialized receipt leaked raw content"
    );
}

/// Spec §6 R6.1: `events[]` is versioned and bounded so a long session cannot
/// grow the ledger without limit.
#[test]
fn receipt_events_are_versioned_and_bounded() {
    let mut doc = serde_json::json!({ "namespace": "NeuroStrata", "queries": [] });
    tool_receipts::ensure_events_section(&mut doc);
    assert_eq!(doc["events_version"].as_u64(), Some(1));
    assert!(doc["events"].as_array().unwrap().is_empty());

    for i in 0..(tool_receipts::MAX_EVENTS + 5) {
        let mut receipt = sample_receipt("q", "c").to_value();
        receipt["event_id"] = serde_json::json!(format!("e{i}"));
        tool_receipts::append_event(&mut doc, receipt);
    }
    let events = doc["events"].as_array().unwrap();
    assert_eq!(events.len(), tool_receipts::MAX_EVENTS);
    assert_eq!(events.first().unwrap()["event_id"].as_str(), Some("e5"));
    assert_eq!(doc["queries"].as_array().unwrap().len(), 0);
}

/// Spec §6 R6.3: advanced subsystems get additive counters only — no gates,
/// quotas or nudges are introduced by the receipt recorder.
#[test]
fn receipt_tool_invocations_are_additive_only() {
    let mut doc = serde_json::json!({});
    tool_receipts::bump_tool_invocation(&mut doc, "neurostrata_related");
    tool_receipts::bump_tool_invocation(&mut doc, "neurostrata_related");
    tool_receipts::bump_tool_invocation(&mut doc, "neurostrata_taxonomy");
    assert_eq!(doc["tool_invocations"]["neurostrata_related"].as_u64(), Some(2));
    assert_eq!(doc["tool_invocations"]["neurostrata_taxonomy"].as_u64(), Some(1));
    assert!(doc.get("quota").is_none());
    assert!(doc.get("gate").is_none());
    assert!(doc.get("nudge").is_none());
}

/// Spec §6 R6.4: receipts are private/local JSON events, never memory rows,
/// so a SynapticMesh export over all candidate rows contains zero of them.
#[test]
fn receipts_are_private_and_digest_only() {
    use tool_receipts::{mesh_exportable_rows, RECEIPT_FIELD_NAMES};

    let receipt = sample_receipt("query with secret text", "content with secret text").to_value();
    let obj = receipt.as_object().expect("receipt must be a JSON object");
    assert_eq!(obj.len(), RECEIPT_FIELD_NAMES.len(), "exact R6 field set");
    for key in RECEIPT_FIELD_NAMES {
        assert!(obj.contains_key(*key), "missing R6 receipt field {key}");
    }
    for key in ["query", "content", "memory_type", "eligibility"] {
        assert!(!obj.contains_key(key), "receipt must not carry {key}");
    }
    assert!(!receipt.to_string().contains("secret"));

    let mut doc = serde_json::json!({ "namespace": "NeuroStrata", "queries": [] });
    tool_receipts::ensure_events_section(&mut doc);
    tool_receipts::append_event(&mut doc, receipt);
    let mut candidates = doc["queries"].as_array().cloned().unwrap_or_default();
    candidates.extend(doc["events"].as_array().cloned().unwrap_or_default());

    let shareable = ["rule", "fact", "procedure", "action_type", "thesis", "concept"];
    let exported = mesh_exportable_rows(&candidates, &shareable);
    assert!(
        exported.iter().all(|row| row.get("event_id").is_none()),
        "mesh export must contain zero receipt rows: {exported:?}"
    );
}

/// Review fix (Critical 1): a receipt built from a REAL empty / unsupported /
/// failed response must carry that status — never a hardcoded Success.
#[test]
fn receipt_status_reflects_real_outcome_not_dispatch_time() {
    use tool_receipts::{build_dispatch_receipt, ReceiptStatus};

    let mk = |text: &str, is_error: bool| {
        build_dispatch_receipt(
            "evt-outcome".to_string(),
            1,
            "ses".to_string(),
            "ns".to_string(),
            "neurostrata_memories_for_file".to_string(),
            "src/main.rs",
            text,
            is_error,
            9,
        )
    };

    let empty = serde_json::json!({ "memories": [], "unsupported": false }).to_string();
    let unsupported = serde_json::json!({
        "memories": [], "unsupported": true, "status": "unsupported"
    })
    .to_string();
    let success = serde_json::json!({ "memories": [ { "id": "m1" } ] }).to_string();

    assert_eq!(mk(&empty, false).status, ReceiptStatus::Empty);
    assert_eq!(mk(&unsupported, false).status, ReceiptStatus::Unsupported);
    assert_eq!(mk("No relevant memories found.", false).status, ReceiptStatus::Empty);
    assert_eq!(mk("boom", true).status, ReceiptStatus::Failed);

    let ok = mk(&success, false);
    assert_eq!(ok.status, ReceiptStatus::Success);
    assert_eq!(ok.result_count, 1);
    assert_eq!(ok.returned_ids, vec!["m1".to_string()]);
    assert_eq!(ok.latency_ms, 9);
}

/// Review fix (Critical 2): every tool in GRAPH_TOOLS must have a post-dispatch
/// receipt path. GRAPH_TOOLS is the exact gate the MCP dispatcher uses, so a
/// missing tool here means its dispatch writes no receipt.
#[test]
fn every_graph_tool_has_post_dispatch_receipt_path() {
    use std::collections::BTreeSet;
    use tool_receipts::{build_dispatch_receipt, route_class, RECEIPT_FIELD_NAMES};

    let response = serde_json::json!({ "memories": [ { "id": "m1" } ] }).to_string();
    let mut covered = BTreeSet::new();
    for tool in snapshot_readiness::GRAPH_TOOLS {
        let receipt = build_dispatch_receipt(
            "evt-coverage".to_string(),
            1,
            "ses".to_string(),
            "ns".to_string(),
            (*tool).to_string(),
            "q",
            &response,
            false,
            3,
        )
        .to_value();
        assert_eq!(receipt["tool"].as_str(), Some(*tool));
        assert_eq!(receipt["status"].as_str(), Some("success"));
        assert_eq!(receipt.as_object().unwrap().len(), RECEIPT_FIELD_NAMES.len());
        assert!(!route_class(tool).is_empty());
        covered.insert(*tool);
    }
    assert_eq!(covered.len(), 8, "R8 requires all eight graph tools covered");
    for required in [
        "neurostrata_memories_for_file",
        "neurostrata_related",
        "neurostrata_locate",
        "neurostrata_sparql_query",
        "neurostrata_object_view",
        "neurostrata_taxonomy",
        "neurostrata_diff_tour",
        "neurostrata_search_memory",
    ] {
        assert!(covered.contains(required), "no receipt path for {required}");
    }
}

/// Review fix (Minor 1): append_event bounds returned_ids even on a manually
/// constructed event, not just through ReceiptFields::new.
#[test]
fn append_event_bounds_manual_returned_ids() {
    let response = serde_json::json!({ "memories": [] }).to_string();
    let mut event = tool_receipts::build_dispatch_receipt(
        "evt-bound".to_string(),
        1,
        "ses".to_string(),
        "ns".to_string(),
        "neurostrata_related".to_string(),
        "q",
        &response,
        false,
        1,
    )
    .to_value();
    let ids: Vec<serde_json::Value> = (0..(tool_receipts::MAX_RETURNED_IDS + 10))
        .map(|i| serde_json::json!(format!("m{i}")))
        .collect();
    event["returned_ids"] = serde_json::json!(ids);
    let mut doc = serde_json::json!({});
    tool_receipts::append_event(&mut doc, event);
    let stored = doc["events"][0]["returned_ids"].as_array().unwrap();
    assert_eq!(stored.len(), tool_receipts::MAX_RETURNED_IDS);
}

/// Spec §6 R7.2: a genuine one-hop lookup is not coverage for an applicable
/// escalation. Only an unmet `T_HIGH_BLAST`/`T_DIFF` produces a non-blocking
/// `W_GRAPH_NO_EXPANSION`; the old `graph_queries_count == 0` nudge is gone.
#[test]
fn validate_warns_only_on_unresolved_applicable_triggers() {
    use graph_advisories::{compute_advisories, AdvisoryInput, W_GRAPH_NO_EXPANSION};
    use std::collections::BTreeSet;

    let active: BTreeSet<String> = ["t-active".to_string()].into_iter().collect();
    let allowed = snapshot_readiness::GRAPH_TOOLS;
    let no_triggers: Vec<String> = Vec::new();

    // Genuine one-hop lookup only (plus a legacy synthetic row). No escalation
    // is applicable, so no advisory is emitted.
    let one_hop = serde_json::json!({
        "events_version": 1,
        "events": [{
            "task_id": "t-active",
            "tool": "neurostrata_memories_for_file",
            "operation": "memories_for_file",
            "trigger_id": "T_PATH",
            "status": "success",
            "returned_ids": ["mem-1"]
        }],
        "queries": [{ "tool": "pre-commit", "query": "src/task/mod.rs" }]
    });
    let clean = compute_advisories(&AdvisoryInput {
        ledger: &one_hop,
        active_task_ids: &active,
        allowed_graph_tools: allowed,
        applicable_trigger_ids: &no_triggers,
        taxonomy_available: false,
        concept_grounding_linked: false,
    })
    .to_json();
    assert_eq!(clean["genuine_lookup_coverage"], true);
    assert_eq!(clean["distinct_graph_tools"], 1);
    assert!(
        clean["recommendations"].as_array().unwrap().is_empty(),
        "a genuine one-hop lookup must not warn"
    );
    assert!(clean["unresolved_trigger_ids"].as_array().unwrap().is_empty());

    // Applicable T_HIGH_BLAST with no genuine related receipt: the prescribed
    // action was emitted, but the escalation dispatch failed. The one-hop
    // success and the legacy related row must NOT count as compliance.
    let unresolved = serde_json::json!({
        "events_version": 1,
        "events": [
            {
                "task_id": "t-active",
                "tool": "neurostrata_memories_for_file",
                "trigger_id": "T_PATH",
                "status": "success",
                "prescribed_action_ids": ["T_HIGH_BLAST:abc123"]
            },
            {
                "task_id": "t-active",
                "tool": "neurostrata_related",
                "trigger_id": "T_HIGH_BLAST",
                "status": "failed"
            }
        ],
        "queries": [{ "tool": "neurostrata_related" }]
    });
    let applicable = vec!["T_HIGH_BLAST".to_string()];
    let report = compute_advisories(&AdvisoryInput {
        ledger: &unresolved,
        active_task_ids: &active,
        allowed_graph_tools: allowed,
        applicable_trigger_ids: &applicable,
        taxonomy_available: false,
        concept_grounding_linked: false,
    })
    .to_json();
    let codes: Vec<&str> = report["recommendations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(codes, vec![W_GRAPH_NO_EXPANSION], "only the unmet escalation warns");
    assert_eq!(report["high_blast_radius_unexplored"], true);
    assert_eq!(
        report["unresolved_trigger_ids"],
        serde_json::json!(["T_HIGH_BLAST"])
    );
    assert!(
        report.get("violations").is_none(),
        "advisories never embed gate violations"
    );
}

/// Spec §6 R8: the evaluation contract is a checked-in, strict-JSON declaration.
fn load_evaluation_contract() -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("eval/agent_adoption/evaluation_contract.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("missing evaluation contract at {}: {e}", path.display()));
    serde_json::from_str(&text).expect("evaluation contract must be strict JSON")
}

/// Spec §6 R8.3: pin every acceptance threshold to its exact declared value,
/// and assert the owner-labeled task sets are a declaration only.
#[test]
fn evaluation_contract_matches_spec_thresholds() {
    let contract = load_evaluation_contract();
    let t = &contract["thresholds"];

    assert_eq!(t["route_coverage"].as_f64(), Some(0.95));
    assert_eq!(t["follow_through"].as_f64(), Some(0.80));
    assert_eq!(t["routing_precision"].as_f64(), Some(0.90));
    assert_eq!(t["unnecessary_ontology_routing"].as_f64(), Some(0.05));
    assert_eq!(t["blind_accuracy_lift"].as_f64(), Some(0.10));
    assert_eq!(t["regression_floor"].as_f64(), Some(-0.02));
    assert_eq!(t["response_p95_bytes"].as_u64(), Some(32768));
    assert_eq!(t["routing_p95_ms"].as_u64(), Some(750));
    assert_eq!(t["ontology_lift"].as_f64(), Some(0.10));

    // R8.2: a declaration of 120 owner-labeled tasks across three sets -- never
    // fabricated labels. The real suite is tracked separately as neurostrata-pqhb.
    let sets = &contract["task_sets"];
    assert_eq!(sets["label_source"].as_str(), Some("owner"));
    assert_eq!(sets["total_tasks"].as_u64(), Some(120));
    assert!(
        sets.get("labels").is_none(),
        "task sets are a declaration, not fabricated labels"
    );
    let declared = sets["sets"]
        .as_array()
        .expect("task_sets.sets must be an array");
    assert_eq!(declared.len(), 3);
    for (id, count) in [
        ("routine_edits", 40u64),
        ("cross_file_impact", 40),
        ("ontology_relevant", 40),
    ] {
        let set = declared
            .iter()
            .find(|s| s["id"] == id)
            .unwrap_or_else(|| panic!("missing task set {id}"));
        assert_eq!(set["count"].as_u64(), Some(count));
        assert_eq!(set["label_source"].as_str(), Some("owner"));
    }

    assert_eq!(contract["production_roi_claimed"].as_bool(), Some(false));
}

/// Spec §6 R8.1/R8.4: the ontology lift gate stays honestly FAILING while the
/// measured mixed alias+hierarchy subset lift is below 0.10. No default-ranking
/// ROI claim is allowed anywhere.
#[test]
fn semantic_lift_gate_remains_honest() {
    let contract = load_evaluation_contract();
    assert_eq!(contract["production_roi_claimed"].as_bool(), Some(false));

    let gate = &contract["semantic_lift_gate"];
    let measured = gate["measured_alias_hierarchy_recall_at_5_lift"]
        .as_f64()
        .expect("gate must record the measured subset lift");
    let threshold = gate["threshold"]
        .as_f64()
        .expect("gate must record its threshold");
    assert_eq!(threshold, 0.10);
    assert!(
        measured < 0.10,
        "measured lift {measured} must remain below the 0.10 floor"
    );
    assert_eq!(gate["verdict"].as_str(), Some("FAIL"));
    assert_ne!(gate["verdict"].as_str(), Some("PASS"));
    assert_eq!(gate["production_claim"].as_bool(), Some(false));
    assert_eq!(gate["default_ranking_roi_claimed"].as_bool(), Some(false));
}
