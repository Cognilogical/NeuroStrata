//! Rust -> Lean parity and Aeneas extractability contract for the formal kernel.
//!
//! This target compiles `src/formal_kernel.rs` from its real source path because
//! the crate ships a binary target only (no `[lib]`), mirroring the other
//! crate-free contract tests under `tests/`.
//!
//! Three contracts:
//! 1. the kernel compiles standalone with zero `--extern` flags (Aeneas/Charon
//!    extractability precondition);
//! 2. the exhaustive 389-row truth table produced by the Rust kernel equals the
//!    one emitted by `lake --dir proofs exe neurostrata-proofs`;
//! 3. no Lean proof source carries a `sorry` placeholder or an `axiom`.

#![allow(dead_code)]

#[path = "../src/formal_kernel.rs"]
mod formal_kernel;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use formal_kernel::{
    classify_merge_decision_tag, claim_decision_tag, evaluate_claim_decision,
    evaluate_import_precheck, evaluate_procedure_fire, evaluate_retention_gate,
    evaluate_seq_continuity, evaluate_sovereignty_gate, epoch_gate_tag,
    import_precheck_tag, join_task_status, lifecycle_can_transition, procedure_fire_tag,
    retention_verdict_tag, seq_decision_tag, sovereignty_decision_tag, task_step_tag,
    KernelProcedureTrust,
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

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at the crate root that owns this test target.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Generates the canonical 389-line truth table in `proofs/Main.lean` order:
/// 64 `TASK_FSM` + 16 `LIFECYCLE` + 128 `MESH_MERGE` + 3 `EPOCH_GATE` + 24 `TASK_CLAIM` + 32 `SOVEREIGNTY_GATE` + 8 `IMPORT_PRECHECK` + 60 `PROCEDURE_FIRE` + 16 `TWIN_CRDT_STATUS` + 6 `TWIN_CRDT_SEQ` + 32 `RETENTION_GATE`.
fn generate_rust_truth_table() -> Vec<String> {
    let mut lines: Vec<String> = Vec::with_capacity(389);

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
                        let tag = classify_merge_decision_tag(formal_kernel::classify_merge_decision(
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
    let age_cases: [(&str, Option<i64>); 3] = [("none", None), ("live", Some(1800)), ("stale", Some(3601))];
    for &status in TASK_STATUSES.iter() {
        for &same_session in [false, true].iter() {
            for &(age_label, age_secs) in age_cases.iter() {
                let tag = claim_decision_tag(evaluate_claim_decision(status, same_session, age_secs, 3600));
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
                        let tag = sovereignty_decision_tag(evaluate_sovereignty_gate(perm, shareable, counter, acl_empty, in_acl));
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
                let tag = import_precheck_tag(evaluate_import_precheck(row_exists, has_prov, same_upd));
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
    let budget_cases: [(&str, Option<u64>); 3] = [("unbounded", None), ("zero", Some(0)), ("two", Some(2))];
    for &trust in trusts.iter() {
        for &imported in [false, true].iter() {
            for &(lapsed_label, valid_to) in lapsed_cases.iter() {
                for &(budget_label, remaining) in budget_cases.iter() {
                    let tag = procedure_fire_tag(evaluate_procedure_fire(
                        trust,
                        imported,
                        valid_to,
                        1000,
                        remaining,
                        0,
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
                        let tag = retention_verdict_tag(evaluate_retention_gate(budget, non_empty, found, bound, matches_wt));
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
#[test]
fn formal_kernel_compiles_standalone_for_aeneas_extraction() {
    let root = repo_root();
    let script = root.join("proofs").join("scripts").join("extract-aeneas.sh");
    assert!(
        script.is_file(),
        "missing extract script at {}",
        script.display()
    );

    let status = Command::new("bash")
        .arg(&script)
        .current_dir(&root)
        .status()
        .expect("failed to spawn proofs/scripts/extract-aeneas.sh");
    assert!(status.success(), "extract-aeneas.sh exited non-zero");
}

/// Live parity: the Rust kernel's exhaustive truth table must equal the Lean
/// generated executable's stdout line-for-line.
#[test]
fn rust_formal_kernel_truth_table_matches_lean_cli() {
    let rust_lines = generate_rust_truth_table();
    assert_eq!(
        rust_lines.len(),
        389,
        "expected 389 canonical lines (64+16+128+3+24+32+8+60+16+6+32)"
    );

    // Section-count verification: 64 TASK_FSM + 16 LIFECYCLE + 128 MESH_MERGE
    // + 3 EPOCH_GATE + 24 TASK_CLAIM + 32 SOVEREIGNTY_GATE + 8 IMPORT_PRECHECK
    // + 60 PROCEDURE_FIRE + 16 TWIN_CRDT_STATUS + 6 TWIN_CRDT_SEQ + 32 RETENTION_GATE,
    // matching the exact generation order in `proofs/Main.lean`.
    let section_counts = rust_lines
        .iter()
        .fold(HashMap::new(), |mut acc, line| {
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

    let lake = find_lake().expect("lake not found on PATH or ~/.elan/bin/lake; install elan to run Lean parity tests");
    let root = repo_root();
    let build_status = Command::new(&lake)
        .arg("--dir")
        .arg("proofs")
        .arg("build")
        .current_dir(&root)
        .status()
        .expect("failed to run lake --dir proofs build");
    assert!(build_status.success(), "lake --dir proofs build exited non-zero");

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
    assert!(proofs.is_dir(), "missing proofs dir at {}", proofs.display());

    let mut sources: Vec<PathBuf> = Vec::new();
    for mandatory in ["Main.lean", "NeuroStrata.lean"] {
        let p = proofs.join(mandatory);
        assert!(p.is_file(), "missing mandatory proof source: {}", p.display());
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
