#[path = "../../src/search.rs"]
mod search;
#[path = "../../src/task/routing.rs"]
mod routing;

use search::{
    append_search_contract_line, passes_filters, SearchFilters, SearchMemoryContract,
    INCLUDE_RELATED_DEPRECATION_WARNING,
};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

// Finding 2: every hard-error return (missing query/namespace, bad mode, init
// or embed failure, search failure) is normalized through this same helper,
// so the additive contract line is present even when no results exist.

#[test]
fn missing_query_error_carries_additive_search_contract() {
    let args = json!({ "namespace": "NeuroStrata", "include_related": true });
    let out = append_search_contract_line("Missing 'query' parameter.".to_string(), &args, vec![]);
    assert!(
        out.starts_with("Missing 'query' parameter.\n\nsearch_contract: "),
        "error text must stay byte-identical and contract appended: {out}"
    );
    let line = out.split("search_contract: ").nth(1).expect("contract line");
    let contract: serde_json::Value = serde_json::from_str(line).expect("contract JSON");
    assert_eq!(contract["mode"], "hybrid", "hybrid default on error path");
    assert_eq!(contract["graph_expanded_ids"], json!([]));
    let warnings = contract["warnings"].as_array().expect("warnings array");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str() == Some(INCLUDE_RELATED_DEPRECATION_WARNING)),
        "deprecation warning must appear on error path: {contract}"
    );
}

#[test]
fn mode_and_search_errors_carry_additive_search_contract() {
    for error in [
        "ERROR [MODE]: unknown search mode 'bogus'. Use 'hybrid' (default), 'vector', 'lexical', or 'lexical-inhouse'.",
        "Failed to search database.",
    ] {
        let out = append_search_contract_line(
            error.to_string(),
            &json!({ "mode": "lexical" }),
            vec![],
        );
        assert!(
            out.starts_with(&format!("{error}\n\nsearch_contract: ")),
            "error text must stay byte-identical: {out}"
        );
        let line = out.split("search_contract: ").nth(1).expect("contract line");
        let contract: serde_json::Value = serde_json::from_str(line).expect("contract JSON");
        assert_eq!(contract["mode"], "lexical", "explicit mode preserved: {contract}");
        assert_eq!(contract["graph_expanded_ids"], json!([]));
    }
}

#[test]
fn excludes_code_ast_by_default() {
    let f = SearchFilters::default_filters();
    assert!(!search::type_allowed("code_ast", &f));
}

#[test]
fn fact_shape_filter() {
    let f = SearchFilters {
        fact_shape: Some("decision".into()),
        ..SearchFilters::default_filters()
    };
    let m = json!({"decision": {"choice":"x"}});
    assert!(search::fact_shape_matches(&m, "decision"));
    assert!(passes_filters("fact", &m, "src/lib.rs", &f));
    let m2 = json!({});
    assert!(!passes_filters("fact", &m2, "src/lib.rs", &f));
}

#[test]
fn path_prefix_filter() {
    let f = SearchFilters {
        path_prefix: Some("src/".into()),
        ..SearchFilters::default_filters()
    };
    assert!(passes_filters("rule", &json!({}), "src/lib.rs", &f));
    assert!(!passes_filters("rule", &json!({}), "tests/x.rs", &f));
}

#[test]
fn exclude_stale_filter() {
    let f = SearchFilters {
        exclude_stale: true,
        ..SearchFilters::default_filters()
    };
    let m = json!({"source_currency":"changed","verification":{"status":"unreviewed"}});
    assert!(!passes_filters("fact", &m, "src/lib.rs", &f));
    let m2 = json!({"source_currency":"changed","verification":{"status":"verified"}});
    assert!(passes_filters("fact", &m2, "src/lib.rs", &f));
}

#[test]
fn search_memory_rejects_silent_include_related_noop() {
    let contract = SearchMemoryContract::from_arguments(
        &json!({
            "query": "graph contract",
            "namespace": "NeuroStrata",
            "include_related": true
        }),
        vec![],
    );
    let value = serde_json::to_value(&contract).expect("contract serializes");
    let warnings = value["warnings"].as_array().expect("warnings is an array");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str() == Some(INCLUDE_RELATED_DEPRECATION_WARNING)),
        "exact deprecation warning required: {value}"
    );
    assert_eq!(
        value["mode"].as_str(),
        Some("hybrid"),
        "mode must default to hybrid: {value}"
    );
    assert!(
        value["graph_expanded_ids"].is_array(),
        "graph_expanded_ids must always be an array: {value}"
    );
    assert_eq!(
        value["include_references"],
        json!(true),
        "deprecated alias must map to include_references: {value}"
    );
}

#[test]
fn search_memory_contract_prefers_explicit_include_references() {
    let contract = SearchMemoryContract::from_arguments(
        &json!({"include_related": true, "include_references": false, "mode": "lexical"}),
        vec!["neighbor".to_string()],
    );
    let value = serde_json::to_value(&contract).expect("contract serializes");
    assert_eq!(value["include_references"], json!(false));
    assert_eq!(value["mode"].as_str(), Some("lexical"));
    assert_eq!(value["graph_expanded_ids"], json!(["neighbor"]));
    let warnings = value["warnings"].as_array().expect("warnings array");
    assert_eq!(warnings.len(), 1, "one warning for the deprecated alias");
}

// Tracked sites, including the generation source for the harness skill
// (`.agents/.../SKILL.md`, pulled in via include_str! in harness_plugins.rs).
// These are ALWAYS asserted — a missing tracked file is a failure, not a skip.
const GRAPH_CONTRACT_TRACKED_SITES: [&str; 6] = [
    "AGENTS.md",
    ".agents/agents/NeuroStrata-Task.md",
    ".agents/skills/neurostrata/SKILL.md",
    "src/task/mod.rs",
    "src/task/wiring.rs",
    "src/task/harness_plugins.rs",
];

// Generated from the tracked .agents/ skill at runtime and gitignored; checked
// only as an additional assertion when present in the working tree.
const GENERATED_SKILL_SITE: &str = ".opencode/skills/neurostrata/SKILL.md";

fn assert_graph_contract(root: &Path, rel: &str) {
    let path = root.join(rel);
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {rel}: {e}"));
    assert!(
        !text.contains("include_related: true"),
        "{rel} still prescribes the removed include_related flag"
    );
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        if (lower.contains("search_memory") || lower.contains("mode"))
            && lower.contains("vector")
            && lower.contains("default")
            && !lower.contains("hybrid")
        {
            panic!("{rel} still claims vector is the default mode: {line}");
        }
    }
    if text.contains("search_memory") {
        assert!(
            text.contains("hybrid"),
            "{rel} must document hybrid as the default mode"
        );
        assert!(
            text.contains("include_references"),
            "{rel} must document include_references"
        );
    }
}

#[test]
fn prompt_sites_have_no_stale_graph_contract() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for rel in GRAPH_CONTRACT_TRACKED_SITES {
        assert_graph_contract(&root, rel);
    }
    let generated = root.join(GENERATED_SKILL_SITE);
    if generated.exists() {
        assert_graph_contract(&root, GENERATED_SKILL_SITE);
    }
}

// Task 13 P1-6 / review IMPORTANT-1: derive BOTH tool names and required args
// from `agent_routing_contract_json()` and check them against the prompt's
// command-signature lines, not just a hand-written list of three names.

/// `(tool, required_args)` for every canonical R4 trigger, read from the contract.
fn contract_tool_args(contract: &serde_json::Value) -> Vec<(String, Vec<String>)> {
    contract["triggers"]
        .as_array()
        .expect("triggers array")
        .iter()
        .map(|t| {
            (
                t["tool"].as_str().expect("tool").to_string(),
                t["required_args"]
                    .as_array()
                    .expect("required_args")
                    .iter()
                    .map(|a| a.as_str().expect("arg").to_string())
                    .collect(),
            )
        })
        .collect()
}

/// Word-boundary token check, robust to backticks, commas, and parentheses.
fn line_has_token(line: &str, token: &str) -> bool {
    line.match_indices(token).any(|(i, _)| {
        let before = line[..i].chars().last();
        let after = line[i + token.len()..].chars().next();
        let boundary = |c: Option<char>| c.map_or(true, |c| !c.is_alphanumeric() && c != '_');
        boundary(before) && boundary(after)
    })
}

/// Lines where the tool name is immediately followed by an arg list:
/// `` `neurostrata_related` (`namespace`, ...) ``. Tolerant of spacing.
fn signature_lines<'a>(text: &'a str, tool: &str) -> Vec<&'a str> {
    text.lines()
        .filter(|line| {
            line.match_indices(tool).any(|(i, _)| {
                let rest = line[i + tool.len()..].trim_start();
                let rest = rest.strip_prefix('`').unwrap_or(rest).trim_start();
                rest.starts_with('(')
            })
        })
        .collect()
}

fn assert_signature_args(sig: &str, tool: &str, args: &[String], site: &str) {
    for arg in args {
        assert!(
            line_has_token(sig, arg),
            "{site}: {tool} signature line missing required arg `{arg}`: {sig}"
        );
    }
}

#[test]
fn routing_contract_tool_names_match_agent_prompt() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let agent_prompt =
        fs::read_to_string(root.join(".agents/agents/NeuroStrata-Task.md")).expect("agent prompt");
    let contract = routing::agent_routing_contract_json();
    let tools = contract_tool_args(&contract);

    // Every tool this prompt documents with a signature carries ALL of that
    // tool's contract-required args on that very line (no cross-line borrowing).
    for (tool, args) in &tools {
        for sig in signature_lines(&agent_prompt, tool) {
            assert_signature_args(sig, tool, args, "agent prompt");
        }
    }

    // Across all tracked prompt sites, any tool with at least one signature
    // must have SOME signature carrying its full required args. `namespace` is
    // ambient in authored prose (the caller namespace is inferred), so only the
    // non-ambient args are enforced here; the strict agent-prompt check above
    // still requires every contract arg including `namespace`. SKILL.md is a
    // broad generated surface whose legacy `related(depth)` line is tolerated,
    // but it cannot be the sole coverage for a contract tool.
    let ambient = ["namespace"];
    let site_texts: Vec<String> = GRAPH_CONTRACT_TRACKED_SITES
        .iter()
        .map(|rel| fs::read_to_string(root.join(rel)).unwrap_or_default())
        .collect();
    for (tool, args) in &tools {
        let mut sigs: Vec<&str> = Vec::new();
        for text in &site_texts {
            sigs.extend(signature_lines(text, tool));
        }
        if sigs.is_empty() {
            continue;
        }
        let checked: Vec<&String> = args.iter().filter(|a| !ambient.contains(&a.as_str())).collect();
        assert!(
            sigs.iter().any(|sig| checked.iter().all(|a| line_has_token(sig, a))),
            "no tracked-site {tool} signature carries required args {checked:?}"
        );
    }

    // RED control: a drifted `related(depth)` signature is rejected by the
    // very same helper, proving the test is not vacuous.
    let drifted = agent_prompt.replace("`max_hops`", "`depth`");
    let drifted_sig = signature_lines(&drifted, "neurostrata_related")
        .into_iter()
        .next()
        .expect("related signature");
    assert!(
        !line_has_token(drifted_sig, "max_hops"),
        "RED control: drifted signature must lose max_hops"
    );

    // Keep the minimum name-presence and no-drift phrase checks.
    for tool in [
        "neurostrata_memories_for_file",
        "neurostrata_related",
        "neurostrata_sparql_query",
    ] {
        assert!(agent_prompt.contains(tool), "agent prompt missing tool {tool}");
    }
    assert!(agent_prompt.contains("max_hops"), "related must document max_hops");
    assert!(!agent_prompt.contains("`depth`"), "related must not document depth");
    assert!(agent_prompt.contains("\"hybrid\"") && agent_prompt.contains("(default)"));
    assert!(agent_prompt.contains("include_references: true"));
    assert!(!agent_prompt.contains("include_related"));
}
