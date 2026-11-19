#!/usr/bin/env bash
#
# extract-aeneas.sh — build the Aeneas-compatible pure Rust formal kernel
# (src/formal_kernel.rs) as a standalone rlib, optionally translating via
# charon + aeneas when those tools are on PATH (or --podman is passed).
#
# Designed to be run from the repo root:
#   bash proofs/scripts/extract-aeneas.sh [--podman]
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
KERNEL_SRC="${REPO_ROOT}/src/formal_kernel.rs"
OUT_DIR="${TMPDIR:-/tmp}/neurostrata-aeneas"
mkdir -p "${OUT_DIR}"
RLIB_OUT="${OUT_DIR}/libformal_kernel.rlib"

echo "[extract-aeneas] compiling ${KERNEL_SRC} -> ${RLIB_OUT}"
rustc -D warnings --edition=2021 --crate-type=lib "${KERNEL_SRC}" -o "${RLIB_OUT}"
echo "[extract-aeneas] kernel compiled OK -> ${RLIB_OUT}"

# Symbolic model checking via Kani Rust Verifier (#[cfg(kani)] proof harnesses in src/formal_kernel.rs).
KANI_BIN=""
if command -v kani >/dev/null 2>&1; then
    KANI_BIN="$(command -v kani)"
elif [ -x "${HOME:-}/.cargo/bin/kani" ]; then
    KANI_BIN="${HOME}/.cargo/bin/kani"
fi

if [ "${NEUROSTRATA_REQUIRE_KANI:-0}" = "1" ] && [ "${NEUROSTRATA_SKIP_KANI:-0}" = "1" ]; then
    echo "[extract-aeneas] ERROR: NEUROSTRATA_REQUIRE_KANI=1 conflicts with NEUROSTRATA_SKIP_KANI=1" >&2
    exit 1
fi

if [ "${NEUROSTRATA_SKIP_KANI:-0}" = "1" ]; then
    echo "[extract-aeneas] NEUROSTRATA_SKIP_KANI=1; skipping Kani verification"
elif [ -n "${KANI_BIN}" ]; then
    echo "[extract-aeneas] running Kani symbolic model checker (${KANI_BIN}) on ${KERNEL_SRC}"
    "${KANI_BIN}" "${KERNEL_SRC}"
    echo "[extract-aeneas] Kani verification OK"
elif [ "${NEUROSTRATA_REQUIRE_KANI:-0}" = "1" ]; then
    echo "[extract-aeneas] ERROR: NEUROSTRATA_REQUIRE_KANI=1 but kani was not found on PATH or ~/.cargo/bin/kani" >&2
    exit 1
else
    echo "[extract-aeneas] kani not on PATH; skipping Kani symbolic verification"
fi

# Optional Lean-4 translation: charon (Rust -> Aeneas IR) then aeneas (IR -> Lean).
# Only runs when charon and aeneas are on PATH, or --podman is passed
# (in which case a container image would need to be provided by the operator).
USE_PODMAN=0
if [ "${1:-}" = "--podman" ]; then USE_PODMAN=1; fi

# `--podman` is honored before any PATH check so CI users who pass --podman
# always get the container path even when charon/aeneas happen to be on PATH.
if [ "${USE_PODMAN}" -eq 1 ]; then
    if command -v charon >/dev/null 2>&1 && command -v aeneas >/dev/null 2>&1; then
        echo "[extract-aeneas] translating with charon + aeneas -backend lean"
        charon "${KERNEL_SRC}" --out "${OUT_DIR}/formal_kernel.aeneas"
        aeneas -backend lean "${OUT_DIR}/formal_kernel.aeneas" --out "${OUT_DIR}/formal_kernel.lean"
        echo "[extract-aeneas] Lean 4 translation complete"
    else
        echo "[extract-aeneas] running in podman container..."
        podman run --rm -v "$REPO_ROOT:/work:Z" -w /work ghcr.io/aeneasverif/aeneas:latest sh -c "charon rustc -- --crate-type=lib src/formal_kernel.rs && aeneas -backend lean formal_kernel.llbc -dest proofs/NeuroStrata"
    fi
elif command -v charon >/dev/null 2>&1 && command -v aeneas >/dev/null 2>&1; then
    echo "[extract-aeneas] translating with charon + aeneas -backend lean"
    charon "${KERNEL_SRC}" --out "${OUT_DIR}/formal_kernel.aeneas"
    aeneas -backend lean "${OUT_DIR}/formal_kernel.aeneas" --out "${OUT_DIR}/formal_kernel.lean"
    echo "[extract-aeneas] Lean 4 translation complete"
else
    echo "[extract-aeneas] charon/aeneas not on PATH; skipping Lean 4 translation"
fi

echo "[extract-aeneas] done"

