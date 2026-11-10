#!/usr/bin/env bash
# neurostrata-backup.sh — reliable DB backup using CHECKPOINT+cp pattern.
#
# Written when the engine's EXPORT DATABASE was unusable (lbug 0.20.4
# EXPORT DATABASE SIGSEGV — see memory 74524cc4 / 93357321). Revalidated
# on lbug 0.22.3: EXPORT DATABASE of a fixture store succeeded (Linux
# x86_64); this script's pattern is kept as-is. It bypasses the engine
# entirely:
#   1. systemctl --user stop neurostrata  (graceful shutdown + checkpoint)
#   2. lsof-verify no holders
#   3. Raw `cp` the db file (plus its store-state sidecar, when present,
#      and its WAL when the daemon was not running — step 4c below)
#   4. md5-verify the copies
#   5. Prune old backups (keep N most recent; a pruned backup's sidecar
#      and WAL companions are deleted with it)
#   6. systemctl --user start neurostrata
#
# Source DB is never touched. Failure modes are: bad backup (md5 mismatch)
# → script deletes the bad copy and exits non-zero. The source is safe.
#
# Store-state companion: a db at path P records its embedding identity in
# the sidecar P.store-state.json (model_choice::store_state_path). When
# that sidecar exists, step 4 copies it alongside the backup as
# db-<timestamp>.store-state.json and md5-verifies it the same way; any
# sidecar copy/verify failure deletes both copies and exits non-zero,
# under the same failure contract as the db copy. To recover, place the
# db backup at the target db path and its companion at
# <target db path>.store-state.json — exactly where the store looks for
# it — so the recovered store keeps its recorded identity. A backup
# without a companion (a pre-feature store with no sidecar) restores
# exactly as before: no recorded identity (LockedUnknown), unchanged.
#
# WAL companion: the checkpoint guarantee above comes from the systemd
# stop. On the daemon-inactive path (daemon not running — direct copy)
# there is no checkpoint, so after an unclean shutdown the WAL at
# <db path>.wal may hold the only copy of recent writes. When the
# daemon was NOT running at backup time and that WAL exists, step 4c
# copies it alongside the backup as db-<timestamp>.wal and md5-verifies
# it the same way, under the same failure contract as steps 4/4b. A
# daemon-inactive backup may therefore carry a db-<timestamp>.wal
# companion; to recover, place it at <target db path>.wal alongside
# the db file (and the sidecar companion at its documented place
# above), so the recovered store replays exactly the writes the backup
# captured. When the daemon WAS running, its graceful stop
# checkpointed the WAL into the main db file, so those backups carry
# no WAL companion and restore exactly as before.
#
# Backup-set unit invariant: a backup is a unit — the db file plus
# the companions (sidecar, WAL) this run produces under one timestamp
# name. The timestamp has second resolution, so two runs in the same
# second into the same directory reuse one destination name and the
# step-4 cp overwrites the db in place. A reused destination must
# never mix the current run's db with a previous run's companions:
# steps 4b/4c therefore remove the destination sidecar / WAL companion
# whenever the current run does not produce one, so the surviving set
# is exactly the set the current run captured.
#
# Scope of that invariant: it is enforced by the steps 4b/4c cleanup
# on runs that complete steps 4–4c successfully. It is not a
# construction guarantee — a run that fails or is interrupted
# mid-copy can leave a mixed set (and the in-place step-4 cp can
# already have overwritten a prior good backup), and orphaned
# companions are never pruned. Building the set under temp names
# and renaming it into place is tracked in #93.
#
# Assumes the daemon is managed by systemd (neurostrata-mcp setup installs
# the service). If you ran `neurostrata-mcp daemon &` ad-hoc, the stop/start
# here will fail — run `neurostrata-mcp setup` once to install the service.
#
# Env overrides:
#   NEUROSTRATA_CONFIG        — config.json path (default: ~/.config/neurostrata/config.json)
#   NEUROSTRATA_BACKUP_DIR    — where to write backups (default: ~/.local/share/neurostrata/backups)
#   NEUROSTRATA_MAX_BACKUPS   — keep N most recent (default: 7)
#   NEUROSTRATA_DAEMON_PORT   — daemon HTTP port (default: 34343)
#   NEUROSTRATA_SERVICE       — systemd service name (default: neurostrata.service)

set -euo pipefail

CONFIG="${NEUROSTRATA_CONFIG:-$HOME/.config/neurostrata/config.json}"
BACKUP_DIR="${NEUROSTRATA_BACKUP_DIR:-$HOME/.local/share/neurostrata/backups}"
MAX_BACKUPS="${NEUROSTRATA_MAX_BACKUPS:-7}"
DAEMON_PORT="${NEUROSTRATA_DAEMON_PORT:-34343}"
SERVICE="${NEUROSTRATA_SERVICE:-neurostrata.service}"

log() { echo "[$(date +%H:%M:%S)] $*"; }
die() { local msg="$1" code="${2:-1}"; echo "ERROR: $msg" >&2; exit "$code"; }

# 0. Sanity: systemd-managed daemon (not ad-hoc)
if ! systemctl --user cat "$SERVICE" >/dev/null 2>&1; then
    die "systemd service '$SERVICE' not installed — run: neurostrata-mcp setup" 9
fi

# 1. Read db_path from config
[ -f "$CONFIG" ] || die "config not found: $CONFIG" 2
command -v jq >/dev/null || die "jq required (apt install jq)" 3
DB_PATH=$(jq -r '.db_path' "$CONFIG") || die "cannot parse $CONFIG" 2
[ -f "$DB_PATH" ] || die "db file not found: $DB_PATH" 2

log "db_path: $DB_PATH"

# 2. Stop daemon via systemd (triggers graceful shutdown + checkpoint)
DAEMON_WAS_RUNNING=false
if systemctl --user is-active --quiet "$SERVICE"; then
    DAEMON_WAS_RUNNING=true
    log "daemon running — systemctl stop $SERVICE (checkpoints + releases file)"
    systemctl --user stop "$SERVICE" || true
fi

# Never exit without the daemon back up, whatever killed us (die, set -e, signal).
restore_daemon() {
    if ${DAEMON_WAS_RUNNING:-false} && ! systemctl --user is-active --quiet "$SERVICE"; then
        log "restoring $SERVICE after exit"
        systemctl --user start "$SERVICE" || true
    fi
}
trap restore_daemon EXIT

# Wait for the db file to have no holders. Ground truth is lsof on the file,
# not the process state — pgrep -f matches itself, and a "stopped" service
# can still have a file mapping briefly.
wait_for_file_free() {
    local path="$1" timeout="${2:-30}"
    if ! command -v lsof >/dev/null 2>&1; then return 0; fi
    for _ in $(seq 1 "$timeout"); do
        if [ -z "$(lsof -t "$path" 2>/dev/null || true)" ]; then return 0; fi
        sleep 1
    done
    return 1
}

if $DAEMON_WAS_RUNNING; then
    if ! wait_for_file_free "$DB_PATH" 30; then
        HOLDERS=$(lsof -t "$DB_PATH" 2>/dev/null || systemctl --user status "$SERVICE" --no-pager)
        die "db file still held after 30s: $HOLDERS" 5
    fi
    log "daemon stopped, file released"
else
    log "daemon not running — proceeding with direct copy"
fi

# 3. lsof-verify no holders (belt + suspenders)
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

# 4b. Copy the store-state sidecar as the backup's companion, so a
# recovered store keeps the identity recorded at backup time. The
# sidecar is not optional once present: a backup that silently drops
# identity is a bad backup, under the same failure contract as above.
SIDECAR_PATH="$DB_PATH.store-state.json"
COMPANION_FILE="$BACKUP_FILE.store-state.json"

if [ -f "$SIDECAR_PATH" ]; then
    if ! cp -p "$SIDECAR_PATH" "$COMPANION_FILE"; then
        rm -f "$BACKUP_FILE" "$COMPANION_FILE"
        die "sidecar cp failed — bad backup deleted, source untouched" 7
    fi
    SRC_SIDECAR_MD5=$(md5sum "$SIDECAR_PATH" | awk '{print $1}')
    DST_SIDECAR_MD5=$(md5sum "$COMPANION_FILE" | awk '{print $1}')

    if [ "$SRC_SIDECAR_MD5" != "$DST_SIDECAR_MD5" ]; then
        rm -f "$BACKUP_FILE" "$COMPANION_FILE"
        die "sidecar md5 mismatch: src=$SRC_SIDECAR_MD5 dst=$DST_SIDECAR_MD5 — bad backup deleted, source untouched" 8
    fi

    log "sidecar OK: $COMPANION_FILE (md5=$SRC_SIDECAR_MD5)"
else
    # Unit invariant (see header): on a reused destination name, a
    # companion left by an earlier run must not survive into a backup
    # whose source has no sidecar — it would lend this db a recorded
    # identity it did not have at backup time.
    if [ -f "$COMPANION_FILE" ]; then
        rm -f "$COMPANION_FILE"
        log "removed stale sidecar companion $COMPANION_FILE — destination reused, current backup carries no recorded identity"
    fi
    log "no store-state sidecar at $SIDECAR_PATH — backup carries no recorded identity"
fi

# 4c. Copy the WAL as the backup's companion — only when the daemon was
# NOT running at backup time. A running daemon's graceful stop (step 2)
# checkpointed the WAL into the main db file, so its backup needs no
# WAL companion. On the daemon-inactive path there was no checkpoint:
# after an unclean shutdown the WAL may hold the only copy of recent
# writes, and a backup that silently drops it loses them — a bad
# backup, under the same failure contract as steps 4/4b.
WAL_PATH="$DB_PATH.wal"
WAL_COMPANION_FILE="$BACKUP_FILE.wal"

if ! $DAEMON_WAS_RUNNING && [ -f "$WAL_PATH" ]; then
    if ! cp -p "$WAL_PATH" "$WAL_COMPANION_FILE"; then
        rm -f "$BACKUP_FILE" "$COMPANION_FILE" "$WAL_COMPANION_FILE"
        die "wal cp failed — bad backup deleted, source untouched" 7
    fi
    SRC_WAL_MD5=$(md5sum "$WAL_PATH" | awk '{print $1}')
    DST_WAL_MD5=$(md5sum "$WAL_COMPANION_FILE" | awk '{print $1}')

    if [ "$SRC_WAL_MD5" != "$DST_WAL_MD5" ]; then
        rm -f "$BACKUP_FILE" "$COMPANION_FILE" "$WAL_COMPANION_FILE"
        die "wal md5 mismatch: src=$SRC_WAL_MD5 dst=$DST_WAL_MD5 — bad backup deleted, source untouched" 8
    fi

    log "wal OK: $WAL_COMPANION_FILE (md5=$SRC_WAL_MD5)"
elif ! $DAEMON_WAS_RUNNING; then
    # Unit invariant (see header): on a reused destination name, a WAL
    # companion left by an earlier run must not survive into a backup
    # whose source has no WAL — restoring it would replay unrelated
    # writes against this run's db.
    if [ -f "$WAL_COMPANION_FILE" ]; then
        rm -f "$WAL_COMPANION_FILE"
        log "removed stale WAL companion $WAL_COMPANION_FILE — destination reused, current backup carries no uncheckpointed writes"
    fi
    log "no WAL at $WAL_PATH — backup carries no uncheckpointed writes"
else
    # Daemon was running: its checkpoint means this run carries no WAL
    # companion, so the same unit invariant applies — a WAL companion
    # left at a reused destination by an earlier daemon-inactive run
    # must not survive into this backup.
    if [ -f "$WAL_COMPANION_FILE" ]; then
        rm -f "$WAL_COMPANION_FILE"
        log "removed stale WAL companion $WAL_COMPANION_FILE — destination reused, daemon-running backup carries no WAL companion"
    fi
fi

# 5. Prune old backups (pair-aware: companions are not backups in their
# own right — count/select only db-* files without the sidecar or WAL
# suffix, and delete a pruned backup's companions with it)
if [ "$MAX_BACKUPS" -gt 0 ]; then
    PRUNE_COUNT=$(ls -1t "$BACKUP_DIR"/db-* 2>/dev/null | grep -v -e '\.store-state\.json$' -e '\.wal$' | wc -l)
    if [ "$PRUNE_COUNT" -gt "$MAX_BACKUPS" ]; then
        ls -1t "$BACKUP_DIR"/db-* 2>/dev/null | grep -v -e '\.store-state\.json$' -e '\.wal$' | tail -n +$((MAX_BACKUPS + 1)) | while IFS= read -r OLD_BACKUP; do
            rm -f "$OLD_BACKUP" "$OLD_BACKUP.store-state.json" "$OLD_BACKUP.wal"
        done
        log "pruned $((PRUNE_COUNT - MAX_BACKUPS)) old backup(s), keeping $MAX_BACKUPS"
    fi
fi

# 6. Restart daemon if it was running
if $DAEMON_WAS_RUNNING; then
    log "systemctl start $SERVICE"
    systemctl --user start "$SERVICE"
    HEALTHY=false
    for _ in $(seq 1 15); do
        if curl -s -f "http://127.0.0.1:$DAEMON_PORT/health" >/dev/null 2>&1; then
            HEALTHY=true
            log "daemon healthy"
            break
        fi
        sleep 1
    done
    if [ "$HEALTHY" != true ]; then
        die "daemon did not become healthy within 15s after backup" 5
    fi
fi

log "done"
