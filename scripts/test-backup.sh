#!/usr/bin/env bash
# test-backup.sh — automated coverage for scripts/backup.sh steps 4b/4c:
# the store-state sidecar carry, the WAL carry on the daemon-inactive
# path, and pair-aware pruning.
#
# Covers:
#   1. Recorded store (db + <db>.store-state.json present): the backup's
#      companion db-<timestamp>.store-state.json is created and is
#      byte-identical to the source sidecar.
#   2. Pre-sidecar store (db only): the backup is created with no
#      companion file.
#   3. Daemon-inactive store with a WAL (db + sidecar + <db>.wal
#      present; the harness shim always reports the daemon inactive, so
#      no checkpoint happened): the backup's companion
#      db-<timestamp>.wal is created and is byte-identical to the
#      source WAL, and the db + sidecar companions are as in (1).
#   4. Pair-aware pruning: with NEUROSTRATA_MAX_BACKUPS exceeded, pruned
#      backups lose their sidecar and WAL companions with them,
#      surviving backups keep theirs, and no orphan companion is left
#      behind.
#   5. Failure contract: a sidecar-only verify failure (step 4b) exits
#      non-zero, deletes BOTH the backup db and its companion, and
#      leaves the source db and source sidecar byte-untouched.
#   6. Backup-name reuse: two daemon-inactive runs in the same second
#      (same timestamp, same destination directory) — the second run
#      overwrites the db, and companions the second run does not
#      produce (source WAL and sidecar both removed between runs)
#      must not survive from the first run.
#   7. Failed retry on a reused same-second destination: a retry whose
#      companion copy fails exits non-zero and leaves the prior good
#      backup set — db, sidecar, WAL — byte-identical, with no .tmp-*
#      litter.
#   8. Filename-timestamp pruning: an older-named backup with a newer
#      mtime is still pruned, and orphaned companions / staged .tmp-*
#      leftovers are removed.
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
# 5's corrupting cp shim); returns backup.sh's exit code.
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

# make_store <dir> <with_sidecar:0|1> [with_wal:0|1]: fixture db
# (+ sidecar) (+ WAL) and a config.json pointing at it. Prints the
# config path.
make_store() {
    local dir="$1" with_sidecar="$2" with_wal="${3:-0}"
    mkdir -p "$dir"
    printf 'fixture-db-bytes-%s\n' "$dir" > "$dir/store.db"
    if [ "$with_sidecar" = "1" ]; then
        printf '{"embedding_model":"fixture-model","dimension":4}\n' \
            > "$dir/store.db.store-state.json"
    fi
    if [ "$with_wal" = "1" ]; then
        printf 'fixture-wal-bytes-%s\n' "$dir" > "$dir/store.db.wal"
    fi
    printf '{"db_path": "%s/store.db"}\n' "$dir" > "$dir/config.json"
    printf '%s/config.json' "$dir"
}

# count_backups <dir>: db-* files excluding sidecar and WAL companions.
count_backups() {
    local dir="$1" f n=0
    shopt -s nullglob
    for f in "$dir"/db-*; do
        case "$f" in
            *.store-state.json) ;;
            *.wal) ;;
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
            *.wal) ;;
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

# --- Scenario 3: daemon-inactive store with a WAL carries it -------------
# The shim forces the daemon-inactive path, so no checkpoint folded the
# WAL into the db before the copy: the WAL holds writes the db file
# alone does not, and the backup must carry it as a .wal companion.
echo "== scenario 3: daemon-inactive store with WAL (db + sidecar + wal)"
S3="$WORK/s3"
CONFIG3="$(make_store "$S3/store" 1 1)"
if ! run_backup "$CONFIG3" "$S3/backups" 7; then
    cat "$WORK/last-run.log" >&2
    fail "scenario 3: backup.sh exited non-zero (see log above)"
fi
pass "scenario 3: backup.sh exits 0"

[ "$(count_backups "$S3/backups")" = "1" ] \
    || fail "scenario 3: expected exactly 1 backup, found $(count_backups "$S3/backups")"
pass "scenario 3: exactly one backup file created"

BACKUP3="$(only_backup "$S3/backups")"
cmp -s "$S3/store/store.db" "$BACKUP3" \
    || fail "scenario 3: backup bytes differ from fixture db"
pass "scenario 3: backup is byte-identical to the fixture db"

[ -f "$BACKUP3.wal" ] \
    || fail "scenario 3: WAL companion $BACKUP3.wal missing"
pass "scenario 3: WAL companion db-*.wal exists"

cmp -s "$S3/store/store.db.wal" "$BACKUP3.wal" \
    || fail "scenario 3: WAL companion bytes differ from fixture WAL"
pass "scenario 3: WAL companion is byte-identical to the fixture WAL"

cmp -s "$S3/store/store.db.store-state.json" "$BACKUP3.store-state.json" \
    || fail "scenario 3: sidecar companion missing or bytes differ from fixture sidecar"
pass "scenario 3: sidecar companion exists and is byte-identical to the fixture sidecar"

# --- Scenario 4: pair-aware pruning ---------------------------------------
# backup.sh selects prune victims by filename timestamp
# (db-YYYYMMDD-HHMMSS, `sort -r`), so seeded backups get both old names
# and old, name-ordered mtimes; seeded names can then never collide
# with the new run's timestamp.
echo "== scenario 4: pair-aware pruning (MAX_BACKUPS=2)"
S4="$WORK/s4"
CONFIG4="$(make_store "$S4/store" 1)"
mkdir -p "$S4/backups"
seed() { # seed <name> <touch-date> <with_companion:0|1> <with_wal:0|1>
    printf 'seeded backup %s\n' "$1" > "$S4/backups/$1"
    touch -d "$2" "$S4/backups/$1"
    if [ "$3" = "1" ]; then
        printf '{"seeded":true}\n' > "$S4/backups/$1.store-state.json"
        touch -d "$2" "$S4/backups/$1.store-state.json"
    fi
    if [ "$4" = "1" ]; then
        printf 'seeded wal %s\n' "$1" > "$S4/backups/$1.wal"
        touch -d "$2" "$S4/backups/$1.wal"
    fi
}
seed db-20000101-000001 "2000-01-01 00:00:01" 1 1
seed db-20000102-000002 "2000-01-02 00:00:02" 1 1
seed db-20000103-000003 "2000-01-03 00:00:03" 0 1
seed db-20000104-000004 "2000-01-04 00:00:04" 1 1

if ! run_backup "$CONFIG4" "$S4/backups" 2; then
    cat "$WORK/last-run.log" >&2
    fail "scenario 4: backup.sh exited non-zero (see log above)"
fi
pass "scenario 4: backup.sh exits 0"

[ "$(count_backups "$S4/backups")" = "2" ] \
    || fail "scenario 4: expected 2 non-companion backups after pruning, found $(count_backups "$S4/backups")"
pass "scenario 4: total non-companion backups == MAX_BACKUPS (2)"

for pruned in db-20000101-000001 db-20000102-000002 db-20000103-000003; do
    [ ! -e "$S4/backups/$pruned" ] \
        || fail "scenario 4: backup $pruned should have been pruned"
    [ ! -e "$S4/backups/$pruned.store-state.json" ] \
        || fail "scenario 4: pruned backup $pruned left its companion behind"
done
pass "scenario 4: pruned backups and their companions are gone"

for pruned in db-20000101-000001 db-20000102-000002 db-20000103-000003; do
    [ ! -e "$S4/backups/$pruned.wal" ] \
        || fail "scenario 4: pruned backup $pruned left its WAL companion behind"
done
pass "scenario 4: pruned backups' WAL companions are gone"

[ -f "$S4/backups/db-20000104-000004" ] \
    || fail "scenario 4: newest seeded backup should have survived pruning"
[ -f "$S4/backups/db-20000104-000004.store-state.json" ] \
    || fail "scenario 4: surviving backup lost its companion"
pass "scenario 4: surviving seeded backup keeps its companion"

[ -f "$S4/backups/db-20000104-000004.wal" ] \
    || fail "scenario 4: surviving backup lost its WAL companion"
pass "scenario 4: surviving seeded backup keeps its WAL companion"

shopt -s nullglob
for companion in "$S4/backups"/*.store-state.json; do
    partner="${companion%.store-state.json}"
    [ -f "$partner" ] \
        || fail "scenario 4: orphan companion $companion has no db-* partner"
done
shopt -u nullglob
pass "scenario 4: no orphan companion exists"

shopt -s nullglob
for wal_companion in "$S4/backups"/*.wal; do
    partner="${wal_companion%.wal}"
    [ -f "$partner" ] \
        || fail "scenario 4: orphan WAL companion $wal_companion has no db-* partner"
done
shopt -u nullglob
pass "scenario 4: no orphan WAL companion exists"

# --- Scenario 5: sidecar failure contract (step 4b) -----------------------
# A cp shim corrupts ONLY the companion copy: it delegates to the real
# cp, then appends a byte when the destination is a *.store-state.json
# path. The db copy passes through uncorrupted, so backup.sh clears
# step 4's db md5 check and reaches step 4b, where the companion's real
# md5 mismatch must delete BOTH copies and exit non-zero. (Shimming cp,
# not md5sum: the mismatch is then genuine — real bytes checked by the
# real md5sum — instead of a lying verifier.)
echo "== scenario 5: sidecar verify failure deletes both copies"
S5="$WORK/s5"
CONFIG5="$(make_store "$S5/store" 1)"
cp -p "$S5/store/store.db" "$S5/expected.db"
cp -p "$S5/store/store.db.store-state.json" "$S5/expected.sidecar"

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
    *.store-state.json*) printf 'x' >> "$dest" ;;
esac
EOF
chmod +x "$CP_SHIM_DIR/cp"

if run_backup_path "$CP_SHIM_DIR" "$CONFIG5" "$S5/backups" 7; then
    cat "$WORK/last-run.log" >&2
    fail "scenario 5: backup.sh exited 0 on a sidecar verify failure (see log above)"
fi
pass "scenario 5: backup.sh exits non-zero on sidecar verify failure"

[ "$(count_backups "$S5/backups")" = "0" ] \
    || fail "scenario 5: backup db file survived a sidecar verify failure"
pass "scenario 5: backup db file is deleted on sidecar verify failure"

[ "$(count_companions "$S5/backups")" = "0" ] \
    || fail "scenario 5: companion file survived a sidecar verify failure"
pass "scenario 5: companion file is deleted on sidecar verify failure"

cmp -s "$S5/expected.db" "$S5/store/store.db" \
    || fail "scenario 5: source db bytes changed"
pass "scenario 5: source db is byte-untouched"

cmp -s "$S5/expected.sidecar" "$S5/store/store.db.store-state.json" \
    || fail "scenario 5: source sidecar bytes changed"
pass "scenario 5: source sidecar is byte-untouched"

# --- Scenario 6: backup-name reuse drops stale companions ---------------
# backup.sh names backups with second-resolution timestamps, so two
# runs in the same second share a destination. Shim `date` to a fixed
# stamp for the backup-name call only (log() also calls date, with a
# different format, and must delegate to the real date). Run 1 carries
# both companions; run 2's source has neither a WAL nor a sidecar, so
# its backup set must carry neither either — a companion left over
# from run 1 would pair run 2's db with run 1's unrelated writes /
# identity on restore.
echo "== scenario 6: backup-name reuse (same-second double run)"
S6="$WORK/s6"
CONFIG6="$(make_store "$S6/store" 1 1)"

DATE_SHIM_DIR="$WORK/date-shim"
mkdir -p "$DATE_SHIM_DIR"
cat > "$DATE_SHIM_DIR/date" <<'EOF'
#!/usr/bin/env bash
# Fixed stamp for the backup-name format; delegate everything else to
# the real date (first date on PATH outside this shim's own directory).
if [ "${1:-}" = "+%Y%m%d-%H%M%S" ]; then
    printf '20990102-030405\n'
    exit 0
fi
self_dir="$(cd "$(dirname "$0")" && pwd)"
OLDIFS="$IFS"; IFS=:
for d in $PATH; do
    [ "$d" = "$self_dir" ] && continue
    if [ -x "$d/date" ]; then IFS="$OLDIFS"; exec "$d/date" "$@"; fi
done
IFS="$OLDIFS"
exit 127
EOF
chmod +x "$DATE_SHIM_DIR/date"

if ! run_backup_path "$DATE_SHIM_DIR" "$CONFIG6" "$S6/backups" 7; then
    cat "$WORK/last-run.log" >&2
    fail "scenario 6: first backup.sh run exited non-zero (see log above)"
fi
pass "scenario 6: first run exits 0"

BACKUP6="$S6/backups/db-20990102-030405"
[ -f "$BACKUP6" ] \
    || fail "scenario 6: first run did not create the fixed-stamp backup $BACKUP6"
pass "scenario 6: first run creates the fixed-stamp backup"

cmp -s "$S6/store/store.db" "$BACKUP6" \
    || fail "scenario 6: first backup bytes differ from fixture db"
pass "scenario 6: first backup is byte-identical to the fixture db"

[ -f "$BACKUP6.wal" ] \
    || fail "scenario 6: first run WAL companion $BACKUP6.wal missing"
pass "scenario 6: first run WAL companion exists"

cmp -s "$S6/store/store.db.wal" "$BACKUP6.wal" \
    || fail "scenario 6: first run WAL companion bytes differ from fixture WAL"
pass "scenario 6: first run WAL companion is byte-identical to the fixture WAL"

cmp -s "$S6/store/store.db.store-state.json" "$BACKUP6.store-state.json" \
    || fail "scenario 6: first run sidecar companion missing or bytes differ"
pass "scenario 6: first run sidecar companion is byte-identical to the fixture sidecar"

# Change the source: new db bytes, no WAL, no sidecar.
printf 'fixture-db-bytes-run2\n' > "$S6/store/store.db"
rm -f "$S6/store/store.db.wal" "$S6/store/store.db.store-state.json"

if ! run_backup_path "$DATE_SHIM_DIR" "$CONFIG6" "$S6/backups" 7; then
    cat "$WORK/last-run.log" >&2
    fail "scenario 6: second backup.sh run exited non-zero (see log above)"
fi
pass "scenario 6: second run (same stamp, same dir) exits 0"

[ "$(count_backups "$S6/backups")" = "1" ] \
    || fail "scenario 6: expected exactly 1 backup after reuse, found $(count_backups "$S6/backups")"
pass "scenario 6: exactly one backup file exists after reuse"

cmp -s "$S6/store/store.db" "$BACKUP6" \
    || fail "scenario 6: reused backup does not hold the second run's db bytes"
pass "scenario 6: reused backup holds the second run's db bytes"

[ ! -e "$BACKUP6.wal" ] \
    || fail "scenario 6: stale WAL companion from the first run survived the second run"
pass "scenario 6: no WAL companion survives a run that produced none"

[ ! -e "$BACKUP6.store-state.json" ] \
    || fail "scenario 6: stale sidecar companion from the first run survived the second run"
pass "scenario 6: no sidecar companion survives a run that produced none"

# --- Scenario 7: failed retry preserves the prior same-second set ---------
# The timestamp has second resolution, so a retry can reuse the exact
# destination name of a good prior backup. A retry that fails after
# staging (its WAL companion copy fails, via a cp shim) must commit
# nothing: the prior backup's db, sidecar, and WAL stay byte-identical,
# and no .tmp-* litter remains.
echo "== scenario 7: failed retry on a reused same-second destination"
S7="$WORK/s7"
CONFIG7="$(make_store "$S7/store" 1 1)"

if ! run_backup_path "$DATE_SHIM_DIR" "$CONFIG7" "$S7/backups" 7; then
    cat "$WORK/last-run.log" >&2
    fail "scenario 7: first backup.sh run exited non-zero (see log above)"
fi
pass "scenario 7: first run exits 0"

BACKUP7="$S7/backups/db-20990102-030405"
[ -f "$BACKUP7" ] \
    || fail "scenario 7: first run did not create the fixed-stamp backup $BACKUP7"
cp -p "$BACKUP7" "$S7/expected.db"
cp -p "$BACKUP7.store-state.json" "$S7/expected.sidecar"
cp -p "$BACKUP7.wal" "$S7/expected.wal"

# New writes land in the source between the two runs.
printf 'fixture-db-bytes-run2\n' > "$S7/store/store.db"

CP_FAIL_WAL_SHIM_DIR="$WORK/cp-fail-wal-shim"
mkdir -p "$CP_FAIL_WAL_SHIM_DIR"
cat > "$CP_FAIL_WAL_SHIM_DIR/cp" <<'EOF'
#!/usr/bin/env bash
# Delegate to the real cp, then refuse the staged WAL companion: the
# staged destination always carries the run's .tmp-$$ suffix.
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
case "$dest" in
    *.wal.tmp-*) echo "simulated wal companion copy failure: $dest" >&2; exit 1 ;;
esac
"$real_cp" "$@" || exit $?
EOF
chmod +x "$CP_FAIL_WAL_SHIM_DIR/cp"

if run_backup_path "$CP_FAIL_WAL_SHIM_DIR:$DATE_SHIM_DIR" "$CONFIG7" "$S7/backups" 7; then
    cat "$WORK/last-run.log" >&2
    fail "scenario 7: backup.sh exited 0 on a staged WAL companion failure (see log above)"
fi
pass "scenario 7: retry exits non-zero on the staged WAL companion failure"

cmp -s "$S7/expected.db" "$BACKUP7" \
    || fail "scenario 7: the prior good backup db was disturbed by the failed retry"
pass "scenario 7: prior good backup db byte-identical after the failed retry"
cmp -s "$S7/expected.sidecar" "$BACKUP7.store-state.json" \
    || fail "scenario 7: the prior good backup's sidecar companion was disturbed"
pass "scenario 7: prior good sidecar companion byte-identical after the failed retry"
cmp -s "$S7/expected.wal" "$BACKUP7.wal" \
    || fail "scenario 7: the prior good backup's WAL companion was disturbed"
pass "scenario 7: prior good WAL companion byte-identical after the failed retry"

tmp_litter=0
shopt -s nullglob
for tmp in "$S7/backups"/*.tmp-*; do
    [ -e "$tmp" ] || continue
    echo "scenario 7: staged leftover $tmp survived the failed retry" >&2
    tmp_litter=1
done
shopt -u nullglob
[ "$tmp_litter" = "0" ] \
    || fail "scenario 7: failed retry left .tmp-* litter in the backup dir"
pass "scenario 7: no .tmp-* litter remains after the failed retry"

# --- Scenario 8: filename-timestamp pruning + orphan cleanup --------------
# Pruning selects victims by the db-YYYYMMDD-HHMMSS filename, not by mtime:
# the oldest-named backup below carries the NEWEST mtime, so an
# mtime-ordered prune would wrongly keep it. Orphan companions (no base
# db-* file) and staged .tmp-* leftovers are swept by the same step.
echo "== scenario 8: pruning by filename timestamp; orphan companions removed"
S8="$WORK/s8"
CONFIG8="$(make_store "$S8/store" 1)"
mkdir -p "$S8/backups"
seed8() { # seed8 <name> <touch-date> <with_companion:0|1>
    printf 'seeded backup %s\n' "$1" > "$S8/backups/$1"
    touch -d "$2" "$S8/backups/$1"
    if [ "$3" = "1" ]; then
        printf '{"seeded":true}\n' > "$S8/backups/$1.store-state.json"
        touch -d "$2" "$S8/backups/$1.store-state.json"
    fi
}
seed8 db-20000101-000001 "2000-01-01 00:00:01" 1
seed8 db-20000102-000002 "2000-01-02 00:00:02" 1
seed8 db-20000103-000003 "2000-01-03 00:00:03" 1
seed8 db-20000104-000004 "2000-01-04 00:00:04" 1
# Invert the mtime story: the oldest-named backup gets the newest mtime.
touch -d "2020-05-05 00:00:00" "$S8/backups/db-20000101-000001"
touch -d "2000-01-01 00:00:00" "$S8/backups/db-20000104-000004"
# Orphan companions whose base backups no longer exist.
printf 'orphan sidecar\n' > "$S8/backups/db-20000100-000000.store-state.json"
printf 'orphan wal\n' > "$S8/backups/db-20000100-000000.wal"
# A staged leftover from an interrupted run.
printf 'staged leftover\n' > "$S8/backups/db-20000101-000001.tmp-999999"

if ! run_backup "$CONFIG8" "$S8/backups" 2; then
    cat "$WORK/last-run.log" >&2
    fail "scenario 8: backup.sh exited non-zero (see log above)"
fi
pass "scenario 8: backup.sh exits 0"

[ -f "$S8/backups/db-20000104-000004" ] \
    || fail "scenario 8: newest-named backup should survive pruning despite the oldest mtime"
pass "scenario 8: newest-named backup survives despite the oldest mtime"
[ ! -e "$S8/backups/db-20000101-000001" ] \
    || fail "scenario 8: oldest-named backup with the newest mtime should have been pruned"
pass "scenario 8: oldest-named backup pruned despite the newest mtime"
[ "$(count_backups "$S8/backups")" = "2" ] \
    || fail "scenario 8: expected 2 backups after pruning, found $(count_backups "$S8/backups")"
pass "scenario 8: exactly MAX_BACKUPS (2) backups remain"
for pruned in db-20000101-000001 db-20000102-000002 db-20000103-000003; do
    [ ! -e "$S8/backups/$pruned.store-state.json" ] \
        || fail "scenario 8: pruned backup $pruned left its companion behind"
done
pass "scenario 8: pruned backups' companions are gone"
[ ! -e "$S8/backups/db-20000100-000000.store-state.json" ] \
    || fail "scenario 8: orphan sidecar companion survived"
[ ! -e "$S8/backups/db-20000100-000000.wal" ] \
    || fail "scenario 8: orphan WAL companion survived"
pass "scenario 8: orphan companions of vanished backups are removed"
[ ! -e "$S8/backups/db-20000101-000001.tmp-999999" ] \
    || fail "scenario 8: staged .tmp-* leftover survived"
pass "scenario 8: staged .tmp-* leftovers are removed"

NEW8=""
shopt -s nullglob
for f in "$S8/backups"/db-*; do
    case "$f" in
        *.store-state.json|*.wal|*.tmp-*) ;;
        db-20000104-000004) ;;
        *) NEW8="$f" ;;
    esac
done
shopt -u nullglob
[ -n "$NEW8" ] \
    || fail "scenario 8: the new run's backup is missing"
[ -f "$NEW8.store-state.json" ] \
    || fail "scenario 8: the new run's backup lost its sidecar companion"
pass "scenario 8: the new run's backup carries its sidecar companion"

echo "PASS: all $CHECKS checks passed (backup sidecar + WAL carry + pair-aware pruning + sidecar failure contract + backup-name reuse + failed-retry preservation + filename-timestamp pruning)"
