#!/usr/bin/env bash
# disk-hygiene.sh — automated prevention against recurring NeuroStrata disk bloat.
#
# Cleans the 4 recurring bloat sources without touching active work or DB data:
#   1. Leaked /tmp and /var/tmp/ns-tmp test & rustc dirs older than 2 hours
#   2. Stale Cargo target/ directories in inactive OpenCode/git worktrees (>24h idle)
#   3. Old superseded test binaries in target/debug/deps/ (>48h old)
#   4. Exited Podman containers and dangling (<none>) image layers
#
# Invoked every 6 hours by ~/.config/systemd/user/neurostrata-disk-hygiene.timer.

set -euo pipefail

REPO_ROOT="${NEUROSTRATA_REPO_ROOT:-$HOME/Documents/NeuroStrata}"
OPENCODE_WORKTREES="${OPENCODE_WORKTREES:-$HOME/.local/share/opencode/worktree}"
TMP_MIN_AGE_MINUTES="${NEUROSTRATA_TMP_MIN_AGE_MINUTES:-120}"
WORKTREE_TARGET_IDLE_DAYS="${NEUROSTRATA_WORKTREE_TARGET_IDLE_DAYS:-1}"
OLD_TEST_BIN_DAYS="${NEUROSTRATA_OLD_TEST_BIN_DAYS:-2}"

log() { echo "[$(date +%Y-%m-%dT%H:%M:%S)] [disk-hygiene] $*"; }

# 1. Clean leaked test & compiler temp directories older than 2 hours in /tmp and /var/tmp/ns-tmp
for TMP_BASE in /tmp /var/tmp/ns-tmp; do
    [ -d "$TMP_BASE" ] || continue
    find "$TMP_BASE" -maxdepth 1 -mindepth 1 -mmin +"$TMP_MIN_AGE_MINUTES" \( \
        -name 'ns-*' -o \
        -name 'ns_*' -o \
        -name 'neurostrata*' -o \
        -name 'lbug_*' -o \
        -name 'procmem-*' -o \
        -name 'mcp-restore-*' -o \
        -name 'receipt-test-*' -o \
        -name 'rustc*' \
    \) -exec rm -rf {} + 2>/dev/null || true
done
log "cleaned stale temp directories in /tmp and /var/tmp/ns-tmp"

# 2. Skip Cargo target cleanup if a cargo/rustc build is currently active
if pgrep -x cargo >/dev/null 2>&1 || pgrep -x rustc >/dev/null 2>&1; then
    log "cargo/rustc active — skipping target/ directory pruning this cycle"
else
    # 2a. Remove target/ directories inside inactive OpenCode worktrees (>24h since last modification)
    if [ -d "$OPENCODE_WORKTREES" ]; then
        for wt_target in "$OPENCODE_WORKTREES"/*/*/target "$OPENCODE_WORKTREES"/*/*/.worktrees/*/target; do
            [ -d "$wt_target" ] || continue
            # If nothing top-level in target/ was modified in WORKTREE_TARGET_IDLE_DAYS, remove it
            recent=$(find "$wt_target" -maxdepth 2 -mtime -"$WORKTREE_TARGET_IDLE_DAYS" -print -quit 2>/dev/null || true)
            if [ -z "$recent" ]; then
                log "removing idle worktree target: $wt_target"
                rm -rf "$wt_target"
            fi
        done
    fi

    # 2b. Remove target/ directories inside repo-local .worktrees/ (>24h idle)
    if [ -d "$REPO_ROOT/.worktrees" ]; then
        for wt_target in "$REPO_ROOT/.worktrees"/*/target; do
            [ -d "$wt_target" ] || continue
            recent=$(find "$wt_target" -maxdepth 2 -mtime -"$WORKTREE_TARGET_IDLE_DAYS" -print -quit 2>/dev/null || true)
            if [ -z "$recent" ]; then
                log "removing idle local worktree target: $wt_target"
                rm -rf "$wt_target"
            fi
        done
    fi

    # 2c. Prune old superseded test binaries and incremental cache (>48h old) in main repo target/
    if [ -d "$REPO_ROOT/target/debug/deps" ]; then
        find "$REPO_ROOT/target/debug/deps" -maxdepth 1 -type f -executable -mtime +"$OLD_TEST_BIN_DAYS" -delete 2>/dev/null || true
    fi
    if [ -d "$REPO_ROOT/target/debug/incremental" ]; then
        find "$REPO_ROOT/target/debug/incremental" -maxdepth 1 -mindepth 1 -type d -mtime +"$OLD_TEST_BIN_DAYS" -exec rm -rf {} + 2>/dev/null || true
    fi
fi

# 3. Prune exited Podman containers and dangling (<none>) image layers
if command -v podman >/dev/null 2>&1; then
    podman container prune -f >/dev/null 2>&1 || true
    podman image prune -f >/dev/null 2>&1 || true
    log "pruned exited Podman containers and dangling image layers"
fi

log "done (root free: $(df -h / | awk 'NR==2 {print $4 " / " $5 " used"}'), /tmp: $(df -h /tmp | awk 'NR==2 {print $4 " free / " $5 " used"}'))"
