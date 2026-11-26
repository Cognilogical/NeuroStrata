#!/usr/bin/env bash
# Mutation check for the Kani harnesses.
#
# Each mutant below breaks a property the corresponding harness claims to
# prove. The harness must FAIL on the mutant; a SUCCESSFUL verification means
# the harness has decayed into a panic-freedom check that proves nothing about
# the property.
#
# Usage: scripts/kani-mutation-check.sh
#   NEUROSTRATA_REQUIRE_KANI=1  fail when Kani is unavailable (CI)
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
KERNEL="$ROOT/src/formal_kernel.rs"
KANI="${KANI:-}"

if [ -z "$KANI" ]; then
    if [ -x "$HOME/.kani/kani-0.68.0/bin/kani" ]; then
        KANI="$HOME/.kani/kani-0.68.0/bin/kani"
    elif command -v kani >/dev/null 2>&1; then
        KANI="$(command -v kani)"
    fi
fi
if [ -z "$KANI" ]; then
    if [ "${NEUROSTRATA_REQUIRE_KANI:-0}" = "1" ]; then
        echo "error: Kani not found; install it or set KANI=<path>" >&2
        exit 1
    fi
    echo "skip: Kani not found (set NEUROSTRATA_REQUIRE_KANI=1 to require)" >&2
    exit 0
fi
# kani-driver invokes goto-cc by name from its own bin directory.
KANI_DIR="$(dirname "$KANI")"
export PATH="$KANI_DIR:$PATH"

work="$(mktemp -d "${TMPDIR:-/tmp}/kani-mutants.XXXXXX")"
trap 'rm -rf "$work"' EXIT

failures=0

check() {
    local name="$1"
    local harness="$2"
    local expr="$3"
    local copy="$work/$name.rs"

    cp "$KERNEL" "$copy"
    perl -0pi -e "$expr" "$copy"
    if cmp -s "$KERNEL" "$copy"; then
        echo "FAIL: mutation '$name' did not apply (source changed?)" >&2
        failures=$((failures + 1))
        return 0
    fi

    local out
    out="$("$KANI" "$copy" --harness "$harness" 2>&1)" || true
    if printf '%s' "$out" | grep -q "VERIFICATION:- FAILED"; then
        echo "PASS: mutant '$name' rejected by $harness"
    else
        echo "FAIL: mutant '$name' still verified by $harness" >&2
        printf '%s\n' "$out" | tail -8 >&2
        failures=$((failures + 1))
    fi
}

# C: restore the original usize -> u32 truncation (Bug 1).
check truncate proof_build_resolve_post_gate_input_bounds \
    's/let observed_rows = if observed_rows_len > \(u32::MAX as usize\) \{\s*u32::MAX\s*\} else \{\s*observed_rows_len as u32\s*\};/let observed_rows = observed_rows_len as u32;/'

# E: drop the reachability check from DAG validation (cycles accepted).
check dag-cycle proof_validate_taxonomy_dag_bounded \
    's/if u == v \|\| is_reachable_in_edges\(broader_edges, v, u\) \{/if u == v {/'

# A: drop the extraction guard on Open -> Done.
check task-guard proof_task_edge_guard_legal_edges \
    's/\(Open, Done\) => Some\(KernelGuard::ExtractionRequired\)/(Open, Done) => Some(KernelGuard::None)/'

# F: skip an extra rehearsal slot (budget not conserved).
check budget proof_evaluate_procedure_fire_no_panic \
    's/remaining_fires: Some\(b - 1\)/remaining_fires: Some(b.saturating_sub(2))/'

# G: check CAS before the pointer source digest.
check digest-order proof_evaluate_connector_post_gate_total \
    's/\} else if !input\.source_digest_matches \{\s*KernelConnectorPostVerdict::DenySourceDigestMismatch\s*\} else if matches!\(input\.op, KernelConnectorOp::ApplyWriteBack\)\s*&& !input\.cas_expected_matches\s*\{\s*KernelConnectorPostVerdict::DenyCasConflict/} else if matches!(input.op, KernelConnectorOp::ApplyWriteBack) && !input.cas_expected_matches {\n            KernelConnectorPostVerdict::DenyCasConflict\n        } else if !input.source_digest_matches {\n            KernelConnectorPostVerdict::DenySourceDigestMismatch/'

# H: swap the epoch decisions for older and newer candidates.
check epoch-swap proof_epoch_gate_total \
    's/KernelEpochDecision::QuarantineOld\s*\} else \{\s*KernelEpochDecision::RefuseFuture/KernelEpochDecision::RefuseFuture\n    } else {\n        KernelEpochDecision::QuarantineOld/'

# I: break working-set eviction (allow Immortal tier to demote).
check ws-eviction proof_evaluate_working_set_eviction_total \
    's/WorkingSetTier::Immortal => GcAction::RetainFull,/WorkingSetTier::Immortal => GcAction::DemoteToStub,/'

# J: break quantized stride folding (frontier no longer steps on stride
# boundaries, so the KV-cache prefix mutates every turn).
check ws-stride proof_evaluate_working_set_stride_fold_contract \
    's{\(foldable / stride\) \* stride}{foldable}'

if [ "$failures" -ne 0 ]; then
    echo "kani mutation check: $failures failure(s)" >&2
    exit 1
fi
echo "kani mutation check: all mutants rejected"
