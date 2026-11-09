#!/usr/bin/env bash
# test-backup.sh — automated coverage for scripts/backup.sh step 4b:
# the store-state sidecar carry and pair-aware pruning.
#
# Covers:
#   1. Recorded store (db + <db>.store-state.json present): the backup's
#      companion db-<timestamp>.store-state.json is created and is
#      byte-identical to the source sidecar.
#   2. Pre-sidecar store (db only): the backup is created with no
#      companion file.
#   3. Pair-aware pruning: with NEUROSTRATA_MAX_BACKUPS exceeded, pruned
#      backups lose their companions with them, surviving backups keep
#      theirs, and no orphan companion is left behind.
#   4. Failure contract: a sidecar-only verify failure (step 4b) exits
#      non-zero, deletes BOTH the backup db and its companion, and
#      leaves the source db and source sidecar byte-untouched.
#
# How to run: scripts/test-backup.sh
#   Exits 0 and prints a PASS summary when every assertion holds; exits
#   non-zero with a FAIL message on the first failed assertion.
#   BACKUP_SH may point at an alternate backup.sh (default: the
#   backup.sh next to this script); the harness otherwise always runs
#   the real script, in a throwaway sandbox (fake systemctl on PATH,
#   fixture config/db, env overrides) that is removed on exit.
#
# Note: no CI workflow runs this script yet; wiring it into CI is a
# separate maintainer decision.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BACKUP_SH="${BACKUP_SH:-$SCRIPT_DIR/backup.sh}"
[ -f "$BACKUP_SH" ] || { echo "FAIL: backup.sh not found: $BACKUP_SH" >&2; exit 1; }
command -v jq >/dev/null || { echo "FAIL: jq required (backup.sh needs it)" >&2; exit 1; }

WORK="$(mktemp -d)"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

CHECKS=0
fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { CHECKS=$((CHECKS + 1)); echo "ok $CHECKS - $*"; }

# --- Sandbox: fake systemctl -------------------------------------------
# backup.sh step 0 requires `systemctl --user cat <service>` to succeed;
# reporting the service INACTIVE makes backup.sh take its direct-copy
# path (no stop/start, no health check).
SHIM_DIR="$WORK/shim"
mkdir -p "$SHIM_DIR"
cat > "$SHIM_DIR/systemctl" <<'EOF'
#!/usr/bin/env bash
sub=""
for a in "$@"; do
    case "$a" in
        --*) ;;
        *) if [ -z "$sub" ]; then sub="$a"; fi ;;
    esac
done
case "$sub" in
    cat) exit 0 ;;
    is-active) exit 3 ;;  # inactive
    *) exit 0 ;;
esac
EOF
chmod +x "$SHIM_DIR/systemctl"

# --- Helpers -------------------------------------------------------------
# run_backup <config> <backup_dir> <max_backups>; returns backup.sh's
# exit code (never aborts the harness under set -e).
run_backup() {
    local config="$1" backup_dir="$2" max_backups="$3" rc=0
    env PATH="$SHIM_DIR:$PATH" \
        NEUROSTRATA_CONFIG="$config" \
        NEUROSTRATA_BACKUP_DIR="$backup_dir" \
        NEUROSTRATA_MAX_BACKUPS="$max_backups" \
        NEUROSTRATA_SERVICE="neurostrata-test.service" \
        bash "$BACKUP_SH" >"$WORK/last-run.log" 2>&1 || rc=$?
    return "$rc"
}

# run_backup_path <extra_path_dir> <config> <backup_dir> <max_backups>:
# like run_backup, with one extra directory prepended to PATH (scenario
# 4's corrupting cp shim); returns backup.sh's exit code.
run_backup_path() {
    local extra_path="$1" config="$2" backup_dir="$3" max_backups="$4" rc=0
    env PATH="$extra_path:$SHIM_DIR:$PATH" \
        NEUROSTRATA_CONFIG="$config" \
        NEUROSTRATA_BACKUP_DIR="$backup_dir" \
        NEUROSTRATA_MAX_BACKUPS="$max_backups" \
        NEUROSTRATA_SERVICE="neurostrata-test.service" \
        bash "$BACKUP_SH" >"$WORK/last-run.log" 2>&1 || rc=$?
    return "$rc"
}

# make_store <dir> <with_sidecar:0|1>: fixture db (+ sidecar) and a
# config.json pointing at it. Prints the config path.
make_store() {
    local dir="$1" with_sidecar="$2"
    mkdir -p "$dir"
    printf 'fixture-db-bytes-%s\n' "$dir" > "$dir/store.db"
    if [ "$with_sidecar" = "1" ]; then
        printf '{"embedding_model":"fixture-model","dimension":4}\n' \
            > "$dir/store.db.store-state.json"
    fi
    printf '{"db_path": "%s/store.db"}\n' "$dir" > "$dir/config.json"
    printf '%s/config.json' "$dir"
}

# count_backups <dir>: db-* files excluding sidecar companions.
count_backups() {
    local dir="$1" f n=0
    shopt -s nullglob
    for f in "$dir"/db-*; do
        case "$f" in
            *.store-state.json) ;;
            *) n=$((n + 1)) ;;
        esac
    done
    shopt -u nullglob
    printf '%s' "$n"
}

# count_companions <dir>: *.store-state.json files.
count_companions() {
    local dir="$1" f n=0
    shopt -s nullglob
    for f in "$dir"/*.store-state.json; do n=$((n + 1)); done
    shopt -u nullglob
    printf '%s' "$n"
}

# only_backup <dir>: path of the single non-companion backup.
only_backup() {
    local dir="$1" f found=""
    shopt -s nullglob
    for f in "$dir"/db-*; do
        case "$f" in
            *.store-state.json) ;;
            *) found="$f" ;;
        esac
    done
    shopt -u nullglob
    printf '%s' "$found"
}

# --- Scenario 1: recorded store carries its sidecar ----------------------
echo "== scenario 1: recorded store (db + sidecar)"
S1="$WORK/s1"
CONFIG1="$(make_store "$S1/store" 1)"
if ! run_backup "$CONFIG1" "$S1/backups" 7; then
    cat "$WORK/last-run.log" >&2
    fail "scenario 1: backup.sh exited non-zero (see log above)"
fi
pass "scenario 1: backup.sh exits 0"

[ "$(count_backups "$S1/backups")" = "1" ] \
    || fail "scenario 1: expected exactly 1 backup, found $(count_backups "$S1/backups")"
pass "scenario 1: exactly one backup file created"

BACKUP1="$(only_backup "$S1/backups")"
cmp -s "$S1/store/store.db" "$BACKUP1" \
    || fail "scenario 1: backup bytes differ from fixture db"
pass "scenario 1: backup is byte-identical to the fixture db"

[ -f "$BACKUP1.store-state.json" ] \
    || fail "scenario 1: companion $BACKUP1.store-state.json missing"
pass "scenario 1: companion db-*.store-state.json exists"

cmp -s "$S1/store/store.db.store-state.json" "$BACKUP1.store-state.json" \
    || fail "scenario 1: companion bytes differ from fixture sidecar"
pass "scenario 1: companion is byte-identical to the fixture sidecar"

# --- Scenario 2: pre-sidecar store carries no companion ------------------
echo "== scenario 2: pre-sidecar store (db only)"
S2="$WORK/s2"
CONFIG2="$(make_store "$S2/store" 0)"
if ! run_backup "$CONFIG2" "$S2/backups" 7; then
    cat "$WORK/last-run.log" >&2
    fail "scenario 2: backup.sh exited non-zero (see log above)"
fi
pass "scenario 2: backup.sh exits 0"

[ "$(count_backups "$S2/backups")" = "1" ] \
    || fail "scenario 2: expected exactly 1 backup, found $(count_backups "$S2/backups")"
pass "scenario 2: exactly one backup file created"

[ "$(count_companions "$S2/backups")" = "0" ] \
    || fail "scenario 2: unexpected companion file in backup dir"
pass "scenario 2: no companion file exists in the backup dir"

# --- Scenario 3: pair-aware pruning ---------------------------------------
# backup.sh selects prune victims with `ls -1t` (mtime, newest first),
# so seeded backups get both old names and old, name-ordered mtimes;
# seeded names can then never collide with the new run's timestamp.
echo "== scenario 3: pair-aware pruning (MAX_BACKUPS=2)"
S3="$WORK/s3"
CONFIG3="$(make_store "$S3/store" 1)"
mkdir -p "$S3/backups"
seed() { # seed <name> <touch-date> <with_companion:0|1>
    printf 'seeded backup %s\n' "$1" > "$S3/backups/$1"
    touch -d "$2" "$S3/backups/$1"
    if [ "$3" = "1" ]; then
        printf '{"seeded":true}\n' > "$S3/backups/$1.store-state.json"
        touch -d "$2" "$S3/backups/$1.store-state.json"
    fi
}
seed db-20000101-000001 "2000-01-01 00:00:01" 1
seed db-20000102-000002 "2000-01-02 00:00:02" 1
seed db-20000103-000003 "2000-01-03 00:00:03" 0
seed db-20000104-000004 "2000-01-04 00:00:04" 1

if ! run_backup "$CONFIG3" "$S3/backups" 2; then
    cat "$WORK/last-run.log" >&2
    fail "scenario 3: backup.sh exited non-zero (see log above)"
fi
pass "scenario 3: backup.sh exits 0"

[ "$(count_backups "$S3/backups")" = "2" ] \
    || fail "scenario 3: expected 2 non-companion backups after pruning, found $(count_backups "$S3/backups")"
pass "scenario 3: total non-companion backups == MAX_BACKUPS (2)"

for pruned in db-20000101-000001 db-20000102-000002 db-20000103-000003; do
    [ ! -e "$S3/backups/$pruned" ] \
        || fail "scenario 3: backup $pruned should have been pruned"
    [ ! -e "$S3/backups/$pruned.store-state.json" ] \
        || fail "scenario 3: pruned backup $pruned left its companion behind"
done
pass "scenario 3: pruned backups and their companions are gone"

[ -f "$S3/backups/db-20000104-000004" ] \
    || fail "scenario 3: newest seeded backup should have survived pruning"
[ -f "$S3/backups/db-20000104-000004.store-state.json" ] \
    || fail "scenario 3: surviving backup lost its companion"
pass "scenario 3: surviving seeded backup keeps its companion"

shopt -s nullglob
for companion in "$S3/backups"/*.store-state.json; do
    partner="${companion%.store-state.json}"
    [ -f "$partner" ] \
        || fail "scenario 3: orphan companion $companion has no db-* partner"
done
shopt -u nullglob
pass "scenario 3: no orphan companion exists"

# --- Scenario 4: sidecar failure contract (step 4b) -----------------------
# A cp shim corrupts ONLY the companion copy: it delegates to the real
# cp, then appends a byte when the destination is a *.store-state.json
# path. The db copy passes through uncorrupted, so backup.sh clears
# step 4's db md5 check and reaches step 4b, where the companion's real
# md5 mismatch must delete BOTH copies and exit non-zero. (Shimming cp,
# not md5sum: the mismatch is then genuine — real bytes checked by the
# real md5sum — instead of a lying verifier.)
echo "== scenario 4: sidecar verify failure deletes both copies"
S4="$WORK/s4"
CONFIG4="$(make_store "$S4/store" 1)"
cp -p "$S4/store/store.db" "$S4/expected.db"
cp -p "$S4/store/store.db.store-state.json" "$S4/expected.sidecar"

CP_SHIM_DIR="$WORK/cp-shim"
mkdir -p "$CP_SHIM_DIR"
cat > "$CP_SHIM_DIR/cp" <<'EOF'
#!/usr/bin/env bash
# Delegate to the real cp (first cp on PATH outside this shim's own
# directory), then corrupt a companion destination by appending a byte.
self_dir="$(cd "$(dirname "$0")" && pwd)"
real_cp=""
OLDIFS="$IFS"; IFS=:
for d in $PATH; do
    [ "$d" = "$self_dir" ] && continue
    if [ -x "$d/cp" ]; then real_cp="$d/cp"; break; fi
done
IFS="$OLDIFS"
[ -n "$real_cp" ] || exit 127
dest=""
for a in "$@"; do dest="$a"; done
"$real_cp" "$@" || exit $?
case "$dest" in
    *.store-state.json) printf 'x' >> "$dest" ;;
esac
EOF
chmod +x "$CP_SHIM_DIR/cp"

if run_backup_path "$CP_SHIM_DIR" "$CONFIG4" "$S4/backups" 7; then
    cat "$WORK/last-run.log" >&2
    fail "scenario 4: backup.sh exited 0 on a sidecar verify failure (see log above)"
fi
pass "scenario 4: backup.sh exits non-zero on sidecar verify failure"

[ "$(count_backups "$S4/backups")" = "0" ] \
    || fail "scenario 4: backup db file survived a sidecar verify failure"
pass "scenario 4: backup db file is deleted on sidecar verify failure"

[ "$(count_companions "$S4/backups")" = "0" ] \
    || fail "scenario 4: companion file survived a sidecar verify failure"
pass "scenario 4: companion file is deleted on sidecar verify failure"

cmp -s "$S4/expected.db" "$S4/store/store.db" \
    || fail "scenario 4: source db bytes changed"
pass "scenario 4: source db is byte-untouched"

cmp -s "$S4/expected.sidecar" "$S4/store/store.db.store-state.json" \
    || fail "scenario 4: source sidecar bytes changed"
pass "scenario 4: source sidecar is byte-untouched"

echo "PASS: all $CHECKS checks passed (backup sidecar carry + pair-aware pruning + sidecar failure contract)"
