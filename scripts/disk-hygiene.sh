#!/usr/bin/env bash
# disk-hygiene.sh — automated prevention against recurring NeuroStrata disk bloat.
#
# Reclaims the bloat sources that accumulate around long-running checkouts:
#   1. Leaked test & compiler temp directories (ns-*, rustc*, lbug_*, ...)
#   2. Stale Cargo target/ directories in inactive worktrees
#   3. Stale incremental caches in the main checkout's target directory
#   4. Exited Podman containers and dangling (<none>) image layers
#
# Safety rules this script implements:
#   * The repository root is resolved from the script's own checkout, so the
#     script cleans the tree it ships with and not a hard-coded home path.
#     An unresolvable root is a hard error (exit 2), never a silent no-op.
#   * The active-build check is re-run immediately before EVERY destructive
#     step, so a build that starts mid-run is never raced.
#   * Cargo's own dependency artifacts are never deleted: cargo owns them and
#     deleting them turns the next build into a full rebuild.
#   * --dry-run prints the plan and deletes nothing.
#
# Scheduling is the operator's job: the unit files are committed under
# scripts/systemd/, but nothing installs or enables them automatically. See
# docs/features-api/ops/disk-hygiene.md for how to turn them on.
#
# Environment overrides (all optional):
#   NEUROSTRATA_REPO_ROOT            root to operate on (default: this checkout)
#   NEUROSTRATA_OPENCODE_WORKTREES   OpenCode worktree parent (default: ~/.local/share/opencode/worktree)
#   NEUROSTRATA_DISK_HYGIENE_TMP_BASES  colon-separated temp bases (default: /tmp:/var/tmp/ns-tmp)
#   NEUROSTRATA_DISK_HYGIENE_PGREP   pgrep-compatible lookup (default: pgrep)
#   NEUROSTRATA_DISK_HYGIENE_PODMAN  podman binary; empty string disables pruning
#   NEUROSTRATA_TMP_MIN_AGE_MINUTES  temp dir age threshold (default: 120)
#   NEUROSTRATA_WORKTREE_TARGET_IDLE_DAYS  worktree idle threshold (default: 1)
#   NEUROSTRATA_OLD_TEST_BIN_DAYS    incremental cache age threshold (default: 2)

set -euo pipefail

DRY_RUN=0
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    -h|--help)
      sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "disk-hygiene: unknown argument '$arg'" >&2
      exit 64
      ;;
  esac
done

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)

log() { echo "[$(date +%Y-%m-%dT%H:%M:%S)] [disk-hygiene] $*"; }

# --- root resolution --------------------------------------------------------

resolve_root() {
  # 1. explicit override
  if [ -n "${NEUROSTRATA_REPO_ROOT-}" ]; then
    if [ ! -d "$NEUROSTRATA_REPO_ROOT" ]; then
      echo "disk-hygiene: repo root '$NEUROSTRATA_REPO_ROOT' does not exist" >&2
      return 1
    fi
    (CDPATH='' cd -- "$NEUROSTRATA_REPO_ROOT" && pwd)
    return 0
  fi
  # 2. this script's own checkout (worktree root when it is a linked worktree)
  local top
  if top=$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel 2>/dev/null) && [ -n "$top" ]; then
    echo "$top"
    return 0
  fi
  # 3. the common git directory's parent (plain checkout)
  local common
  if common=$(git -C "$SCRIPT_DIR" rev-parse --git-common-dir 2>/dev/null) && [ -n "$common" ]; then
    (CDPATH='' cd -- "$common/.." && pwd)
    return 0
  fi
  echo "disk-hygiene: cannot resolve a repo root from '$SCRIPT_DIR'" >&2
  return 1
}

REPO_ROOT=$(resolve_root) || exit 2
[ -n "$REPO_ROOT" ] || { echo "disk-hygiene: empty repo root" >&2; exit 2; }

OPENCODE_WORKTREES="${NEUROSTRATA_OPENCODE_WORKTREES:-$HOME/.local/share/opencode/worktree}"
TMP_BASES="${NEUROSTRATA_DISK_HYGIENE_TMP_BASES-/tmp:/var/tmp/ns-tmp}"
PGREP_BIN="${NEUROSTRATA_DISK_HYGIENE_PGREP-pgrep}"
PODMAN_BIN="${NEUROSTRATA_DISK_HYGIENE_PODMAN-podman}"
TMP_MIN_AGE_MINUTES="${NEUROSTRATA_TMP_MIN_AGE_MINUTES:-120}"
WORKTREE_TARGET_IDLE_DAYS="${NEUROSTRATA_WORKTREE_TARGET_IDLE_DAYS:-1}"
OLD_TEST_BIN_DAYS="${NEUROSTRATA_OLD_TEST_BIN_DAYS:-2}"

log "repo root: $REPO_ROOT"
if [ "$DRY_RUN" = "1" ]; then
  log "dry run: nothing will be deleted"
fi

# --- helpers ----------------------------------------------------------------

# Is a cargo/rustc build running right now? Re-checked before every removal.
# Zombie processes are ignored: a reaped-but-not-yet-collected cargo is not a
# build, and treating one as active would disable the script forever.
build_active() {
  local out pid state saw=0
  for proc_name in cargo rustc; do
    out=$("$PGREP_BIN" -x "$proc_name" 2>/dev/null) || out=""
    for pid in $out; do
      saw=1
      state=$(sed 's/^.*) //' "/proc/$pid/stat" 2>/dev/null | awk '{print $1}') || state=""
      if [ -n "$state" ] && [ "$state" != "Z" ]; then
        return 0
      fi
    done
  done
  # No pid information available (a stubbed or non-procfs lookup): fall back
  # to the lookup's own exit status.
  if [ "$saw" -eq 0 ]; then
    "$PGREP_BIN" -x cargo >/dev/null 2>&1 && return 0
    "$PGREP_BIN" -x rustc >/dev/null 2>&1 && return 0
  fi
  return 1
}

# Remove a path, honouring both the dry-run flag and the active-build guard.
remove_path() {
  local path="$1"
  if build_active; then
    log "build active — skipped removal of $path"
    return 0
  fi
  if [ "$DRY_RUN" = "1" ]; then
    log "would remove $path"
    return 0
  fi
  log "removing $path"
  rm -rf -- "$path"
}

# Is a directory idle? True when nothing inside it changed recently.
is_idle() {
  local dir="$1"
  [ -z "$(find "$dir" -maxdepth 2 -mtime -"$WORKTREE_TARGET_IDLE_DAYS" -print -quit 2>/dev/null)" ]
}

# --- 1. leaked temp directories --------------------------------------------

IFS=':' read -r -a TMP_BASE_LIST <<< "$TMP_BASES"
for TMP_BASE in "${TMP_BASE_LIST[@]}"; do
  [ -n "$TMP_BASE" ] || continue
  [ -d "$TMP_BASE" ] || continue
  while IFS= read -r -d '' stale; do
    remove_path "$stale"
  done < <(
    find "$TMP_BASE" -maxdepth 1 -mindepth 1 -mmin +"$TMP_MIN_AGE_MINUTES" \( \
        -name 'ns-*' -o \
        -name 'ns_*' -o \
        -name 'neurostrata*' -o \
        -name 'lbug_*' -o \
        -name 'procmem-*' -o \
        -name 'mcp-restore-*' -o \
        -name 'receipt-test-*' -o \
        -name 'rustc*' \
    \) -print0 2>/dev/null || true
  )
done

# --- 2. stale Cargo targets in inactive worktrees ---------------------------

if [ -d "$OPENCODE_WORKTREES" ]; then
  for wt_target in "$OPENCODE_WORKTREES"/*/*/target "$OPENCODE_WORKTREES"/*/*/.worktrees/*/target; do
    [ -d "$wt_target" ] || continue
    if is_idle "$wt_target"; then
      remove_path "$wt_target"
    fi
  done
fi

# --- 3. stale Cargo targets in repo-local worktrees -------------------------

if [ -d "$REPO_ROOT/.worktrees" ]; then
  for wt_target in "$REPO_ROOT/.worktrees"/*/target; do
    [ -d "$wt_target" ] || continue
    if is_idle "$wt_target"; then
      remove_path "$wt_target"
    fi
  done
fi

# --- 4. stale incremental caches in the main checkout ------------------------
# Cargo's dependency artifacts are deliberately left alone: cargo owns them.

INCREMENTAL_DIR="$REPO_ROOT/target/debug/incremental"
if [ -d "$INCREMENTAL_DIR" ]; then
  while IFS= read -r -d '' cache; do
    remove_path "$cache"
  done < <(
    find "$INCREMENTAL_DIR" -maxdepth 1 -mindepth 1 -type d -mtime +"$OLD_TEST_BIN_DAYS" -print0 2>/dev/null || true
  )
fi

# --- 5. Podman --------------------------------------------------------------

if [ -n "$PODMAN_BIN" ] && command -v "$PODMAN_BIN" >/dev/null 2>&1; then
  if [ "$DRY_RUN" = "1" ]; then
    log "would prune exited Podman containers and dangling image layers"
  else
    "$PODMAN_BIN" container prune -f >/dev/null 2>&1 || true
    "$PODMAN_BIN" image prune -f >/dev/null 2>&1 || true
    log "pruned exited Podman containers and dangling image layers"
  fi
fi

log "done (root free: $(df -h / | awk 'NR==2 {print $4 " / " $5 " used"}'), /tmp: $(df -h /tmp | awk 'NR==2 {print $4 " free / " $5 " used"}'))"
