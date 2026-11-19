//! Rust -> Lean parity and Aeneas extractability contract for the formal kernel.
//!
//! This target compiles `src/formal_kernel.rs` from its real source path because
//! the crate ships a binary target only (no `[lib]`), mirroring the other
//! crate-free contract tests under `tests/`.
//!
//! Three contracts:
//! 1. the kernel compiles standalone with zero `--extern` flags (Aeneas/Charon
//!    extractability precondition);
//! 2. the exhaustive 1,273-row truth table produced by the Rust kernel equals the
//!    one emitted by `lake --dir proofs exe neurostrata-proofs`;
//! 3. no Lean proof source carries a `sorry` placeholder or an `axiom`.

#![allow(dead_code)]

#[path = "../src/formal_kernel.rs"]
mod formal_kernel;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use formal_kernel::{
    action_gate_tag, action_revert_tag, action_task_effect_tag, allocate_hybrid_slots,
    cert_verdict_tag, claim_decision_tag, claim_scope_verdict_tag, classify_merge_decision_tag,
    classify_tour_stop, epoch_gate_tag, evaluate_action_gate, evaluate_action_revert,
    evaluate_action_task_effect, evaluate_claim_decision, evaluate_claim_scope_gate,
    evaluate_connector_post_gate, evaluate_connector_pre_gate, evaluate_import_precheck,
    evaluate_insight_define, evaluate_procedure_fire, evaluate_reducer_total,
    evaluate_retention_gate, evaluate_scenario_commit, evaluate_seq_continuity,
    evaluate_sovereignty_gate, hash_verdict_tag, import_precheck_tag, intersect_var_bounds,
    is_action_memory_shareable, is_memory_valid_at, join_task_status, lifecycle_can_transition,
    owl2rl_rule_tag, procedure_fire_tag, resolve_cross_file_call, resolve_scenario_row,
    retention_verdict_tag, seq_decision_tag, sovereignty_decision_tag, step_verdict_tag,
    kernel_rule_tag, task_step_tag, verify_certificate_chain, verify_certificate_chain_slices,
    verify_certificate_hash, verify_derivation_certificate, verify_derivation_step_structure,
    ActionMemoryKind, ActionReceiptState, ActionTaskTargetStatus, EpistemicStatus,
    KernelActionReceiptState, KernelCertificateStep, KernelCertificateVerdict, KernelCertVerdict,
    KernelConnectorOp, KernelConnectorPostGateInput, KernelConnectorPostVerdict,
    KernelConnectorPreGateInput, KernelConnectorPreVerdict, KernelHashVerdict,
    KernelOwl2RlRuleId, KernelProcedureTrust, KernelStepVerdict, ReducerOpKind, TwinBoundError,
};

/// The statuses in the exact order used by `proofs/Main.lean`.
const TASK_STATUSES: [formal_kernel::KernelTaskStatus; 4] = [
    formal_kernel::KernelTaskStatus::Open,
    formal_kernel::KernelTaskStatus::InProgress,
    formal_kernel::KernelTaskStatus::Blocked,
    formal_kernel::KernelTaskStatus::Done,
];

/// The lifecycle states in the exact order used by `proofs/Main.lean`.
const LIFECYCLE_STATES: [formal_kernel::KernelLifecycleState; 4] = [
    formal_kernel::KernelLifecycleState::Active,
    formal_kernel::KernelLifecycleState::Archived,
    formal_kernel::KernelLifecycleState::Superseded,
    formal_kernel::KernelLifecycleState::Tombstoned,
];

fn bool_str(b: bool) -> &'static str {
    if b {
        "true"
    } else {
        "false"
    }
}

fn connector_post_verdict_tag(v: KernelConnectorPostVerdict) -> &'static str {
    match v {
        KernelConnectorPostVerdict::AllowPostOutput => "allow_post_output",
        KernelConnectorPostVerdict::DenyObservedBoundsExceeded => "deny_observed_bounds_exceeded",
        KernelConnectorPostVerdict::DenySourceDigestMismatch => "deny_source_digest_mismatch",
        KernelConnectorPostVerdict::DenyCasConflict => "deny_cas_conflict",
    }
}

/// Fixed-size 64-element bool array with the given prefix set, rest `false`.
fn bool_array_64(prefix: &[bool]) -> [bool; 64] {
    let mut arr = [false; 64];
    for (i, &b) in prefix.iter().enumerate() {
        arr[i] = b;
    }
    arr
}

/// Render a `[bool; 64]` exactly like Lean prints its `List Bool` literal:
/// `[true,true,false,...]` (no spaces after commas), matching the hardcoded
/// `CERT_CHAIN` rows in `proofs/Main.lean`.
fn bool_array_str(b: &[bool; 64]) -> String {
    let parts: Vec<String> = b.iter().map(|x| if *x { "true".to_string() } else { "false".to_string() }).collect();
    format!("[{}]", parts.join(","))
}

/// Render a bool slice exactly like Lean prints a `List Bool` literal.
fn bool_slice_str(b: &[bool]) -> String {
    let parts: Vec<String> = b.iter().map(|x| if *x { "true".to_string() } else { "false".to_string() }).collect();
    format!("[{}]", parts.join(","))
}

/// Render certificate step witnesses as `tag:pc:avail,...`.
fn cert_steps_str(steps: &[KernelCertificateStep]) -> String {
    let parts: Vec<String> = steps
        .iter()
        .map(|s| format!("{}:{}:{}", s.rule_tag, s.premise_count, s.premises_available))
        .collect();
    format!("[{}]", parts.join(","))
}

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at the crate root that owns this test target.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Generates the canonical 1011-line truth table in `proofs/Main.lean` order:
/// 891 base/action/code-intel/ontology rows + 120 taxonomy rows.
fn generate_rust_truth_table() -> Vec<String> {
    let mut lines: Vec<String> = Vec::with_capacity(1273);

    for &from in TASK_STATUSES.iter() {
        for &to in TASK_STATUSES.iter() {
            for &has_reason in [false, true].iter() {
                for &has_extraction in [false, true].iter() {
                    let tag = task_step_tag(from, to, has_reason, has_extraction);
                    lines.push(format!(
                        "TASK_FSM|{}|{}|{}|{}|{}",
                        from.as_str(),
                        to.as_str(),
                        bool_str(has_reason),
                        bool_str(has_extraction),
                        tag
                    ));
                }
            }
        }
    }

    for &from in LIFECYCLE_STATES.iter() {
        for &to in LIFECYCLE_STATES.iter() {
            lines.push(format!(
                "LIFECYCLE|{}|{}|{}",
                from.as_str(),
                to.as_str(),
                bool_str(lifecycle_can_transition(from, to))
            ));
        }
    }

    for &local in LIFECYCLE_STATES.iter() {
        for &cand in LIFECYCLE_STATES.iter() {
            for &same_sup in [false, true].iter() {
                for &content_changed in [false, true].iter() {
                    for &concurrent in [false, true].iter() {
                        let tag =
                            classify_merge_decision_tag(formal_kernel::classify_merge_decision(
                                local,
                                cand,
                                same_sup,
                                content_changed,
                                concurrent,
                            ));
                        lines.push(format!(
                            "MESH_MERGE|{}|{}|{}|{}|{}|{}",
                            local.as_str(),
                            cand.as_str(),
                            bool_str(same_sup),
                            bool_str(content_changed),
                            bool_str(concurrent),
                            tag
                        ));
                    }
                }
            }
        }
    }

    for &(cand, manifest) in [(1u32, 2u32), (2, 2), (3, 2)].iter() {
        lines.push(format!(
            "EPOCH_GATE|{}|{}|{}",
            cand,
            manifest,
            epoch_gate_tag(cand, manifest)
        ));
    }

    // TASK_CLAIM: 24 rows (4 statuses × 2 same_session × 3 age cases)
    let age_cases: [(&str, Option<i64>); 3] =
        [("none", None), ("live", Some(1800)), ("stale", Some(3601))];
    for &status in TASK_STATUSES.iter() {
        for &same_session in [false, true].iter() {
            for &(age_label, age_secs) in age_cases.iter() {
                let tag = claim_decision_tag(evaluate_claim_decision(
                    status,
                    same_session,
                    age_secs,
                    3600,
                ));
                lines.push(format!(
                    "TASK_CLAIM|{}|{}|{}|{}",
                    status.as_str(),
                    bool_str(same_session),
                    age_label,
                    tag
                ));
            }
        }
    }

    // SOVEREIGNTY_GATE: 32 rows (5 booleans)
    for &perm in [false, true].iter() {
        for &shareable in [false, true].iter() {
            for &counter in [false, true].iter() {
                for &acl_empty in [false, true].iter() {
                    for &in_acl in [false, true].iter() {
                        let tag = sovereignty_decision_tag(evaluate_sovereignty_gate(
                            perm, shareable, counter, acl_empty, in_acl,
                        ));
                        lines.push(format!(
                            "SOVEREIGNTY_GATE|{}|{}|{}|{}|{}|{}",
                            bool_str(perm),
                            bool_str(shareable),
                            bool_str(counter),
                            bool_str(acl_empty),
                            bool_str(in_acl),
                            tag
                        ));
                    }
                }
            }
        }
    }

    // IMPORT_PRECHECK: 8 rows (3 booleans)
    for &row_exists in [false, true].iter() {
        for &has_prov in [false, true].iter() {
            for &same_upd in [false, true].iter() {
                let tag =
                    import_precheck_tag(evaluate_import_precheck(row_exists, has_prov, same_upd));
                lines.push(format!(
                    "IMPORT_PRECHECK|{}|{}|{}|{}",
                    bool_str(row_exists),
                    bool_str(has_prov),
                    bool_str(same_upd),
                    tag
                ));
            }
        }
    }

    // PROCEDURE_FIRE: 60 rows (5 trusts × 2 imported × 2 lapsed × 3 budgets)
    let trusts = [
        KernelProcedureTrust::Untrusted,
        KernelProcedureTrust::Trusted,
        KernelProcedureTrust::Promoted,
        KernelProcedureTrust::Local,
        KernelProcedureTrust::AbsentOrUnknown,
    ];
    let lapsed_cases: [(&str, Option<i64>); 2] = [("live", None), ("lapsed", Some(1000))];
    let budget_cases: [(&str, Option<u64>); 3] =
        [("unbounded", None), ("zero", Some(0)), ("two", Some(2))];
    for &trust in trusts.iter() {
        for &imported in [false, true].iter() {
            for &(lapsed_label, valid_to) in lapsed_cases.iter() {
                for &(budget_label, remaining) in budget_cases.iter() {
                    let tag = procedure_fire_tag(evaluate_procedure_fire(
                        trust, imported, valid_to, 1000, remaining, 0,
                    ));
                    lines.push(format!(
                        "PROCEDURE_FIRE|{}|{}|{}|{}|{}",
                        trust.as_str(),
                        bool_str(imported),
                        lapsed_label,
                        budget_label,
                        tag
                    ));
                }
            }
        }
    }

    // TWIN_CRDT_STATUS: 16 rows (4×4 statuses)
    for &a in TASK_STATUSES.iter() {
        for &b in TASK_STATUSES.iter() {
            let joined = join_task_status(a, b);
            lines.push(format!(
                "TWIN_CRDT_STATUS|{}|{}|{}",
                a.as_str(),
                b.as_str(),
                joined.as_str()
            ));
        }
    }

    // TWIN_CRDT_SEQ: 6 rows (2 already_seen × 3 seq)
    for &already_seen in [false, true].iter() {
        for &seq in [1u64, 2, 3].iter() {
            let tag = seq_decision_tag(evaluate_seq_continuity(already_seen, 2, seq));
            lines.push(format!(
                "TWIN_CRDT_SEQ|{}|{}|{}",
                bool_str(already_seen),
                seq,
                tag
            ));
        }
    }

    // RETENTION_GATE: 32 rows (5 booleans)
    for &budget in [false, true].iter() {
        for &non_empty in [false, true].iter() {
            for &found in [false, true].iter() {
                for &bound in [false, true].iter() {
                    for &matches_wt in [false, true].iter() {
                        let tag = retention_verdict_tag(evaluate_retention_gate(
                            budget, non_empty, found, bound, matches_wt,
                        ));
                        lines.push(format!(
                            "RETENTION_GATE|{}|{}|{}|{}|{}|{}",
                            bool_str(budget),
                            bool_str(non_empty),
                            bool_str(found),
                            bool_str(bound),
                            bool_str(matches_wt),
                            tag
                        ));
                    }
                }
            }
        }
    }

    // CLAIM_SCOPE_GATE: 32 rows (5 booleans)
    for &is_scoped in [false, true].iter() {
        for &caller_id in [false, true].iter() {
            for &claim_attr in [false, true].iter() {
                for &branch_match in [false, true].iter() {
                    for &wt_match in [false, true].iter() {
                        let tag = claim_scope_verdict_tag(evaluate_claim_scope_gate(
                            is_scoped,
                            caller_id,
                            claim_attr,
                            branch_match,
                            wt_match,
                        ));
                        lines.push(format!(
                            "CLAIM_SCOPE_GATE|{}|{}|{}|{}|{}|{}",
                            bool_str(is_scoped),
                            bool_str(caller_id),
                            bool_str(claim_attr),
                            bool_str(branch_match),
                            bool_str(wt_match),
                            tag
                        ));
                    }
                }
            }
        }
    }

    // ACTION_GATE: 32 rows (5 booleans)
    for &principal in [false, true].iter() {
        for &params in [false, true].iter() {
            for &sparql in [false, true].iter() {
                for &twin in [false, true].iter() {
                    for &shacl in [false, true].iter() {
                        let tag = action_gate_tag(evaluate_action_gate(
                            principal, params, sparql, twin, shacl,
                        ));
                        lines.push(format!(
                            "ACTION_GATE|{}|{}|{}|{}|{}|{}",
                            bool_str(principal),
                            bool_str(params),
                            bool_str(sparql),
                            bool_str(twin),
                            bool_str(shacl),
                            tag
                        ));
                    }
                }
            }
        }
    }

    // ACTION_REVERT: 32 rows (2 receipt states × 4 booleans)
    let receipt_states = [
        ActionReceiptState::Active,
        ActionReceiptState::AlreadyReverted,
    ];
    for &rstate in receipt_states.iter() {
        for &cas in [false, true].iter() {
            for &force in [false, true].iter() {
                for &owner_admin in [false, true].iter() {
                    for &editor in [false, true].iter() {
                        let tag = action_revert_tag(evaluate_action_revert(
                            rstate,
                            cas,
                            force,
                            owner_admin,
                            editor,
                        ));
                        lines.push(format!(
                            "ACTION_REVERT|{}|{}|{}|{}|{}|{}",
                            rstate.as_str(),
                            bool_str(cas),
                            bool_str(force),
                            bool_str(owner_admin),
                            bool_str(editor),
                            tag
                        ));
                    }
                }
            }
        }
    }

    // ACTION_TASK_EFFECT: 16 rows (4 current task statuses × 4 requested target statuses)
    let target_statuses = [
        ActionTaskTargetStatus::Open,
        ActionTaskTargetStatus::InProgress,
        ActionTaskTargetStatus::Blocked,
        ActionTaskTargetStatus::Done,
    ];
    for &curr in TASK_STATUSES.iter() {
        for &req in target_statuses.iter() {
            let tag = action_task_effect_tag(evaluate_action_task_effect(curr, req));
            lines.push(format!(
                "ACTION_TASK_EFFECT|{}|{}|{}",
                curr.as_str(),
                req.as_str(),
                tag
            ));
        }
    }

    // ACTION_SHAREABLE: 4 rows (2 kinds × 2 explicit_shareable)
    let kinds = [
        ActionMemoryKind::ActionType,
        ActionMemoryKind::ActionReceipt,
    ];
    for &kind in kinds.iter() {
        for &explicit in [false, true].iter() {
            let shareable = is_action_memory_shareable(kind, explicit);
            lines.push(format!(
                "ACTION_SHAREABLE|{}|{}|{}",
                kind.as_str(),
                bool_str(explicit),
                bool_str(shareable)
            ));
        }
    }

    // HYBRID_SLOTS: 72 rows (4 limits × 3 avail_norm × 3 avail_exp × 2 avail_other)
    for &limit in [2u32, 4, 5, 8].iter() {
        for &avail_norm in [0u32, 2, 4].iter() {
            for &avail_exp in [0u32, 1, 3].iter() {
                for &avail_other in [0u32, 3].iter() {
                    let alloc = allocate_hybrid_slots(limit, avail_norm, avail_exp, avail_other);
                    lines.push(format!(
                        "HYBRID_SLOTS|{}|{}|{}|{}|{}|{}|{}",
                        limit,
                        avail_norm,
                        avail_exp,
                        avail_other,
                        alloc.normative_taken,
                        alloc.expansion_taken,
                        alloc.remaining_taken
                    ));
                }
            }
        }
    }

    // TWIN_BOUNDS: 5 rows
    let bound_cases: [(i64, i64, i64, i64); 5] = [
        (10, 0, 0, 100),
        (0, 100, 20, 10),
        (0, 10, 20, 30),
        (-50, 200, 0, 120),
        (0, 100, -10, 80),
    ];
    for &(imin, imax, tmin, tmax) in bound_cases.iter() {
        let tag = match intersect_var_bounds(imin, imax, tmin, tmax) {
            Err(TwinBoundError::InvertedInterface) => "err_inverted_iface".to_string(),
            Err(TwinBoundError::InvertedTwin) => "err_inverted_twin".to_string(),
            Err(TwinBoundError::Disjoint) => "err_disjoint".to_string(),
            Ok((emin, emax)) => format!("ok_{}_{}", emin, emax),
        };
        lines.push(format!(
            "TWIN_BOUNDS|{}|{}|{}|{}|{}",
            imin, imax, tmin, tmax, tag
        ));
    }

    // TWIN_REDUCER: 21 rows (7 ops × 3 inputs)
    let reducer_ops = [
        ReducerOpKind::Count,
        ReducerOpKind::Sum,
        ReducerOpKind::Min,
        ReducerOpKind::Max,
        ReducerOpKind::Avg,
        ReducerOpKind::AllInBounds,
        ReducerOpKind::AnyOutOfBounds,
    ];
    let reducer_inputs = [
        ("empty", 0u32, 0i64, 0i64, 0i64, false),
        ("populated_ok", 3, 45, 10, 20, false),
        ("populated_oob", 3, 45, 10, 20, true),
    ];
    for &op in reducer_ops.iter() {
        for &(label, cnt, sum_v, min_v, max_v, oob) in reducer_inputs.iter() {
            let out = evaluate_reducer_total(op, cnt, sum_v, min_v, max_v, oob);
            lines.push(format!(
                "TWIN_REDUCER|{}|{}|{}|{}|{}",
                op.as_str(),
                label,
                out.numeric_val,
                bool_str(out.bool_val),
                bool_str(out.is_defined)
            ));
        }
    }

    // CROSS_FILE_CALL: 96 rows (2 self × 2 same × 2 imported × 3 ws_count × 2 ident_len × 2 is_std)
    for &self_call in [false, true].iter() {
        for &same_file in [false, true].iter() {
            for &imported in [false, true].iter() {
                for &ws_count in [0u32, 1, 2].iter() {
                    for &ident_len in [3u32, 6].iter() {
                        for &is_std in [false, true].iter() {
                            let tag = resolve_cross_file_call(
                                self_call, same_file, imported, ws_count, ident_len, is_std,
                            )
                            .as_tag();
                            lines.push(format!(
                                "CROSS_FILE_CALL|{}|{}|{}|{}|{}|{}|{}",
                                bool_str(self_call),
                                bool_str(same_file),
                                bool_str(imported),
                                ws_count,
                                ident_len,
                                bool_str(is_std),
                                tag
                            ));
                        }
                    }
                }
            }
        }
    }

    for &live_present in [false, true].iter() {
        for &staged_retired in [false, true].iter() {
            for &staged_upsert in [false, true].iter() {
                let tag =
                    resolve_scenario_row(live_present, staged_retired, staged_upsert).as_tag();
                lines.push(format!(
                    "SCENARIO_ROW|{}|{}|{}|{}",
                    bool_str(live_present),
                    bool_str(staged_retired),
                    bool_str(staged_upsert),
                    tag
                ));
            }
        }
    }

    for &expired in [false, true].iter() {
        for &cas_match in [false, true].iter() {
            for &rebac_ok in [false, true].iter() {
                for &enforce_shacl in [false, true].iter() {
                    for &shacl_violations in [0u32, 1, 2].iter() {
                        let tag = evaluate_scenario_commit(
                            expired,
                            cas_match,
                            rebac_ok,
                            enforce_shacl,
                            shacl_violations,
                        )
                        .as_tag();
                        lines.push(format!(
                            "SCENARIO_COMMIT|{}|{}|{}|{}|{}|{}",
                            bool_str(expired),
                            bool_str(cas_match),
                            bool_str(rebac_ok),
                            bool_str(enforce_shacl),
                            shacl_violations,
                            tag
                        ));
                    }
                }
            }
        }
    }

    for &is_test_or_doc in [false, true].iter() {
        for &is_foundation in [false, true].iter() {
            for &is_entrypoint in [false, true].iter() {
                for &in_deg in [0u32, 1, 2].iter() {
                    for &out_deg in [0u32, 1, 2].iter() {
                        let cat = classify_tour_stop(
                            is_test_or_doc,
                            is_foundation,
                            is_entrypoint,
                            in_deg,
                            out_deg,
                        );
                        lines.push(format!(
                            "DIFF_TOUR|{}|{}|{}|{}|{}|{}|{}",
                            bool_str(is_test_or_doc),
                            bool_str(is_foundation),
                            bool_str(is_entrypoint),
                            in_deg,
                            out_deg,
                            cat.as_tag(),
                            cat.stop_number()
                        ));
                    }
                }
            }
        }
    }

    for &q_non_empty in [false, true].iter() {
        for &is_builtin in [false, true].iter() {
            for &is_existing in [false, true].iter() {
                for &existing_count in [0u32, 15, 16, 17].iter() {
                    let dec = evaluate_insight_define(
                        q_non_empty,
                        is_builtin,
                        is_existing,
                        existing_count,
                        16,
                    );
                    lines.push(format!(
                        "INSIGHT_DEFINE|{}|{}|{}|{}|{}",
                        bool_str(q_non_empty),
                        bool_str(is_builtin),
                        bool_str(is_existing),
                        existing_count,
                        dec.as_tag()
                    ));
                }
            }
        }
    }

    for &vf in [10i64, 20].iter() {
        for &has_vt in [false, true].iter() {
            for &vt in [15i64, 25].iter() {
                for &as_of in [5i64, 15, 20, 25].iter() {
                    let valid = is_memory_valid_at(vf, has_vt, vt, as_of);
                    lines.push(format!(
                        "INSIGHT_VALID_AT|{}|{}|{}|{}|{}",
                        vf,
                        bool_str(has_vt),
                        vt,
                        as_of,
                        bool_str(valid)
                    ));
                }
            }
        }
    }

    // TAXONOMY_CLASSIFY: 16 rows (4 bools)
    for d in [false, true] {
        for t in [false, true] {
            for s in [false, true] {
                for g in [false, true] {
                    let st = formal_kernel::classify_epistemic_status(d, t, s, g);
                    lines.push(format!(
                        "TAXONOMY_CLASSIFY|{}|{}|{}|{}|{}",
                        bool_str(d),
                        bool_str(t),
                        bool_str(s),
                        bool_str(g),
                        st.as_str()
                    ));
                }
            }
        }
    }

    // TAXONOMY_DAG: 9 rows
    let dag_cases: [(&str, &[(u32, u32)]); 9] = [
        ("empty", &[] as &[(u32, u32)]),
        ("single_edge", &[(1, 2)]),
        ("self_loop", &[(1, 1)]),
        ("two_cycle", &[(1, 2), (2, 1)]),
        ("three_cycle", &[(1, 2), (2, 3), (3, 1)]),
        ("diamond", &[(1, 2), (1, 3), (2, 4), (3, 4)]),
        ("four_cycle", &[(1, 2), (2, 3), (3, 4), (4, 1)]),
        ("disconnected_cycle", &[(1, 2), (3, 4), (4, 5), (5, 3)]),
        ("multi_parent_dag", &[(1, 3), (2, 3), (3, 4), (1, 4)]),
    ];
    for (label, edges) in dag_cases.iter() {
        lines.push(format!(
            "TAXONOMY_DAG|{}|{}",
            label,
            formal_kernel::validate_taxonomy_dag(edges)
        ));
    }

    // TAXONOMY_DAG_3NODE: all 64 3-node non-self directed graph topologies
    let candidate_3_edges: [(u32, u32); 6] = [(0, 1), (0, 2), (1, 0), (1, 2), (2, 0), (2, 1)];
    for mask in 0u32..64u32 {
        let mut edges = Vec::new();
        for (bit, &edge) in candidate_3_edges.iter().enumerate() {
            if (mask >> bit) & 1 == 1 {
                edges.push(edge);
            }
        }
        lines.push(format!(
            "TAXONOMY_DAG_3NODE|{}|{}",
            mask,
            formal_kernel::validate_taxonomy_dag(&edges)
        ));
    }

    // TAXONOMY_SKOS: broader chain 0 -> 1 -> 2 + cyclic broader rejection
    type SkosCase<'a> = (&'a str, &'a [(u32, u32)], &'a [(u32, u32)]);
    let skos_broader: &[(u32, u32)] = &[(0, 1), (1, 2)];
    let skos_cases: [SkosCase; 10] = [
        ("related_direct", &[(0, 1)], &[] as &[(u32, u32)]),
        ("related_trans", &[(0, 2)], &[] as &[(u32, u32)]),
        ("related_reverse", &[(1, 0)], &[] as &[(u32, u32)]),
        ("related_reverse_trans", &[(2, 0)], &[] as &[(u32, u32)]),
        ("related_self", &[(0, 0)], &[] as &[(u32, u32)]),
        ("contrast_trans", &[] as &[(u32, u32)], &[(0, 2)]),
        ("contrast_reverse", &[] as &[(u32, u32)], &[(2, 1)]),
        ("contrast_self", &[] as &[(u32, u32)], &[(1, 1)]),
        ("unrelated", &[(0, 5)], &[] as &[(u32, u32)]),
        ("empty_all", &[] as &[(u32, u32)], &[] as &[(u32, u32)]),
    ];
    for (label, rel, con) in skos_cases.iter() {
        lines.push(format!(
            "TAXONOMY_SKOS|{}|{}",
            label,
            formal_kernel::validate_skos_disjointness(skos_broader, rel, con)
        ));
    }
    let cyclic_broader: &[(u32, u32)] = &[(0, 1), (1, 0)];
    lines.push(format!(
        "TAXONOMY_SKOS|cyclic_broader_empty|{}",
        formal_kernel::validate_skos_disjointness(cyclic_broader, &[], &[])
    ));
    lines.push(format!(
        "TAXONOMY_SKOS|cyclic_broader_unrelated|{}",
        formal_kernel::validate_skos_disjointness(cyclic_broader, &[(2, 3)], &[])
    ));

    // TAXONOMY_COVERAGE: 7 rows
    let coverage_cases: [(&str, Vec<EpistemicStatus>, u32); 7] = [
        ("empty_zero", Vec::new(), 0),
        ("seed", vec![EpistemicStatus::Seed], 0),
        ("verified", vec![EpistemicStatus::Verified], 0),
        (
            "seed_verified",
            vec![EpistemicStatus::Seed, EpistemicStatus::Verified],
            0,
        ),
        (
            "seed_verified_unmapped",
            vec![EpistemicStatus::Seed, EpistemicStatus::Verified],
            1,
        ),
        ("four_seed", vec![EpistemicStatus::Seed; 4], 0),
        (
            "mixed",
            vec![
                EpistemicStatus::Seed,
                EpistemicStatus::Defined,
                EpistemicStatus::Synthesized,
                EpistemicStatus::Verified,
            ],
            0,
        ),
    ];
    for (label, statuses, unmapped) in coverage_cases.iter() {
        lines.push(format!(
            "TAXONOMY_COVERAGE|{}|{}",
            label,
            formal_kernel::compute_epistemic_coverage_bps(statuses, *unmapped)
        ));
    }

    // TAXONOMY_REALIZATION: 6 rows
    let realization_cases: [&str; 6] = [
        "unimplemented",
        "partial",
        "unknown",
        "implemented",
        "external_only",
        "other",
    ];
    for status in realization_cases.iter() {
        lines.push(format!(
            "TAXONOMY_REALIZATION|{}|{}",
            status,
            formal_kernel::realization_status_gap_bps(status)
        ));
    }

    // TAXONOMY_FRONTIER: 6 rows
    let frontier_cases: [(u32, u32, u32, bool, bool); 6] = [
        (5000, 0, 3, true, true),
        (5000, 2000, 3, false, true),
        (5000, 2000, 3, true, true),
        (10000, 10000, 10, true, true),
        (20000, 20000, 20, true, true),
        (3000, 5000, 2, false, false),
    ];
    let frontier_labels: [&str; 6] = [
        "zero_gap",
        "low_gap_no_frontier",
        "low_gap_frontier",
        "full",
        "capped",
        "partial",
    ];
    for (i, (axis, gap, conn, frontier, cite)) in frontier_cases.iter().enumerate() {
        lines.push(format!(
            "TAXONOMY_FRONTIER|{}|{}",
            frontier_labels[i],
            formal_kernel::compute_frontier_leverage_bps(*axis, *gap, *conn, *frontier, *cite)
        ));
    }

    // WORKFLOW_STAGE: 144 rows (8 stages × 18 event instances — the full
    // guard-relevant space of KernelWorkflowEvent)
    let wf_stages: [formal_kernel::KernelWorkflowStage; 8] = [
        formal_kernel::KernelWorkflowStage::Unpreflighted,
        formal_kernel::KernelWorkflowStage::ContextLoaded,
        formal_kernel::KernelWorkflowStage::Claimed,
        formal_kernel::KernelWorkflowStage::Mutating,
        formal_kernel::KernelWorkflowStage::BreathRequired,
        formal_kernel::KernelWorkflowStage::PreCheckpoint,
        formal_kernel::KernelWorkflowStage::PrePublish,
        formal_kernel::KernelWorkflowStage::Completed,
    ];
    let wf_events: [(&str, formal_kernel::KernelWorkflowEvent); 18] = [
        (
            "load_context",
            formal_kernel::KernelWorkflowEvent::LoadContext,
        ),
        (
            "claim_ok",
            formal_kernel::KernelWorkflowEvent::ClaimTask {
                has_valid_scope: true,
            },
        ),
        (
            "claim_bad_scope",
            formal_kernel::KernelWorkflowEvent::ClaimTask {
                has_valid_scope: false,
            },
        ),
        (
            "mutate_under_limit",
            formal_kernel::KernelWorkflowEvent::Mutate {
                mutations_since_note_after: 1,
                breath_limit: 5,
            },
        ),
        (
            "mutate_at_limit",
            formal_kernel::KernelWorkflowEvent::Mutate {
                mutations_since_note_after: 5,
                breath_limit: 5,
            },
        ),
        (
            "record_breath",
            formal_kernel::KernelWorkflowEvent::RecordBreathNote,
        ),
        (
            "pre_checkpoint_ok",
            formal_kernel::KernelWorkflowEvent::TriggerPreCheckpoint { has_receipt: true },
        ),
        (
            "pre_checkpoint_no_receipt",
            formal_kernel::KernelWorkflowEvent::TriggerPreCheckpoint { has_receipt: false },
        ),
        (
            "checkpoint_passed",
            formal_kernel::KernelWorkflowEvent::CheckpointPassed,
        ),
        (
            "pre_publish_ok",
            formal_kernel::KernelWorkflowEvent::TriggerPrePublish {
                task_gate_ok: true,
                doc_gate_ok: true,
            },
        ),
        (
            "pre_publish_bad_doc",
            formal_kernel::KernelWorkflowEvent::TriggerPrePublish {
                task_gate_ok: true,
                doc_gate_ok: false,
            },
        ),
        (
            "pre_publish_bad_task",
            formal_kernel::KernelWorkflowEvent::TriggerPrePublish {
                task_gate_ok: false,
                doc_gate_ok: true,
            },
        ),
        (
            "pre_publish_both_bad",
            formal_kernel::KernelWorkflowEvent::TriggerPrePublish {
                task_gate_ok: false,
                doc_gate_ok: false,
            },
        ),
        (
            "publish_passed",
            formal_kernel::KernelWorkflowEvent::PublishPassed,
        ),
        (
            "complete_ok",
            formal_kernel::KernelWorkflowEvent::CompleteTask {
                has_extracted_memory: true,
                dirty_targets: 0,
            },
        ),
        (
            "complete_no_mem",
            formal_kernel::KernelWorkflowEvent::CompleteTask {
                has_extracted_memory: false,
                dirty_targets: 0,
            },
        ),
        (
            "complete_dirty",
            formal_kernel::KernelWorkflowEvent::CompleteTask {
                has_extracted_memory: true,
                dirty_targets: 1,
            },
        ),
        (
            "complete_no_mem_dirty",
            formal_kernel::KernelWorkflowEvent::CompleteTask {
                has_extracted_memory: false,
                dirty_targets: 1,
            },
        ),
    ];
    fn wf_stage_str(s: formal_kernel::KernelWorkflowStage) -> &'static str {
        match s {
            formal_kernel::KernelWorkflowStage::Unpreflighted => "unpreflighted",
            formal_kernel::KernelWorkflowStage::ContextLoaded => "context_loaded",
            formal_kernel::KernelWorkflowStage::Claimed => "claimed",
            formal_kernel::KernelWorkflowStage::Mutating => "mutating",
            formal_kernel::KernelWorkflowStage::BreathRequired => "breath_required",
            formal_kernel::KernelWorkflowStage::PreCheckpoint => "pre_checkpoint",
            formal_kernel::KernelWorkflowStage::PrePublish => "pre_publish",
            formal_kernel::KernelWorkflowStage::Completed => "completed",
        }
    }
    for stage in wf_stages.iter() {
        for (label, event) in wf_events.iter() {
            let tag = match formal_kernel::evaluate_workflow_stage_transition(*stage, *event) {
                Ok(next) => format!("ok:{}", wf_stage_str(next)),
                Err(e) => format!("err:{e}"),
            };
            lines.push(format!(
                "WORKFLOW_STAGE|{}|{}|{}",
                wf_stage_str(*stage),
                label,
                tag
            ));
        }
    }

    // CONNECTOR_RESOLVE_POST_BOUNDED: 4 rows (neurostrata-zbe7 Kani parity)
    type ResolvePostCase = (&'static str, usize, u32, u32, u32, u32, u32, bool);
    let resolve_post_cases: [ResolvePostCase; 4] = [
        ("within_both", 2, 2, 10, 32, 64, 1024, true),
        ("over_req_under_schema", 5, 2, 10, 32, 64, 1024, true),
        ("over_u32_wrap", 4294967296, 2, 10, 32, 64, 1024, true),
        ("bad_digest", 2, 2, 10, 32, 64, 1024, false),
    ];
    for &(label, obs_r, req_r, max_r, obs_b, req_b, max_b, sdm) in resolve_post_cases.iter() {
        let v = evaluate_connector_post_gate(formal_kernel::build_resolve_post_gate_input(
            obs_r, req_r, max_r, obs_b, req_b, max_b, sdm,
        ));
        lines.push(format!(
            "CONNECTOR_RESOLVE_POST_BOUNDED|{}|{}",
            label,
            connector_post_verdict_tag(v)
        ));
    }
    for ptr_ok in [false, true] {
        for cas_ok in [false, true] {
            let v = evaluate_connector_post_gate(formal_kernel::build_writeback_post_gate_input(
                10, 1024, ptr_ok, cas_ok,
            ));
            lines.push(format!(
                "CONNECTOR_WRITEBACK_POST_BOUNDED|{}|{}|{}",
                bool_str(ptr_ok),
                bool_str(cas_ok),
                connector_post_verdict_tag(v)
            ));
        }
    }
    // PROCEDURE_NEXT_DUE: 9 rows (neurostrata-zbe7 Kani parity)
    type NextDueCase = (
        &'static str,
        i64,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
    );
    let next_due_cases: [NextDueCase; 9] = [
        ("never_performed", 1000000, None, Some(3), None, None),
        ("days_positive", 1000000, Some(900000), Some(2), None, None),
        ("days_zero", 1000000, Some(900000), Some(0), None, None),
        ("days_negative", 1000000, Some(900000), Some(-5), None, None),
        (
            "days_overflow",
            1000000,
            Some(1000000),
            Some(9223372036854775807),
            None,
            None,
        ),
        (
            "sessions_default",
            1000000,
            Some(900000),
            None,
            Some(4),
            None,
        ),
        (
            "sessions_explicit",
            1000000,
            Some(900000),
            None,
            Some(4),
            Some(2),
        ),
        (
            "sessions_negative_elapsed",
            1000000,
            Some(900000),
            None,
            Some(4),
            Some(-3),
        ),
        (
            "sessions_overflow",
            1000000,
            Some(1000000),
            None,
            Some(9223372036854775807),
            None,
        ),
    ];
    for &(label, now, last, day_cad, sess_cad, sess_elapsed) in next_due_cases.iter() {
        let due =
            formal_kernel::compute_procedure_next_due(now, last, day_cad, sess_cad, sess_elapsed);
        lines.push(format!("PROCEDURE_NEXT_DUE|{}|{}", label, due));
    }

    // WAL_VERDICT: 32 rows (4 commit-evidence values x commit_proved x
    // checkpoint_observed x applied_at_least_one) — the removed-frozen-WAL
    // verdict of `#228`, shared with the recovery funnel through the formal
    // kernel so there is exactly one truth table.
    let wal_evidences: [formal_kernel::KernelWalCommitEvidence; 4] = [
        formal_kernel::KernelWalCommitEvidence::CommitSeen,
        formal_kernel::KernelWalCommitEvidence::NoCommit,
        formal_kernel::KernelWalCommitEvidence::Unverifiable,
        formal_kernel::KernelWalCommitEvidence::Unreadable,
    ];
    for evidence in wal_evidences.iter() {
        for commit_proved in [false, true].iter() {
            for checkpoint_observed in [false, true].iter() {
                for applied_at_least_one in [false, true].iter() {
                    let verdict = formal_kernel::evaluate_removed_wal_verdict(
                        *evidence,
                        *commit_proved,
                        *checkpoint_observed,
                        *applied_at_least_one,
                    );
                    lines.push(format!(
                        "WAL_VERDICT|{}|{}|{}|{}|{}",
                        formal_kernel::wal_commit_evidence_tag(*evidence),
                        bool_str(*commit_proved),
                        bool_str(*checkpoint_observed),
                        bool_str(*applied_at_least_one),
                        formal_kernel::removed_wal_verdict_tag(verdict),
                    ));
                }
            }
        }
    }

    // DERIVATION_STEP: 52 rows (26 rules x 2: min, min-1). >= min is valid.
    for &rule in formal_kernel::KernelOwl2RlRuleId::ALL.iter() {
        let pc_ok = rule.min_premise_count();
        let pc_bad = pc_ok.saturating_sub(1);
        let v_ok = formal_kernel::step_verdict_tag(
            formal_kernel::verify_derivation_step_structure(rule, pc_ok),
        );
        let v_bad = formal_kernel::step_verdict_tag(
            formal_kernel::verify_derivation_step_structure(rule, pc_bad),
        );
        lines.push(format!(
            "DERIVATION_STEP|{}|{}|{}",
            formal_kernel::owl2rl_rule_tag(rule),
            pc_ok,
            v_ok
        ));
        lines.push(format!(
            "DERIVATION_STEP|{}|{}|{}",
            formal_kernel::owl2rl_rule_tag(rule),
            pc_bad,
            v_bad
        ));
    }

    // CERT_CHAIN: 7 rows (baseline 4 + short-list fail-closed 2 + 64-cap 1)
    let short_cases: [(
        usize,
        &[bool],
        &[bool],
        &[bool],
        bool,
    ); 6] = [
        (2, &[true, true], &[true, true], &[true, true], true),
        (2, &[true, true], &[true, true], &[true, true], false),
        (2, &[true, true], &[true, false], &[true, true], true),
        (2, &[true, false], &[true, true], &[true, true], true),
        (3, &[true, true], &[true, true], &[true, true], true),
        (
            3,
            &[true, true, true],
            &[true, true, false],
            &[true, true, true],
            true,
        ),
    ];
    for (step_count, rv, pc, cc, proof) in short_cases.iter() {
        let verdict = formal_kernel::verify_certificate_chain_slices(*step_count, rv, pc, cc, *proof);
        lines.push(format!(
            "CERT_CHAIN|{}|{}|{}|{}|{}|{}",
            step_count,
            bool_slice_str(rv),
            bool_slice_str(pc),
            bool_slice_str(cc),
            proof,
            formal_kernel::cert_verdict_tag(verdict),
        ));
    }
    // I1: legacy window is a hard cap; 65 steps must fail closed (matches Lean).
    let all_true_64 = [true; 64];
    let cap_verdict = formal_kernel::verify_certificate_chain(
        65,
        &all_true_64,
        &all_true_64,
        &all_true_64,
        true,
    );
    lines.push(format!(
        "CERT_CHAIN|65|{}|{}|{}|true|{}",
        bool_array_str(&all_true_64),
        bool_array_str(&all_true_64),
        bool_array_str(&all_true_64),
        formal_kernel::cert_verdict_tag(cap_verdict),
    ));

    // CERT_HASH: 2 rows (match / mismatch)
    for (expected, actual) in [(42u64, 42u64), (42u64, 99u64)].iter() {
        let verdict = formal_kernel::verify_certificate_hash(*expected, *actual);
        lines.push(format!(
            "CERT_HASH|{}|{}|{}",
            expected,
            actual,
            formal_kernel::hash_verdict_tag(verdict),
        ));
    }

    // CERT_FULL: full certificate verifier (no 64-step bypass)
    let tag_sco = kernel_rule_tag(formal_kernel::KernelOwl2RlRuleId::CaxSco);
    let tag_trp = kernel_rule_tag(formal_kernel::KernelOwl2RlRuleId::PrpTrp);
    let step_sco = KernelCertificateStep { rule_tag: tag_sco, premise_count: 2, premises_available: true };
    let step_trp = KernelCertificateStep { rule_tag: tag_trp, premise_count: 3, premises_available: true };
    let step_bad_rule = KernelCertificateStep { rule_tag: 255, premise_count: 2, premises_available: true };
    let step_bad_pc = KernelCertificateStep { rule_tag: tag_sco, premise_count: 1, premises_available: true };
    let step_bad_chain = KernelCertificateStep { rule_tag: tag_sco, premise_count: 2, premises_available: false };
    let big_steps: Vec<KernelCertificateStep> = (0..70).map(|_| step_sco).collect();
    let full_cases: [(u8, bool, &[KernelCertificateStep], bool, &str); 8] = [
        (1, true, &[step_sco, step_trp], true, "[8:2:true,2:3:true]"),
        (1, true, &[step_bad_rule], true, "[255:2:true]"),
        (1, true, &[step_bad_pc], true, "[8:1:true]"),
        (1, true, &[step_bad_chain], true, "[8:2:false]"),
        (1, false, &[step_sco], true, "[8:2:true]"),
        (2, true, &[step_sco], true, "[8:2:true]"),
        (1, true, &[step_sco], false, "[8:2:true]"),
        (1, true, &big_steps, true, "70x[8:2:true]"),
    ];
    for (profile, digest, steps, proof, label) in full_cases.iter() {
        let verdict = verify_derivation_certificate(*profile, *digest, steps, *proof);
        lines.push(format!(
            "CERT_FULL|{}|{}|{}|{}|{}",
            profile,
            digest,
            label,
            proof,
            match verdict {
                KernelCertificateVerdict::Valid => "valid",
                KernelCertificateVerdict::InvalidProfile => "invalid_profile",
                KernelCertificateVerdict::DigestMismatch => "digest_mismatch",
                KernelCertificateVerdict::MissingProof => "missing_proof",
                KernelCertificateVerdict::UnknownRule => "unknown_rule",
                KernelCertificateVerdict::PremiseCountMismatch => "premise_count_mismatch",
                KernelCertificateVerdict::BrokenChain => "broken_chain",
            }
        ));
    }

    lines
}

/// Locates the `lake` binary: PATH first, then the elan default install dir.
fn find_lake() -> Option<PathBuf> {
    if let Ok(out) = Command::new("which").arg("lake").output() {
        if out.status.success() {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
    }
    let home = std::env::var_os("HOME")?;
    let candidate = PathBuf::from(home).join(".elan").join("bin").join("lake");
    if candidate.is_file() {
        Some(candidate)
    } else {
        None
    }
}

/// The formal kernel must keep compiling as a dependency-free standalone crate
/// so Charon/Aeneas can translate it. This runs the real extract script (which
/// invokes `rustc -D warnings --crate-type=lib` with zero `--extern` flags).
/// Kani runs in its own CI step, so skip the duplicate run here.
#[test]
fn formal_kernel_compiles_standalone_for_aeneas_extraction() {
    let root = repo_root();
    let script = root
        .join("proofs")
        .join("scripts")
        .join("extract-aeneas.sh");
    assert!(
        script.is_file(),
        "missing extract script at {}",
        script.display()
    );

    let status = Command::new("bash")
        .arg(&script)
        .current_dir(&root)
        .env("NEUROSTRATA_SKIP_KANI", "1")
        .status()
        .expect("failed to spawn proofs/scripts/extract-aeneas.sh");
    assert!(status.success(), "extract-aeneas.sh exited non-zero");
}

#[test]
fn ci_kani_wrapper_dispatches_standalone_file_to_driver() {
    let root = repo_root();
    let workflow =
        std::fs::read_to_string(root.join(".github/workflows/ci.yml")).expect("read CI workflow");
    let setup = workflow
        .lines()
        .find(|line| line.trim_start().starts_with("printf '#!/usr/bin/env bash"))
        .expect("Kani wrapper setup")
        .trim();

    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(temp.path().join(".cargo/bin")).expect("Cargo bin dir");
    let bin_dir = temp.path().join(".kani/kani-0.68.0/bin");
    std::fs::create_dir_all(&bin_dir).expect("Kani bin dir");
    let driver = bin_dir.join("kani-driver");
    std::fs::write(
        &driver,
        "#!/usr/bin/env bash\nprintf '%s\\n' \"$@\" > \"$KANI_ARGS_OUT\"\n",
    )
    .expect("write fake driver");
    let mut permissions = std::fs::metadata(&driver)
        .expect("driver metadata")
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o755);
        std::fs::set_permissions(&driver, permissions).expect("driver executable");
    }

    let args_out = temp.path().join("args");
    let status = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "set -euo pipefail\nTC_LIB=/unused\nKANI_VER=0.68.0\n{setup}\nchmod +x \"$HOME/.cargo/bin/kani\"\n\"$HOME/.cargo/bin/kani\" src/formal_kernel.rs"
        ))
        .env("HOME", temp.path())
        .env("KANI_ARGS_OUT", &args_out)
        .status()
        .expect("run CI wrapper");
    assert!(status.success(), "CI wrapper setup failed");
    assert_eq!(
        std::fs::read_to_string(args_out).expect("driver arguments"),
        "src/formal_kernel.rs\n"
    );
}

/// Live parity: the Rust kernel's exhaustive truth table must equal the Lean
/// generated executable's stdout line-for-line.
#[test]
fn rust_formal_kernel_truth_table_matches_lean_cli() {
    let rust_lines = generate_rust_truth_table();
    assert_eq!(
        rust_lines.len(),
        1273,
        "expected 1273 canonical lines (891 base/action/code-intel/ontology + 120 taxonomy + 144 workflow + 17 Kani parity + 32 WAL verdict + 69 derivation-cert)"
    );

    // Section-count verification: 64 TASK_FSM + 16 LIFECYCLE + 128 MESH_MERGE
    // + 3 EPOCH_GATE + 24 TASK_CLAIM + 32 SOVEREIGNTY_GATE + 8 IMPORT_PRECHECK
    // + 60 PROCEDURE_FIRE + 16 TWIN_CRDT_STATUS + 6 TWIN_CRDT_SEQ + 32 RETENTION_GATE
    // + 32 CLAIM_SCOPE_GATE + 32 ACTION_GATE + 32 ACTION_REVERT + 16 ACTION_TASK_EFFECT
    // + 4 ACTION_SHAREABLE + 72 HYBRID_SLOTS + ...,
    // matching the exact generation order in `proofs/Main.lean`.
    let section_counts = rust_lines.iter().fold(HashMap::new(), |mut acc, line| {
        *acc.entry(line.split('|').next().unwrap_or("")).or_insert(0) += 1;
        acc
    });
    assert_eq!(
        *section_counts.get("TASK_FSM").unwrap_or(&0),
        64,
        "expected 64 TASK_FSM rows"
    );
    assert_eq!(
        *section_counts.get("LIFECYCLE").unwrap_or(&0),
        16,
        "expected 16 LIFECYCLE rows"
    );
    assert_eq!(
        *section_counts.get("MESH_MERGE").unwrap_or(&0),
        128,
        "expected 128 MESH_MERGE rows"
    );
    assert_eq!(
        *section_counts.get("EPOCH_GATE").unwrap_or(&0),
        3,
        "expected 3 EPOCH_GATE rows"
    );
    assert_eq!(
        *section_counts.get("TASK_CLAIM").unwrap_or(&0),
        24,
        "expected 24 TASK_CLAIM rows"
    );
    assert_eq!(
        *section_counts.get("SOVEREIGNTY_GATE").unwrap_or(&0),
        32,
        "expected 32 SOVEREIGNTY_GATE rows"
    );
    assert_eq!(
        *section_counts.get("IMPORT_PRECHECK").unwrap_or(&0),
        8,
        "expected 8 IMPORT_PRECHECK rows"
    );
    assert_eq!(
        *section_counts.get("PROCEDURE_FIRE").unwrap_or(&0),
        60,
        "expected 60 PROCEDURE_FIRE rows"
    );
    assert_eq!(
        *section_counts.get("TWIN_CRDT_STATUS").unwrap_or(&0),
        16,
        "expected 16 TWIN_CRDT_STATUS rows"
    );
    assert_eq!(
        *section_counts.get("TWIN_CRDT_SEQ").unwrap_or(&0),
        6,
        "expected 6 TWIN_CRDT_SEQ rows"
    );
    assert_eq!(
        *section_counts.get("RETENTION_GATE").unwrap_or(&0),
        32,
        "expected 32 RETENTION_GATE rows"
    );
    assert_eq!(
        *section_counts.get("CLAIM_SCOPE_GATE").unwrap_or(&0),
        32,
        "expected 32 CLAIM_SCOPE_GATE rows"
    );
    assert_eq!(
        *section_counts.get("ACTION_GATE").unwrap_or(&0),
        32,
        "expected 32 ACTION_GATE rows"
    );
    assert_eq!(
        *section_counts.get("ACTION_REVERT").unwrap_or(&0),
        32,
        "expected 32 ACTION_REVERT rows"
    );
    assert_eq!(
        *section_counts.get("ACTION_TASK_EFFECT").unwrap_or(&0),
        16,
        "expected 16 ACTION_TASK_EFFECT rows"
    );
    assert_eq!(
        *section_counts.get("ACTION_SHAREABLE").unwrap_or(&0),
        4,
        "expected 4 ACTION_SHAREABLE rows"
    );
    assert_eq!(
        *section_counts.get("HYBRID_SLOTS").unwrap_or(&0),
        72,
        "expected 72 HYBRID_SLOTS rows"
    );
    assert_eq!(
        *section_counts.get("TWIN_BOUNDS").unwrap_or(&0),
        5,
        "expected 5 TWIN_BOUNDS rows"
    );
    assert_eq!(
        *section_counts.get("TWIN_REDUCER").unwrap_or(&0),
        21,
        "expected 21 TWIN_REDUCER rows"
    );
    assert_eq!(
        *section_counts.get("CROSS_FILE_CALL").unwrap_or(&0),
        96,
        "expected 96 CROSS_FILE_CALL rows"
    );
    assert_eq!(
        *section_counts.get("SCENARIO_ROW").unwrap_or(&0),
        8,
        "expected 8 SCENARIO_ROW rows"
    );
    assert_eq!(
        *section_counts.get("SCENARIO_COMMIT").unwrap_or(&0),
        48,
        "expected 48 SCENARIO_COMMIT rows"
    );
    assert_eq!(
        *section_counts.get("DIFF_TOUR").unwrap_or(&0),
        72,
        "expected 72 DIFF_TOUR rows"
    );
    assert_eq!(
        *section_counts.get("INSIGHT_DEFINE").unwrap_or(&0),
        32,
        "expected 32 INSIGHT_DEFINE rows"
    );
    assert_eq!(
        *section_counts.get("INSIGHT_VALID_AT").unwrap_or(&0),
        32,
        "expected 32 INSIGHT_VALID_AT rows"
    );
    assert_eq!(
        *section_counts.get("WORKFLOW_STAGE").unwrap_or(&0),
        144,
        "expected 144 WORKFLOW_STAGE rows"
    );
    assert_eq!(
        *section_counts.get("WAL_VERDICT").unwrap_or(&0),
        32,
        "expected 32 WAL_VERDICT rows"
    );
    assert_eq!(
        *section_counts.get("DERIVATION_STEP").unwrap_or(&0),
        52,
        "expected 52 DERIVATION_STEP rows"
    );
    assert_eq!(
        *section_counts.get("CERT_CHAIN").unwrap_or(&0),
        7,
        "expected 7 CERT_CHAIN rows"
    );
    assert_eq!(
        *section_counts.get("CERT_HASH").unwrap_or(&0),
        2,
        "expected 2 CERT_HASH rows"
    );
    assert_eq!(
        *section_counts.get("CERT_FULL").unwrap_or(&0),
        8,
        "expected 8 CERT_FULL rows"
    );

    let lake = find_lake().expect(
        "lake not found on PATH or ~/.elan/bin/lake; install elan to run Lean parity tests",
    );
    let root = repo_root();
    let build_status = Command::new(&lake)
        .arg("--dir")
        .arg("proofs")
        .arg("build")
        .current_dir(&root)
        .status()
        .expect("failed to run lake --dir proofs build");
    assert!(
        build_status.success(),
        "lake --dir proofs build exited non-zero"
    );

    let output = Command::new(&lake)
        .arg("--dir")
        .arg("proofs")
        .arg("exe")
        .arg("neurostrata-proofs")
        .current_dir(&root)
        .output()
        .expect("failed to run lake --dir proofs exe neurostrata-proofs");
    assert!(
        output.status.success(),
        "lake exited non-zero: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lean_lines: Vec<String> = stdout
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();

    assert_eq!(
        lean_lines.len(),
        rust_lines.len(),
        "line count mismatch: rust={} lean={}",
        rust_lines.len(),
        lean_lines.len()
    );

    for (idx, (r, l)) in rust_lines.iter().zip(lean_lines.iter()).enumerate() {
        assert_eq!(r, l, "line {} mismatch:\n  rust: {}\n  lean: {}", idx, r, l);
    }

    // Review-focus rows pinned explicitly so a regression names itself.
    for expected in [
        "MESH_MERGE|superseded|archived|false|true|true|supersede_wins_preserve_edit",
        "MESH_MERGE|archived|superseded|false|true|true|supersede_wins_preserve_edit",
        "MESH_MERGE|superseded|archived|false|false|true|refuse_stale_restore",
        "MESH_MERGE|archived|superseded|false|false|true|apply",
        "TASK_FSM|open|done|true|false|err_extraction_required",
        "TASK_FSM|in_progress|done|true|false|err_extraction_required",
        "TASK_CLAIM|in_progress|true|live|idempotent_same_session",
        "TASK_CLAIM|in_progress|false|live|refuse_live_holder",
        "TASK_CLAIM|in_progress|false|stale|takeover_stale_holder",
        "SOVEREIGNTY_GATE|true|false|false|true|true|not_shareable",
        "SOVEREIGNTY_GATE|true|true|true|true|true|forbidden_runtime_counter",
        "IMPORT_PRECHECK|true|false|false|refuse_private_sovereign",
        "PROCEDURE_FIRE|absent|true|live|two|refuse_untrusted",
        "PROCEDURE_FIRE|local|false|lapsed|two|refuse_lapsed",
        "PROCEDURE_FIRE|local|false|live|zero|refuse_spent",
        "PROCEDURE_FIRE|local|false|live|two|fired_bounded",
        "TWIN_CRDT_STATUS|blocked|in_progress|in_progress",
        "TWIN_CRDT_SEQ|false|3|buffer_out_of_order",
        "RETENTION_GATE|true|true|true|true|false|failed_worktree_moved",
        "RETENTION_GATE|true|true|true|true|true|retained",
        "CLAIM_SCOPE_GATE|false|true|true|true|true|block_unscoped",
        "CLAIM_SCOPE_GATE|true|false|true|false|false|block_uncapturable_scope",
        "CLAIM_SCOPE_GATE|true|true|false|false|false|block_legacy_unattributed",
        "CLAIM_SCOPE_GATE|true|true|true|true|false|block_branch_match",
        "CLAIM_SCOPE_GATE|true|true|true|false|true|block_worktree_match",
        "CLAIM_SCOPE_GATE|true|true|true|false|false|allow_isolated_scope",
        "ACTION_GATE|true|true|true|true|true|allow",
        "ACTION_GATE|false|true|true|true|true|reject_unauthorized",
        "ACTION_GATE|true|false|true|true|true|reject_invalid_params",
        "ACTION_GATE|true|true|false|true|true|reject_sparql_ask",
        "ACTION_GATE|true|true|true|false|true|reject_twin_bounds",
        "ACTION_GATE|true|true|true|true|false|reject_post_shacl",
        "ACTION_REVERT|already_reverted|true|true|true|true|reject_already_reverted",
        "ACTION_REVERT|active|false|false|true|true|reject_stale_head_conflict",
        "ACTION_REVERT|active|false|true|false|true|reject_permission_denied",
        "ACTION_REVERT|active|false|true|true|true|forced_revert_commit",
        "ACTION_REVERT|active|true|false|false|true|revert_commit",
        "ACTION_TASK_EFFECT|open|done|err_done_requires_complete",
        "ACTION_TASK_EFFECT|in_progress|done|err_done_requires_complete",
        "ACTION_TASK_EFFECT|open|in_progress|ok_in_progress",
        "ACTION_SHAREABLE|action_receipt|true|false",
        "ACTION_SHAREABLE|action_receipt|false|false",
        "ACTION_SHAREABLE|action_type|true|true",
        "ACTION_SHAREABLE|action_type|false|false",
        // `#228`: a removed frozen WAL is a replay only on positive evidence.
        "WAL_VERDICT|commit_seen|true|false|false|committed_lost",
        "WAL_VERDICT|commit_seen|false|false|false|unknown_unverifiable",
        "WAL_VERDICT|commit_seen|true|false|true|replayed_inferred",
        "WAL_VERDICT|no_commit|false|false|false|nothing_committed",
        "WAL_VERDICT|unverifiable|false|false|false|unknown_unverifiable",
        "WAL_VERDICT|unreadable|false|false|false|unknown_unreadable",
        "WAL_VERDICT|commit_seen|true|true|false|replayed_observed",
        "DERIVATION_STEP|prp-dom|2|valid",
        "DERIVATION_STEP|prp-dom|1|premise_count_mismatch",
        "DERIVATION_STEP|cax-sco|2|valid",
        "DERIVATION_STEP|cax-dw|3|valid",
        "DERIVATION_STEP|cax-dw|2|premise_count_mismatch",
        "DERIVATION_STEP|owl-universe|0|valid",
        "CERT_FULL|1|true|70x[8:2:true]|true|valid",
        "CERT_FULL|1|true|[255:2:true]|true|unknown_rule",
        "CERT_HASH|42|42|valid",
        "CERT_HASH|42|99|mismatch",
    ] {
        assert!(
            rust_lines.iter().any(|l| l == expected),
            "missing expected parity row: {expected}"
        );
    }
}

/// True when `token` appears as a standalone identifier token (not as a
/// substring of a longer identifier, and not inside a Lean comment).
fn contains_standalone_token(src: &str, token: &str) -> bool {
    for raw_line in src.lines() {
        // Strip `--` line comments; Lean block comments are stripped below.
        let line = match raw_line.find("--") {
            Some(pos) => &raw_line[..pos],
            None => raw_line,
        };
        for word in line.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '\'')) {
            if word == token {
                return true;
            }
        }
    }
    false
}

/// Strip Lean block comments `/- ... -/` (non-nested scan is sufficient here;
/// the proofs contain no nested comment carrying a bare `sorry`/`axiom` token).
fn strip_block_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut depth: usize = 0;
    let bytes: Vec<char> = src.chars().collect();
    let mut i = 0usize;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == '/' && bytes[i + 1] == '-' {
            depth += 1;
            i += 2;
            continue;
        }
        if depth > 0 && i + 1 < bytes.len() && bytes[i] == '-' && bytes[i + 1] == '/' {
            depth = depth.saturating_sub(1);
            i += 2;
            continue;
        }
        if depth == 0 {
            out.push(bytes[i]);
        } else if bytes[i] == '\n' {
            out.push('\n');
        }
        i += 1;
    }
    out
}

/// Collects every `.lean` file under `proofs/` (Main.lean, NeuroStrata.lean and
/// proofs/NeuroStrata/*.lean) so the no-placeholder contract covers all sources.
fn collect_lean_sources(dir: &Path, acc: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_lean_sources(&path, acc);
        } else if path.extension().map(|e| e == "lean").unwrap_or(false) {
            acc.push(path);
        }
    }
}

#[test]
fn lean_proof_sources_contain_no_sorry_or_axiom() {
    let proofs = repo_root().join("proofs");
    assert!(
        proofs.is_dir(),
        "missing proofs dir at {}",
        proofs.display()
    );

    let mut sources: Vec<PathBuf> = Vec::new();
    for mandatory in ["Main.lean", "NeuroStrata.lean"] {
        let p = proofs.join(mandatory);
        assert!(
            p.is_file(),
            "missing mandatory proof source: {}",
            p.display()
        );
        sources.push(p);
    }
    collect_lean_sources(&proofs.join("NeuroStrata"), &mut sources);
    assert!(
        sources.len() >= 3,
        "expected proofs/Main.lean, proofs/NeuroStrata.lean and proofs/NeuroStrata/*.lean, found {}",
        sources.len()
    );

    let mut offenders: Vec<String> = Vec::new();
    for path in &sources {
        let src = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        let stripped = strip_block_comments(&src);
        for token in ["sorry", "axiom"] {
            if contains_standalone_token(&stripped, token) {
                offenders.push(format!("{} contains `{token}`", path.display()));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "placeholder declarations found:\n{}",
        offenders.join("\n")
    );
}

/// NOT a Lean parity test (renamed in PR #206 remediation, V-9): this
/// asserts the Rust DataConnector gate verdicts against hard-coded
/// expectations. It performs no Lean comparison — `proofs/Main.lean` emits
/// no DATA_CONNECTOR rows and no lake/Lean invocation happens here. A real
/// Rust↔Lean differential for the connector gates remains future work.
#[test]
fn data_connector_gate_rust_verdicts() {
    // Test Pre-Gate Verdicts
    assert_eq!(
        evaluate_connector_pre_gate(KernelConnectorPreGateInput {
            connector_registered: true,
            rebac_allowed: true,
            op: KernelConnectorOp::RegisterPointer,
            pool_writable: false,
            receipt_state: KernelActionReceiptState::NotRequired,
            requested_rows: 0,
            max_slice_rows: 100,
            requested_bytes: 0,
            max_slice_bytes: 1000,
            clock_skew_future: false,
            age_secs: 0,
            max_staleness_secs: 0,
        }),
        KernelConnectorPreVerdict::AllowPreRegister
    );

    assert_eq!(
        evaluate_connector_pre_gate(KernelConnectorPreGateInput {
            connector_registered: false,
            rebac_allowed: true,
            op: KernelConnectorOp::ResolveSlice,
            pool_writable: true,
            receipt_state: KernelActionReceiptState::NotRequired,
            requested_rows: 0,
            max_slice_rows: 100,
            requested_bytes: 0,
            max_slice_bytes: 1000,
            clock_skew_future: false,
            age_secs: 0,
            max_staleness_secs: 0,
        }),
        KernelConnectorPreVerdict::DenyUnregisteredConnector
    );

    assert_eq!(
        evaluate_connector_pre_gate(KernelConnectorPreGateInput {
            connector_registered: true,
            rebac_allowed: false,
            op: KernelConnectorOp::ResolveSlice,
            pool_writable: true,
            receipt_state: KernelActionReceiptState::NotRequired,
            requested_rows: 0,
            max_slice_rows: 100,
            requested_bytes: 0,
            max_slice_bytes: 1000,
            clock_skew_future: false,
            age_secs: 0,
            max_staleness_secs: 0,
        }),
        KernelConnectorPreVerdict::DenyRebacAccess
    );

    assert_eq!(
        evaluate_connector_pre_gate(KernelConnectorPreGateInput {
            connector_registered: true,
            rebac_allowed: true,
            op: KernelConnectorOp::ApplyWriteBack,
            pool_writable: false,
            receipt_state: KernelActionReceiptState::NotRequired,
            requested_rows: 0,
            max_slice_rows: 100,
            requested_bytes: 0,
            max_slice_bytes: 1000,
            clock_skew_future: false,
            age_secs: 0,
            max_staleness_secs: 0,
        }),
        KernelConnectorPreVerdict::DenyReadOnlyPool
    );

    assert_eq!(
        evaluate_connector_pre_gate(KernelConnectorPreGateInput {
            connector_registered: true,
            rebac_allowed: true,
            op: KernelConnectorOp::ApplyWriteBack,
            pool_writable: true,
            receipt_state: KernelActionReceiptState::ActiveBound,
            requested_rows: 0,
            max_slice_rows: 100,
            requested_bytes: 0,
            max_slice_bytes: 1000,
            clock_skew_future: false,
            age_secs: 0,
            max_staleness_secs: 0,
        }),
        KernelConnectorPreVerdict::AllowPreWriteBack
    );

    assert_eq!(
        evaluate_connector_pre_gate(KernelConnectorPreGateInput {
            connector_registered: true,
            rebac_allowed: true,
            op: KernelConnectorOp::ResolveSlice,
            pool_writable: true,
            receipt_state: KernelActionReceiptState::NotRequired,
            requested_rows: 101, // exceeds max_slice_rows
            max_slice_rows: 100,
            requested_bytes: 0,
            max_slice_bytes: 1000,
            clock_skew_future: false,
            age_secs: 0,
            max_staleness_secs: 0,
        }),
        KernelConnectorPreVerdict::DenySliceLimitExceeded
    );

    assert_eq!(
        evaluate_connector_pre_gate(KernelConnectorPreGateInput {
            connector_registered: true,
            rebac_allowed: true,
            op: KernelConnectorOp::ResolveSlice,
            pool_writable: true,
            receipt_state: KernelActionReceiptState::NotRequired,
            requested_rows: 1,
            max_slice_rows: 100,
            requested_bytes: 1,
            max_slice_bytes: 1000,
            clock_skew_future: true, // future clock skew
            age_secs: 0,
            max_staleness_secs: 0,
        }),
        KernelConnectorPreVerdict::DenyClockSkewFuture
    );

    assert_eq!(
        evaluate_connector_pre_gate(KernelConnectorPreGateInput {
            connector_registered: true,
            rebac_allowed: true,
            op: KernelConnectorOp::ResolveSlice,
            pool_writable: true,
            receipt_state: KernelActionReceiptState::NotRequired,
            requested_rows: 1,
            max_slice_rows: 100,
            requested_bytes: 1,
            max_slice_bytes: 1000,
            clock_skew_future: false,
            age_secs: 1000,
            max_staleness_secs: 0,
        }),
        KernelConnectorPreVerdict::AllowPreResolve
    ); // not stale yet

    assert_eq!(
        evaluate_connector_pre_gate(KernelConnectorPreGateInput {
            connector_registered: true,
            rebac_allowed: true,
            op: KernelConnectorOp::ResolveSlice,
            pool_writable: true,
            receipt_state: KernelActionReceiptState::NotRequired,
            requested_rows: 1,
            max_slice_rows: 100,
            requested_bytes: 1,
            max_slice_bytes: 1000,
            clock_skew_future: false,
            age_secs: 1000,
            max_staleness_secs: 1, // stale
        }),
        KernelConnectorPreVerdict::DenyPointerStale
    );

    // Test Post-Gate Verdicts
    assert_eq!(
        evaluate_connector_post_gate(KernelConnectorPostGateInput {
            op: KernelConnectorOp::ResolveSlice,
            observed_rows: 0,
            max_slice_rows: 100,
            observed_bytes: 0,
            max_slice_bytes: 1000,
            source_digest_matches: true,
            cas_expected_matches: true,
        }),
        KernelConnectorPostVerdict::AllowPostOutput
    );

    assert_eq!(
        evaluate_connector_post_gate(KernelConnectorPostGateInput {
            op: KernelConnectorOp::ResolveSlice,
            observed_rows: 101, // exceeds max_slice_rows
            max_slice_rows: 100,
            observed_bytes: 0,
            max_slice_bytes: 1000,
            source_digest_matches: true,
            cas_expected_matches: true,
        }),
        KernelConnectorPostVerdict::DenyObservedBoundsExceeded
    );

    assert_eq!(
        evaluate_connector_post_gate(KernelConnectorPostGateInput {
            op: KernelConnectorOp::ResolveSlice,
            observed_rows: 0,
            max_slice_rows: 100,
            observed_bytes: 0,
            max_slice_bytes: 1000,
            source_digest_matches: false, // digest mismatch
            cas_expected_matches: true,
        }),
        KernelConnectorPostVerdict::DenySourceDigestMismatch
    );

    assert_eq!(
        evaluate_connector_post_gate(KernelConnectorPostGateInput {
            op: KernelConnectorOp::ApplyWriteBack,
            observed_rows: 0,
            max_slice_rows: 100,
            observed_bytes: 0,
            max_slice_bytes: 1000,
            source_digest_matches: true,
            cas_expected_matches: false, // CAS conflict
        }),
        KernelConnectorPostVerdict::DenyCasConflict
    );

    assert_eq!(
        evaluate_connector_post_gate(KernelConnectorPostGateInput {
            op: KernelConnectorOp::ApplyWriteBack,
            observed_rows: 0,
            max_slice_rows: 100,
            observed_bytes: 0,
            max_slice_bytes: 1000,
            source_digest_matches: true,
            cas_expected_matches: true,
        }),
        KernelConnectorPostVerdict::AllowPostOutput
    );

    // Pre-Gate: DenyInvalidActionReceipt for every non-ActiveBound receipt state
    for state in [
        KernelActionReceiptState::NotRequired,
        KernelActionReceiptState::MissingOrNotFound,
        KernelActionReceiptState::RevertedOrSuperseded,
        KernelActionReceiptState::TargetMismatch,
    ] {
        assert_eq!(
            evaluate_connector_pre_gate(KernelConnectorPreGateInput {
                connector_registered: true,
                rebac_allowed: true,
                op: KernelConnectorOp::ApplyWriteBack,
                pool_writable: true,
                receipt_state: state,
                requested_rows: 0,
                max_slice_rows: 100,
                requested_bytes: 0,
                max_slice_bytes: 1000,
                clock_skew_future: false,
                age_secs: 0,
                max_staleness_secs: 0,
            }),
            KernelConnectorPreVerdict::DenyInvalidActionReceipt
        );
    }

    // Pre-Gate: requested_rows boundary sweep {0, 1, max, max+1}
    for (rows, expected) in [
        (0u32, KernelConnectorPreVerdict::DenySliceLimitExceeded),
        (1, KernelConnectorPreVerdict::AllowPreResolve),
        (100, KernelConnectorPreVerdict::AllowPreResolve),
        (101, KernelConnectorPreVerdict::DenySliceLimitExceeded),
    ] {
        assert_eq!(
            evaluate_connector_pre_gate(KernelConnectorPreGateInput {
                connector_registered: true,
                rebac_allowed: true,
                op: KernelConnectorOp::ResolveSlice,
                pool_writable: true,
                receipt_state: KernelActionReceiptState::NotRequired,
                requested_rows: rows,
                max_slice_rows: 100,
                requested_bytes: 1,
                max_slice_bytes: 1000,
                clock_skew_future: false,
                age_secs: 0,
                max_staleness_secs: 0,
            }),
            expected
        );
    }

    // Pre-Gate: requested_bytes boundary sweep {0, 1, max, max+1}
    for (bytes, expected) in [
        (0u32, KernelConnectorPreVerdict::DenySliceLimitExceeded),
        (1, KernelConnectorPreVerdict::AllowPreResolve),
        (1000, KernelConnectorPreVerdict::AllowPreResolve),
        (1001, KernelConnectorPreVerdict::DenySliceLimitExceeded),
    ] {
        assert_eq!(
            evaluate_connector_pre_gate(KernelConnectorPreGateInput {
                connector_registered: true,
                rebac_allowed: true,
                op: KernelConnectorOp::ResolveSlice,
                pool_writable: true,
                receipt_state: KernelActionReceiptState::NotRequired,
                requested_rows: 1,
                max_slice_rows: 100,
                requested_bytes: bytes,
                max_slice_bytes: 1000,
                clock_skew_future: false,
                age_secs: 0,
                max_staleness_secs: 0,
            }),
            expected
        );
    }

    // Pre-Gate: staleness boundary (age == max allows, age == max + 1 denies)
    assert_eq!(
        evaluate_connector_pre_gate(KernelConnectorPreGateInput {
            connector_registered: true,
            rebac_allowed: true,
            op: KernelConnectorOp::ResolveSlice,
            pool_writable: true,
            receipt_state: KernelActionReceiptState::NotRequired,
            requested_rows: 1,
            max_slice_rows: 100,
            requested_bytes: 1,
            max_slice_bytes: 1000,
            clock_skew_future: false,
            age_secs: 1,
            max_staleness_secs: 1,
        }),
        KernelConnectorPreVerdict::AllowPreResolve
    );

    assert_eq!(
        evaluate_connector_pre_gate(KernelConnectorPreGateInput {
            connector_registered: true,
            rebac_allowed: true,
            op: KernelConnectorOp::ResolveSlice,
            pool_writable: true,
            receipt_state: KernelActionReceiptState::NotRequired,
            requested_rows: 1,
            max_slice_rows: 100,
            requested_bytes: 1,
            max_slice_bytes: 1000,
            clock_skew_future: false,
            age_secs: 2,
            max_staleness_secs: 1,
        }),
        KernelConnectorPreVerdict::DenyPointerStale
    );

    // Post-Gate: observed_bytes boundary (== max allows, == max + 1 denies)
    assert_eq!(
        evaluate_connector_post_gate(KernelConnectorPostGateInput {
            op: KernelConnectorOp::ResolveSlice,
            observed_rows: 0,
            max_slice_rows: 100,
            observed_bytes: 1000,
            max_slice_bytes: 1000,
            source_digest_matches: true,
            cas_expected_matches: true,
        }),
        KernelConnectorPostVerdict::AllowPostOutput
    );

    assert_eq!(
        evaluate_connector_post_gate(KernelConnectorPostGateInput {
            op: KernelConnectorOp::ResolveSlice,
            observed_rows: 0,
            max_slice_rows: 100,
            observed_bytes: 1001,
            max_slice_bytes: 1000,
            source_digest_matches: true,
            cas_expected_matches: true,
        }),
        KernelConnectorPostVerdict::DenyObservedBoundsExceeded
    );
}

#[test]
fn exhaustive_three_node_dag_and_monotonicity_properties() {
    // 1. Exhaustive 2^9 = 512 3-node directed graphs (including self-loops)
    let all_9_edges: [(u32, u32); 9] = [
        (0, 0),
        (0, 1),
        (0, 2),
        (1, 0),
        (1, 1),
        (1, 2),
        (2, 0),
        (2, 1),
        (2, 2),
    ];
    let mut valid_dag_count = 0usize;
    for mask in 0u32..512u32 {
        let mut edges = Vec::new();
        for (bit, &edge) in all_9_edges.iter().enumerate() {
            if (mask >> bit) & 1 == 1 {
                edges.push(edge);
            }
        }
        let is_dag = formal_kernel::validate_taxonomy_dag(&edges);
        if is_dag {
            valid_dag_count += 1;
        } else {
            // Cyclic broader hierarchy must always be rejected by validate_skos_disjointness
            assert!(
                !formal_kernel::validate_skos_disjointness(&edges, &[], &[]),
                "cyclic broader mask {mask} must be rejected by validate_skos_disjointness"
            );
        }
    }
    // Exactly 25 labeled DAGs exist on 3 vertices
    assert_eq!(
        valid_dag_count, 25,
        "3 vertices have exactly 25 labeled DAGs"
    );

    // 2. Pairwise boolean monotonicity for classify_epistemic_status (256 pairs)
    for m1 in 0u8..16 {
        let (d1, t1, s1, g1) = (m1 & 1 != 0, m1 & 2 != 0, m1 & 4 != 0, m1 & 8 != 0);
        for m2 in 0u8..16 {
            let (d2, t2, s2, g2) = (m2 & 1 != 0, m2 & 2 != 0, m2 & 4 != 0, m2 & 8 != 0);
            if (!d1 || d2) && (!t1 || t2) && (!s1 || s2) && (!g1 || g2) {
                let st1 = formal_kernel::classify_epistemic_status(d1, t1, s1, g1);
                let st2 = formal_kernel::classify_epistemic_status(d2, t2, s2, g2);
                assert!(st1 <= st2);
                assert!(st1.weight_bps() <= st2.weight_bps());
            }
        }
    }

    // 3. Pairwise 5-argument monotonicity and gating for compute_frontier_leverage_bps
    let axes = [0u32, 2500, 7000, 10000, 15000];
    let gaps = [0u32, 2000, 2500, 6000, 7500, 10000, 12000];
    let conns = [0u32, 1, 5, 10, 15];
    for &a1 in &axes {
        for &a2 in &axes {
            if a1 > a2 {
                continue;
            }
            for &g1 in &gaps {
                for &g2 in &gaps {
                    if g1 > g2 {
                        continue;
                    }
                    for &c1 in &conns {
                        for &c2 in &conns {
                            if c1 > c2 {
                                continue;
                            }
                            for &f1 in &[false, true] {
                                for &f2 in &[false, true] {
                                    if f1 && !f2 {
                                        continue;
                                    }
                                    for &r1 in &[false, true] {
                                        for &r2 in &[false, true] {
                                            if r1 && !r2 {
                                                continue;
                                            }
                                            let v1 = formal_kernel::compute_frontier_leverage_bps(
                                                a1, g1, c1, f1, r1,
                                            );
                                            let v2 = formal_kernel::compute_frontier_leverage_bps(
                                                a2, g2, c2, f2, r2,
                                            );
                                            assert!(
                                                v1 <= v2,
                                                "monotonicity violated: ({a1},{g1},{c1},{f1},{r1})={v1} > ({a2},{g2},{c2},{f2},{r2})={v2}"
                                            );
                                            assert!(v2 <= 10000);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
