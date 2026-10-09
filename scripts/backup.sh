#!/usr/bin/env bash
# neurostrata-backup.sh — reliable DB backup using CHECKPOINT+cp pattern.
#
# The CLI `backup` is broken (lbug 0.20.4 EXPORT DATABASE SIGSEGV — see
# memory 74524cc4 / 93357321). This script bypasses the engine entirely:
#   1. Graceful shutdown if daemon running (checkpoints + releases file)
#   2. lsof-verify no holders
#   3. Raw `cp` the db file
#   4. md5-verify the copy
#   5. Prune old backups (keep N most recent)
#   6. Restart daemon if it was running
#
# Source DB is never touched. Failure modes are: bad backup (md5 mismatch)
# → script deletes the bad copy and exits non-zero. The source is safe.
#
# Env overrides:
#   NEUROSTRATA_CONFIG        — config.json path (default: ~/.config/neurostrata/config.json)
#   NEUROSTRATA_BACKUP_DIR    — where to write backups (default: ~/.local/share/neurostrata/backups)
#   NEUROSTRATA_MAX_BACKUPS   — keep N most recent (default: 7)
#   NEUROSTRATA_DAEMON_PORT   — daemon HTTP port (default: 34343)
#   NEUROSTRATA_DAEMON_CWD    — dir to start daemon from (default: dirname of config/..)

set -euo pipefail

CONFIG="${NEUROSTRATA_CONFIG:-$HOME/.config/neurostrata/config.json}"
BACKUP_DIR="${NEUROSTRATA_BACKUP_DIR:-$HOME/.local/share/neurostrata/backups}"
MAX_BACKUPS="${NEUROSTRATA_MAX_BACKUPS:-7}"
DAEMON_PORT="${NEUROSTRATA_DAEMON_PORT:-34343}"

log() { echo "[$(date +%H:%M:%S)] $*"; }
die() { echo "ERROR: $*" >&2; exit "${2:-1}"; }

# 1. Read db_path from config
[ -f "$CONFIG" ] || die "config not found: $CONFIG" 2
command -v jq >/dev/null || die "jq required (apt install jq)" 3
DB_PATH=$(jq -r '.db_path' "$CONFIG") || die "cannot parse $CONFIG" 2
[ -f "$DB_PATH" ] || die "db file not found: $DB_PATH" 2

log "db_path: $DB_PATH"

wait_for_file_free() {
    local path="$1" timeout="${2:-30}"
    if ! command -v lsof >/dev/null; then
        # No lsof — fall back to process check, excluding self
        for _ in $(seq 1 "$timeout"); do
            if ! pgrep -f "neurostrata-mcp daemon" | grep -vw "$$" >/dev/null 2>&1; then
                return 0
            fi
            sleep 1
        done
        return 1
    fi
    for _ in $(seq 1 "$timeout"); do
        if [ -z "$(lsof -t "$path" 2>/dev/null || true)" ]; then
            return 0
        fi
        sleep 1
    done
    return 1
}

# 2. Check if daemon is running (HTTP health probe)
DAEMON_WAS_RUNNING=false
if curl -s -f "http://127.0.0.1:$DAEMON_PORT/health" >/dev/null 2>&1; then
    DAEMON_WAS_RUNNING=true
    log "daemon running — graceful shutdown (checkpoints + releases file)"
    # The shutdown CLI's exit code reflects whether it SAW the checkpoint
    # reported, not whether the checkpoint happened. Trust actual state: if
    # the file is free, the shutdown worked.
    neurostrata-mcp shutdown 2>&1 || true
    if ! wait_for_file_free "$DB_PATH" 30; then
        HOLDERS=$(lsof -t "$DB_PATH" 2>/dev/null || pgrep -f "neurostrata-mcp daemon" | grep -vw "$$")
        die "db file still held after 30s: $HOLDERS" 5
    fi
    log "daemon stopped, file released"
else
    log "daemon not running — proceeding with direct copy"
fi

# 3. lsof-verify no holders
if command -v lsof >/dev/null; then
    HOLDERS=$(lsof -t "$DB_PATH" 2>/dev/null || true)
    if [ -n "$HOLDERS" ]; then
        die "$DB_PATH still held by PID(s): $HOLDERS — investigate before retrying" 6
    fi
    log "lsof: no holders"
fi

# 4. Create backup dir, cp, md5-verify
mkdir -p "$BACKUP_DIR"
TIMESTAMP=$(date +%Y%m%d-%H%M%S)
BACKUP_FILE="$BACKUP_DIR/db-$TIMESTAMP"

cp -p "$DB_PATH" "$BACKUP_FILE" || die "cp failed" 7
SRC_MD5=$(md5sum "$DB_PATH" | awk '{print $1}')
DST_MD5=$(md5sum "$BACKUP_FILE" | awk '{print $1}')

if [ "$SRC_MD5" != "$DST_MD5" ]; then
    rm -f "$BACKUP_FILE"
    die "md5 mismatch: src=$SRC_MD5 dst=$DST_MD5 — bad backup deleted, source untouched" 8
fi

log "backup OK: $BACKUP_FILE (md5=$SRC_MD5)"

# 5. Prune old backups
if [ "$MAX_BACKUPS" -gt 0 ]; then
    PRUNE_COUNT=$(ls -1t "$BACKUP_DIR"/db-* 2>/dev/null | wc -l)
    if [ "$PRUNE_COUNT" -gt "$MAX_BACKUPS" ]; then
        ls -1t "$BACKUP_DIR"/db-* 2>/dev/null | tail -n +$((MAX_BACKUPS + 1)) | xargs -r rm -f
        log "pruned $((PRUNE_COUNT - MAX_BACKUPS)) old backup(s), keeping $MAX_BACKUPS"
    fi
fi

# 6. Restart daemon if it was running
if $DAEMON_WAS_RUNNING; then
    log "restarting daemon"
    nohup neurostrata-mcp daemon >/dev/null 2>&1 &
    # Wait for health
    for _ in $(seq 1 15); do
        if curl -s -f "http://127.0.0.1:$DAEMON_PORT/health" >/dev/null 2>&1; then
            log "daemon healthy"
            break
        fi
        sleep 1
    done
fi

log "done"
