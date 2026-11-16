#!/usr/bin/env bash
# Release-narrative reference validation (#216).
#
# The release narrative names MCP tools, daemon routes, CLI subcommands and
# in-tree files. A name that never shipped is a user-facing failure:
# `neurostrata-mcp diff-review` did not exist and `POST /diff-review` was a
# 404, while the shipped commands were `neurostrata-mcp diff-tour` and
# `POST /diff-tour`. This check resolves every such reference against the
# source tree so the narrative cannot drift from the code again.
#
# Scope: the CURRENT release narrative only -- the top-most release section of
# CHANGELOG.md plus the matching docs/releases/changelog/<version>.md. Older
# sections are shipped history whose references were reorganized under
# docs/archive/ afterwards; rewriting that history is out of scope for a
# documentation check, and the rest of the docs tree is never scanned.
#
# Registries are read from the tree itself:
#   MCP tools        src/server.rs     ("name": "<tool>" registrations)
#   daemon routes    src/daemon.rs     (.route("/<path>"))
#   CLI subcommands  src/main.rs       (#[command(name = "<cmd>")])
#   tree paths       the working tree
#
# Exit status: 0 when every reference resolves, 1 with a report otherwise.

set -uo pipefail

ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "$ROOT" || exit 2

TOOL_PATTERN='neurostrata_[a-z0-9_]+'
ROUTE_PATTERN='(POST|GET) /[a-z0-9][a-z0-9/_-]*'
CLI_PATTERN='neurostrata-mcp [a-z][a-z0-9-]*'
PATH_PATTERN='(src|proofs|docs|tests|scripts|plugins|eval|web-ui|src-tauri|\.github)/[A-Za-z0-9._/-]+'

# --- the current release narrative ------------------------------------------

VERSION="$(sed -n 's/^## \[\([^]]*\)\].*/\1/p' CHANGELOG.md 2>/dev/null | head -1)"
if [ -z "$VERSION" ]; then
  echo "::error::no release section found in CHANGELOG.md" >&2
  exit 2
fi

# Line bounds of the top-most release section of CHANGELOG.md.
bounds="$(awk '
  /^## \[/ { count++; if (count == 1) { start = NR } else { print start, NR - 1; done = 1; exit } }
  END { if (!done && start) print start, NR }
' CHANGELOG.md)"
read -r SECTION_START SECTION_END <<<"$bounds"

NOTES="docs/releases/changelog/${VERSION}.md"

echo "release reference check: release ${VERSION} (CHANGELOG.md lines ${SECTION_START}-${SECTION_END}, ${NOTES})"

# --- registries ---------------------------------------------------------------

TOOLS="$(
  {
    grep -rhoE '"name": "(neurostrata_[a-z0-9_]+)"' src/server.rs 2>/dev/null |
      sed -E 's/.*"(neurostrata_[a-z0-9_]+)".*/\1/'
    grep -rhoE '"(neurostrata_[a-z0-9_]+)" =>' src/server.rs 2>/dev/null |
      sed -E 's/"(neurostrata_[a-z0-9_]+)".*/\1/'
  } | LC_ALL=C sort -u
)"

ROUTES="$(
  grep -rhoE '\.route\("/[a-z0-9/_-]+"' src/daemon.rs 2>/dev/null |
    sed -E 's|^\.route\("/||; s|"$||' | LC_ALL=C sort -u
)"

CLIS="$(
  grep -rhoE '#\[command\(name = "[a-z0-9-]+"\)\]' src/main.rs 2>/dev/null |
    sed -E 's/.*name = "//; s/".*//' | LC_ALL=C sort -u
)"

if [ -z "$TOOLS" ] || [ -z "$ROUTES" ] || [ -z "$CLIS" ]; then
  echo "::error::release reference check could not read the source registries" >&2
  exit 2
fi

failures=0

report() {
  # report <doc> <line> <kind> <reference> <hint>
  printf '%s:%s: unresolved %s reference: %s (%s)\n' "$1" "$2" "$3" "$4" "$5" >&2
  failures=$((failures + 1))
}

in_list() {
  # in_list <needle> <newline-separated haystack>
  printf '%s\n' "$2" | grep -qxF -- "$1"
}

check_line() {
  # check_line <doc> <lineno> <line>
  local doc="$1" lineno="$2" line="$3" ref path cmd

  while IFS= read -r ref; do
    [ -n "$ref" ] || continue
    if ! in_list "$ref" "$TOOLS"; then
      report "$doc" "$lineno" "MCP tool" "$ref" "not registered in src/server.rs"
    fi
  done <<<"$(printf '%s\n' "$line" | grep -oE "$TOOL_PATTERN" | LC_ALL=C sort -u)"

  while IFS= read -r ref; do
    [ -n "$ref" ] || continue
    path="${ref#* /}"
    if ! in_list "$path" "$ROUTES"; then
      report "$doc" "$lineno" "daemon route" "$ref" "not routed in src/daemon.rs"
    fi
  done <<<"$(printf '%s\n' "$line" | grep -oE "$ROUTE_PATTERN" | LC_ALL=C sort -u)"

  while IFS= read -r ref; do
    [ -n "$ref" ] || continue
    cmd="${ref#neurostrata-mcp }"
    if ! in_list "$cmd" "$CLIS"; then
      report "$doc" "$lineno" "CLI command" "neurostrata-mcp $cmd" "not a subcommand in src/main.rs"
    fi
  done <<<"$(printf '%s\n' "$line" | grep -oE "$CLI_PATTERN" | LC_ALL=C sort -u)"

  while IFS= read -r ref; do
    [ -n "$ref" ] || continue
    # Trim trailing prose punctuation; a trailing slash marks a directory
    # reference and is kept.
    path="$ref"
    while [ "${path: -1}" = "." ]; do
      path="${path%?}"
    done
    [ -n "$path" ] || continue
    if [ ! -e "$path" ]; then
      report "$doc" "$lineno" "path" "$ref" "not present in the tree"
    fi
  done <<<"$(printf '%s\n' "$line" | grep -oE "$PATH_PATTERN" | LC_ALL=C sort -u)"
}

# --- the CHANGELOG section ---------------------------------------------------

lineno=0
while IFS= read -r line; do
  lineno=$((lineno + 1))
  if [ "$lineno" -ge "$SECTION_START" ] && [ "$lineno" -le "$SECTION_END" ]; then
    check_line "CHANGELOG.md" "$lineno" "$line"
  fi
done <CHANGELOG.md

# --- the matching release note ----------------------------------------------

if [ -f "$NOTES" ]; then
  lineno=0
  while IFS= read -r line; do
    lineno=$((lineno + 1))
    check_line "$NOTES" "$lineno" "$line"
  done <"$NOTES"
else
  echo "::warning::no release note at ${NOTES}; only the CHANGELOG section was checked" >&2
fi

if [ "$failures" -gt 0 ]; then
  echo "::error::release reference check failed: $failures unresolved reference(s) in the ${VERSION} release narrative" >&2
  exit 1
fi

echo "release reference check: every reference in the ${VERSION} release narrative resolves"
exit 0
