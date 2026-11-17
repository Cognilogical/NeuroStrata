#!/usr/bin/env bash
# Validate or apply the repository main merge-gate ruleset (#227).
#
# Modes:
#   scripts/enforce-main-ruleset.sh [--spec-check]
#     Validate the checked-in scripts/main-ruleset.json specification.
#   scripts/enforce-main-ruleset.sh --fixture <path>
#     Validate an arbitrary ruleset JSON document against all #227 invariants.
#   scripts/enforce-main-ruleset.sh --live-check [--repo owner/name] [--ruleset-id id]
#     Fetch the active repository ruleset via `gh api` and validate it.
#   scripts/enforce-main-ruleset.sh --apply [--repo owner/name] [--ruleset-id id]
#     PUT scripts/main-ruleset.json to the repository ruleset and verify the
#     returned live configuration.

set -euo pipefail

ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
SPEC_FILE="${MAIN_RULESET_SPEC:-$ROOT/scripts/main-ruleset.json}"
REPO="${MAIN_RULESET_REPO:-Cognilogical/NeuroStrata}"
RULESET_ID="${MAIN_RULESET_ID:-16846790}"
MODE="spec-check"
FIXTURE_PATH=""

while [ $# -gt 0 ]; do
  case "$1" in
    --spec-check)
      MODE="spec-check"
      shift
      ;;
    --fixture)
      [ $# -ge 2 ] || { echo "error: --fixture requires a path" >&2; exit 2; }
      MODE="fixture"
      FIXTURE_PATH="$2"
      shift 2
      ;;
    --live-check)
      MODE="live-check"
      shift
      ;;
    --apply)
      MODE="apply"
      shift
      ;;
    --repo)
      [ $# -ge 2 ] || { echo "error: --repo requires owner/name" >&2; exit 2; }
      REPO="$2"
      shift 2
      ;;
    --ruleset-id)
      [ $# -ge 2 ] || { echo "error: --ruleset-id requires an id" >&2; exit 2; }
      RULESET_ID="$2"
      shift 2
      ;;
    *)
      echo "error: unknown argument '$1'" >&2
      exit 2
      ;;
  esac
done

validate_ruleset_file() {
  local target_file="$1"
  if [ ! -f "$target_file" ]; then
    echo "error: ruleset file '$target_file' not found" >&2
    return 1
  fi
  if ! jq empty "$target_file" >/dev/null 2>&1; then
    echo "error: '$target_file' is not valid JSON" >&2
    return 1
  fi

  local failures=()

  if [ "$(jq -r '.enforcement // empty' "$target_file")" != "active" ]; then
    failures+=("enforcement must be 'active'")
  fi

  if ! jq -e '.conditions.ref_name.include | index("~DEFAULT_BRANCH") != null or index("refs/heads/main") != null' "$target_file" >/dev/null 2>&1; then
    failures+=("conditions.ref_name.include must cover ~DEFAULT_BRANCH or refs/heads/main")
  fi

  if ! jq -e '.rules | map(.type) | index("deletion") != null' "$target_file" >/dev/null 2>&1; then
    failures+=("rules must include 'deletion'")
  fi

  if ! jq -e '.rules | map(.type) | index("non_fast_forward") != null' "$target_file" >/dev/null 2>&1; then
    failures+=("rules must include 'non_fast_forward'")
  fi

  if ! jq -e '.rules[] | select(.type == "pull_request") | .parameters.required_review_thread_resolution == true' "$target_file" >/dev/null 2>&1; then
    failures+=("pull_request.parameters.required_review_thread_resolution must be true")
  fi

  if ! jq -e '.rules[] | select(.type == "pull_request") | .parameters.require_last_push_approval == true' "$target_file" >/dev/null 2>&1; then
    failures+=("pull_request.parameters.require_last_push_approval must be true")
  fi

  for required_ctx in "build (ubuntu-latest)" "test (ubuntu)" "gate-receipt"; do
    if ! jq -e --arg ctx "$required_ctx" '
      [.rules[] | select(.type == "required_status_checks") | .parameters.required_status_checks[]?.context]
      | index($ctx) != null
    ' "$target_file" >/dev/null 2>&1; then
      failures+=("required_status_checks must require '$required_ctx'")
    fi
  done

  if ! jq -e '
    (.bypass_actors | type == "array") and
    (.bypass_actors | length == 1) and
    (.bypass_actors[0].actor_type == "OrganizationAdmin")
  ' "$target_file" >/dev/null 2>&1; then
    failures+=("bypass_actors must contain only the single OrganizationAdmin break-glass identity")
  fi

  if [ "${#failures[@]}" -gt 0 ]; then
    for msg in "${failures[@]}"; do
      echo "ruleset-check failure: $msg" >&2
    done
    return 1
  fi

  echo "ruleset-check ok: $target_file"
  return 0
}

case "$MODE" in
  spec-check)
    validate_ruleset_file "$SPEC_FILE"
    ;;
  fixture)
    validate_ruleset_file "$FIXTURE_PATH"
    ;;
  live-check)
    tmp="$(mktemp)"
    trap 'rm -f "$tmp"' EXIT
    gh api "repos/${REPO}/rulesets/${RULESET_ID}" >"$tmp"
    validate_ruleset_file "$tmp"
    ;;
  apply)
    validate_ruleset_file "$SPEC_FILE"
    tmp="$(mktemp)"
    trap 'rm -f "$tmp"' EXIT
    gh api -X PUT "repos/${REPO}/rulesets/${RULESET_ID}" --input "$SPEC_FILE" >"$tmp"
    validate_ruleset_file "$tmp"
    ;;
esac
