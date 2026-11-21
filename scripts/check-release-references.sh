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
#   MCP tools        src/server.rs (the tools/list schemas and the dispatch
#                    arms) plus src/synaptic_mesh/tools.rs (the SynapticMesh
#                    definitions the server appends to tools/list at
#                    src/server.rs:1277)
#   daemon routes    the production router in src/daemon.rs -- the block that
#                    starts at `let app = Router::new()` and ends at
#                    `.with_state(..)`. Routes that only exist inside a
#                    `#[cfg(test)]` module (for example the /dummy probe) are
#                    never served and must not satisfy a narrative reference.
#   CLI subcommands  the top-level `Commands` enum in src/main.rs: explicit
#                    `#[command(name = "..")]` values, their `alias = ".."`
#                    values, and the clap-derived kebab-case name of every
#                    variant that carries no explicit name
#   tree paths       the working tree, including root-level manifests
#
# RELEASE_REFERENCE_ROOT overrides the tree root so the self-test can run the
# check against a fixture copy without touching the real source.
#
# Exit status: 0 when every reference resolves, 1 with a report otherwise.

set -uo pipefail

ROOT="${RELEASE_REFERENCE_ROOT:-}"
if [ -z "$ROOT" ]; then
  ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
fi
cd "$ROOT" || exit 2

TOOL_PATTERN='neurostrata_[a-z0-9_]+'
ROUTE_PATTERN='(POST|GET) /[a-z0-9][a-z0-9/_-]*'
CLI_PATTERN='neurostrata-mcp [a-z][a-z0-9-]*'
# A token names a tree path only when it starts with one of these prefixes.
PATH_DIR_PATTERN='^(src|proofs|docs|tests|scripts|plugins|eval|web-ui|src-tauri|\.github)/'
# Root-level manifests the narrative may name; each is checked at the root.
ROOT_MANIFESTS='Cargo.toml Cargo.lock'

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
# check_line reports this path; the loop below feeds the same file on stdin,
# and passing that path as a command argument makes linters read the loop as a
# self-overwriting pipeline (SC2094) even though nothing writes it.
NOTES_LABEL="$NOTES"

echo "release reference check: release ${VERSION} (CHANGELOG.md lines ${SECTION_START}-${SECTION_END}, ${NOTES})"

# --- registries ---------------------------------------------------------------

# MCP tool names. The SynapticMesh definitions live with the module and are
# appended to the tools/list payload at runtime, so they ship too.
TOOL_SOURCES="src/server.rs src/synaptic_mesh/tools.rs"

TOOLS="$(
  {
    for src in $TOOL_SOURCES; do
      [ -f "$src" ] || continue
      grep -hoE '"name": "(neurostrata_[a-z0-9_]+)"' "$src" 2>/dev/null |
        sed -E 's/.*"(neurostrata_[a-z0-9_]+)".*/\1/'
      grep -hoE '"(neurostrata_[a-z0-9_]+)" =>' "$src" 2>/dev/null |
        sed -E 's/.*"(neurostrata_[a-z0-9_]+)".*/\1/'
    done
  } | LC_ALL=C sort -u
)"

# Daemon routes: the production router only. The first `let app = Router::new()`
# is the served router; a later one belongs to a test module.
ROUTES="$(
  awk '
    /let app = Router::new\(\)/ && !seen { seen = 1; inblock = 1 }
    inblock { print }
    inblock && /with_state\(/ { exit }
  ' src/daemon.rs 2>/dev/null |
    tr '\n' ' ' |
    grep -oE '\.route\( *"(/[a-z0-9/_-]+)"' |
    sed -E 's|.*"(/[a-z0-9/_-]+)".*|\1|' |
    LC_ALL=C sort -u
)"

# CLI subcommands: the top-level `Commands` enum. Clap derives a kebab-case
# name for a variant that carries no explicit `name = ".."` attribute, so the
# derived names are part of the registry too.
# shellcheck disable=SC2016  # awk program: $0/$1 are awk fields, not shell expansions
CLI_AWK='
function kebab(s,   out, i, c, p) {
  out = ""; p = ""
  for (i = 1; i <= length(s); i++) {
    c = substr(s, i, 1)
    if (c ~ /[A-Z]/ && p ~ /[a-z0-9]/) out = out "-"
    out = out tolower(c); p = c
  }
  return out
}
/^enum Commands \{/ { inblock = 1; next }
inblock && /^\}/ { inblock = 0; next }
!inblock { next }
{
  if (index($0, "name = \"") > 0) {
    s = $0
    sub(/^.*name = "/, "", s); sub(/".*$/, "", s)
    pname = s
  }
  if (index($0, "alias = \"") > 0) {
    s = $0
    sub(/^.*alias = "/, "", s); sub(/".*$/, "", s)
    palias = s
  }
  if ($0 ~ /^    [A-Z][A-Za-z0-9]*[ ,{]/) {
    v = $1
    sub(/[ ,{].*$/, "", v)
    print (pname != "" ? pname : kebab(v))
    if (palias != "") print palias
    pname = ""; palias = ""
  }
}
'

CLIS="$(awk "$CLI_AWK" src/main.rs 2>/dev/null | LC_ALL=C sort -u)"

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

# path_tokens <line> -- the candidate tree paths a narrative line names.
# Everything that is not a path character separates tokens, so trailing prose
# punctuation never leaks into a reference.
path_tokens() {
  printf '%s\n' "$1" | tr -c 'A-Za-z0-9._/-' '\n' | sed '/^$/d' | LC_ALL=C sort -u
}

check_line() {
  # check_line <doc> <lineno> <line>
  local doc="$1" lineno="$2" line="$3" ref path cmd manifest

  while IFS= read -r ref; do
    [ -n "$ref" ] || continue
    if ! in_list "$ref" "$TOOLS"; then
      report "$doc" "$lineno" "MCP tool" "$ref" "not registered in src/server.rs"
    fi
  done <<<"$(printf '%s\n' "$line" | grep -oE "$TOOL_PATTERN" | LC_ALL=C sort -u)"

  while IFS= read -r ref; do
    [ -n "$ref" ] || continue
    path="${ref#* }"
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
    if printf '%s' "$path" | grep -qE "$PATH_DIR_PATTERN"; then
      if [ ! -e "$path" ]; then
        report "$doc" "$lineno" "path" "$ref" "not present in the tree"
      fi
    else
      for manifest in $ROOT_MANIFESTS; do
        if [ "$path" = "$manifest" ] && [ ! -f "$path" ]; then
          report "$doc" "$lineno" "path" "$ref" "not present at the tree root"
        fi
      done
    fi
  done <<<"$(path_tokens "$line")"
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
    check_line "$NOTES_LABEL" "$lineno" "$line"
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
