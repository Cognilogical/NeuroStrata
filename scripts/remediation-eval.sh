#!/usr/bin/env bash
# Run one registered remediation eval suite and emit JSON evidence.
# Usage: scripts/remediation-eval.sh <suite> [--json <path>]
# Refuses tracked dirty trees unless EVAL_ALLOW_DIRTY=1.
set -euo pipefail

suite=${1:?usage: remediation-eval.sh <suite> [--json <path>]}
shift || true
out=""
if [ "${1:-}" = "--json" ]; then
  out=${2:?--json needs a path}
fi

# GNU `realpath -m` resolves nonexistent paths; stock macOS does not ship it.
# Resolve existing ancestors with pwd -P and follow final symlinks by hand so
# protected-path checks and output operations see the same canonical target.
abspath() {
  local path=$1 parent base target tries=0
  if command -v realpath >/dev/null 2>&1; then
    if realpath -m -- "$path" >/dev/null 2>&1; then
      realpath -m -- "$path"
      return 0
    fi
    if [ -e "$path" ] && realpath "$path" >/dev/null 2>&1; then
      realpath "$path"
      return 0
    fi
  fi
  parent=$(dirname "$path")
  base=$(basename "$path")
  if [ -d "$parent" ]; then
    path="$(cd "$parent" && pwd -P)/$base"
  else
    case $path in /*) ;; *) path="$PWD/$path" ;; esac
  fi
  while [ -L "$path" ]; do
    tries=$((tries + 1))
    if [ "$tries" -gt 40 ]; then
      echo "symlink loop while resolving $path" >&2
      return 1
    fi
    target=$(readlink "$path")
    case $target in
      /*) path=$target ;;
      *) path="$(dirname "$path")/$target" ;;
    esac
    parent=$(dirname "$path")
    base=$(basename "$path")
    if [ -d "$parent" ]; then
      path="$(cd "$parent" && pwd -P)/$base"
    fi
  done
  printf '%s\n' "$path"
}

root=$(git rev-parse --show-toplevel)
root_abs=$(abspath "$root")
registry=${EVAL_REGISTRY:-$root/eval/registry.json}
registry_abs=$(abspath "$registry")
[ -f "$registry_abs" ] || { echo "registry not found: $registry" >&2; exit 2; }

# A linked worktree keeps its metadata outside $root/.git.
git_dir=$(git -C "$root" rev-parse --absolute-git-dir 2>/dev/null || git -C "$root" rev-parse --git-dir)
case $git_dir in
  /*) git_dir_abs=$(abspath "$git_dir") ;;
  *) git_dir_abs=$(abspath "$root/$git_dir") ;;
esac

lock_timeout=${REMEDIATION_EVAL_LOCK_TIMEOUT:-30}
case $lock_timeout in
  ''|*[!0-9]*) echo "invalid lock timeout: $lock_timeout" >&2; exit 2 ;;
esac

run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$-$RANDOM"
commit=$(git -C "$root" rev-parse HEAD)
reg_snapshot=$(mktemp)
cp -f "$registry_abs" "$reg_snapshot"
log=""
tmp_out=""
lock_fd=""
lock_dir=""
lock_owned=0
cleanup() {
  [ -z "$log" ] || rm -f -- "$log"
  [ -z "$tmp_out" ] || rm -f -- "$tmp_out"
  rm -f -- "$reg_snapshot"
  [ -z "$lock_fd" ] || { exec {lock_fd}>&- 2>/dev/null || true; }
  if [ "$lock_owned" = 1 ] && [ -n "$lock_dir" ]; then
    rm -f -- "$lock_dir/owner" 2>/dev/null || true
    rmdir "$lock_dir" 2>/dev/null || true
  fi
}
trap cleanup EXIT
trap 'exit 2' TERM

secure_lock_root() {
  local dir=$1 uid owner mode
  if [ -L "$dir" ]; then
    echo "refusing symlink lock directory: $dir" >&2
    exit 2
  fi
  if [ ! -d "$dir" ]; then
    (umask 077; mkdir -p "$dir") || exit 2
  fi
  uid=$(id -u)
  owner=$(stat -c %u "$dir" 2>/dev/null || stat -f %u "$dir" 2>/dev/null || printf '%s' "$uid")
  if [ "$owner" != "$uid" ]; then
    echo "refusing lock directory owned by another user: $dir" >&2
    exit 2
  fi
  chmod 700 "$dir" 2>/dev/null || true
  mode=$(stat -c %a "$dir" 2>/dev/null || stat -f %Lp "$dir" 2>/dev/null || printf 700)
  case $mode in
    700|0700) ;;
    *) echo "refusing insecure lock directory mode $mode: $dir" >&2; exit 2 ;;
  esac
}

if [ -n "$out" ]; then
  mkdir -p "$(dirname "$out")"
  out_abs=$(abspath "$out")
  script_abs=$(abspath "$root/scripts/remediation-eval.sh")
  reject_output() {
    echo "refusing --json path that names a protected repository input: $out ($1)" >&2
    exit 2
  }
  case "$out_abs" in
    "$registry_abs") reject_output "active registry" ;;
    "$script_abs") reject_output "evaluator script" ;;
    "$root_abs"|"$root_abs/.git"|"$root_abs/.git"/*) reject_output "repository metadata" ;;
    "$git_dir_abs"|"$git_dir_abs"/*) reject_output "worktree git directory" ;;
  esac
  if [ -d "$out_abs" ]; then
    reject_output "directory"
  fi
  case "$out_abs" in
    "$root_abs"|"$root_abs"/*)
      relative_out=${out_abs#"$root_abs"/}
      if [ -e "$out_abs" ] || git -C "$root" ls-files --error-unmatch -- "$relative_out" >/dev/null 2>&1; then
        reject_output "tracked or existing repository file"
      fi
      ;;
  esac

  lock_root=${REMEDIATION_EVAL_LOCK_DIR:-${TMPDIR:-/tmp}/remediation-eval-locks}
  secure_lock_root "$lock_root"
  lock_key=$(printf '%s' "$out_abs" | cksum | awk '{print $1}')
  if command -v flock >/dev/null 2>&1; then
    lock_file="$lock_root/$lock_key.lock"
    if [ -L "$lock_file" ]; then
      echo "refusing symlink lock file: $lock_file" >&2
      exit 2
    fi
    if [ ! -e "$lock_file" ]; then
      (set -o noclobber; : > "$lock_file") 2>/dev/null || true
    fi
    if [ -L "$lock_file" ]; then
      echo "refusing symlink lock file: $lock_file" >&2
      exit 2
    fi
    exec {lock_fd}<>"$lock_file"
    if flock --help 2>&1 | grep -q -- '-w'; then
      flock -w "$lock_timeout" "$lock_fd" || { echo "timed out acquiring output lock" >&2; exit 2; }
    else
      watchdog=0
      (sleep "$lock_timeout"; kill -TERM $$ 2>/dev/null) & watchdog=$!
      flock -x "$lock_fd" || { kill "$watchdog" 2>/dev/null || true; echo "timed out acquiring output lock" >&2; exit 2; }
      kill "$watchdog" 2>/dev/null || true
    fi
  else
    lock_dir="$lock_root/$lock_key.lock.d"
    owner_file="$lock_dir/owner"
    max_tries=$((lock_timeout * 20))
    tries=0
    until mkdir "$lock_dir" 2>/dev/null; do
      tries=$((tries + 1))
      reclaim=0
      if [ -f "$owner_file" ]; then
        owner_pid=$(cat "$owner_file" 2>/dev/null || true)
        if [ -z "$owner_pid" ] || ! kill -0 "$owner_pid" 2>/dev/null; then
          reclaim=1
        fi
      else
        # A crash between mkdir and owner write leaves no pid; reclaim only
        # after the lock's own timeout has elapsed.
        modified=$(stat -c %Y "$lock_dir" 2>/dev/null || stat -f %m "$lock_dir" 2>/dev/null || printf 0)
        now=$(date +%s)
        if [ "$((now - modified))" -gt "$lock_timeout" ]; then
          reclaim=1
        fi
      fi
      if [ "$reclaim" = 1 ]; then
        rm -f -- "$owner_file" 2>/dev/null || true
        rmdir "$lock_dir" 2>/dev/null || true
      fi
      if [ "$tries" -gt "$max_tries" ]; then
        echo "timed out acquiring output lock: $lock_dir" >&2
        exit 2
      fi
      sleep 0.05
    done
    printf '%s\n' "$$" > "$owner_file"
    lock_owned=1
  fi

  if [ -e "$out_abs" ]; then
    rm -f -- "$out_abs"
  fi
  printf 'run_id=%s\n' "$run_id"
fi

if [ "${EVAL_ALLOW_DIRTY:-0}" != "1" ]; then
  if ! git -C "$root" diff --quiet || ! git -C "$root" diff --cached --quiet; then
    echo "refusing to run on a tracked dirty tree (set EVAL_ALLOW_DIRTY=1 for local TDD)" >&2
    exit 3
  fi
fi

entry=$(jq -c --arg s "$suite" '.suites[$s] // empty' "$reg_snapshot")
[ -n "$entry" ] || { echo "unknown suite: $suite" >&2; exit 2; }

commands=()
while IFS= read -r line; do commands+=("$line"); done < <(jq -r --arg s "$suite" '.suites[$s].commands[]' "$reg_snapshot")
expected=()
while IFS= read -r line; do expected+=("$line"); done < <(jq -r --arg s "$suite" '.suites[$s].tests[]' "$reg_snapshot")
[ "${#expected[@]}" -gt 0 ] || { echo "suite has no expected tests: $suite" >&2; exit 2; }

log=$(mktemp)
passed=0
failed=0
ignored=0
export TRACKED_UPGRADES_COMMIT="$commit"
for cmd in "${commands[@]}"; do
  set +e
  output=$(cd "$root" && bash -c "$cmd" 2>&1)
  status=$?
  set -e
  printf '%s\n' "$output" >> "$log"
  summary=$(grep -E '^test result:' <<<"$output" | tail -1 || true)
  if [ -z "$summary" ]; then
    echo "no test summary for: $cmd" >&2
    echo "$output" >&2
    exit 1
  fi
  p=$(sed -nE 's/.* ([0-9]+) passed.*/\1/p' <<<"$summary" | tail -1)
  f=$(sed -nE 's/.* ([0-9]+) failed.*/\1/p' <<<"$summary" | tail -1)
  i=$(sed -nE 's/.* ([0-9]+) ignored.*/\1/p' <<<"$summary" | tail -1)
  passed=$((passed + ${p:-0}))
  failed=$((failed + ${f:-0}))
  ignored=$((ignored + ${i:-0}))
  if [ "$status" -ne 0 ]; then
    echo "command failed: $cmd" >&2
    exit 1
  fi
done

# Every command and the gate must have observed the same committed revision.
final_commit=$(git -C "$root" rev-parse HEAD)
if [ "$final_commit" != "$commit" ]; then
  echo "HEAD moved during the run ($commit -> $final_commit); verdict discarded" >&2
  exit 1
fi

for name in "${expected[@]}"; do
  if ! grep -Fq "test $name ... ok" "$log"; then
    echo "expected test did not pass: $name" >&2
    exit 1
  fi
done

threshold_ignored=$(jq -r --arg s "$suite" '.suites[$s].thresholds.ignored // 0' "$reg_snapshot")
if [ "$ignored" -ne "$threshold_ignored" ]; then
  echo "ignored count $ignored != required $threshold_ignored" >&2
  exit 1
fi
if [ "$failed" -ne 0 ]; then
  echo "failed count $failed != 0" >&2
  exit 1
fi

result=$(jq -n \
  --arg suite "$suite" \
  --arg commit "$commit" \
  --arg run_id "$run_id" \
  --argjson passed "$passed" \
  --argjson failed "$failed" \
  --argjson ignored "$ignored" \
  --argjson expected "$(printf '%s\n' "${expected[@]}" | jq -R . | jq -s .)" \
  --argjson thresholds "$(jq -c --arg s "$suite" '.suites[$s].thresholds' "$reg_snapshot")" \
  '{suite:$suite, commit:$commit, run_id:$run_id, passed:$passed, failed:$failed, ignored:$ignored, expected:$expected, thresholds:$thresholds, verdict:(if $failed==0 and $ignored==0 then "PASS" else "FAIL" end)}')

if [ -n "$out" ]; then
  mkdir -p "$(dirname "$out_abs")"
  tmp_out=$(mktemp "$(dirname "$out_abs")/.$(basename "$out_abs").XXXXXX")
  printf '%s\n' "$result" > "$tmp_out"
  mv -f -- "$tmp_out" "$out_abs"
  tmp_out=""
  printf '%s\n' "$result"
  echo "wrote $out" >&2
else
  printf '%s\n' "$result"
fi
