#!/usr/bin/env bash
# Self-test for scripts/check-release-references.sh (#216).
#
# The reference check is only worth having if it actually rejects references
# that do not exist. These cases pin the four registries it resolves against:
#
#   MCP tools        src/server.rs + src/synaptic_mesh/tools.rs
#   daemon routes    the production router block in src/daemon.rs
#   CLI subcommands  src/main.rs (explicit names and their aliases)
#   tree paths       the working tree, including root-level manifests
#
# Every case runs against a temporary copy of the tree, so a negative case
# can never mutate the real source.

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if REPO="$(git -C "$HERE" rev-parse --show-toplevel 2>/dev/null)"; then
  :
else
  REPO="$(cd "$HERE/.." && pwd)"
fi
CHECK="$HERE/check-release-references.sh"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/release-reference-selftest.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

passed=0
failed=0

ok() {
  passed=$((passed + 1))
  printf 'ok   %s\n' "$1"
}

bad() {
  failed=$((failed + 1))
  printf 'FAIL %s\n' "$1"
  if [ -n "${2:-}" ]; then
    printf '%s\n' "$2" | sed 's/^/     /'
  fi
}

# make_tree <dir> -- a throwaway copy of the registries the check reads.
make_tree() {
  local dest="$1"
  mkdir -p "$dest/src/synaptic_mesh" "$dest/docs/releases/changelog"
  cp "$REPO/src/server.rs" "$dest/src/server.rs"
  cp "$REPO/src/daemon.rs" "$dest/src/daemon.rs"
  cp "$REPO/src/main.rs" "$dest/src/main.rs"
  cp "$REPO/src/synaptic_mesh/tools.rs" "$dest/src/synaptic_mesh/tools.rs"
  cp "$REPO/Cargo.toml" "$dest/Cargo.toml"
  cp "$REPO/Cargo.lock" "$dest/Cargo.lock"
}

# write_narrative <dir> <line> -- a single-bullet release section.
write_narrative() {
  local dest="$1" line="$2"
  cat >"$dest/CHANGELOG.md" <<EOF
# Fixture changelog

## [1.18.0] - 2026-11-15

### Added

- $line
EOF
  : >"$dest/docs/releases/changelog/1.18.0.md"
}

# check <dir> -- run the check against a tree root, print its output.
check() {
  RELEASE_REFERENCE_ROOT="$1" bash "$CHECK" 2>&1
}

# expect_exit <name> <dir> <expected-exit> [expected-substring]
expect_exit() {
  local name="$1" dir="$2" want="$3" needle="${4:-}" out rc
  out="$(check "$dir")"
  rc=$?
  if [ "$rc" -ne "$want" ]; then
    bad "$name" "expected exit $want, got $rc
$out"
    return
  fi
  if [ -n "$needle" ] && ! printf '%s\n' "$out" | grep -qF -- "$needle"; then
    bad "$name" "output did not mention: $needle
$out"
    return
  fi
  ok "$name"
}

# --- 1. a valid narrative resolves ------------------------------------------

# The fixture narratives below must carry literal backtick-quoted names, so
# they stay single-quoted on purpose: nothing here is ever expanded. Each site
# carries its own targeted disable so the harness code stays checked.

TREE="$WORK/valid"
make_tree "$TREE"
# shellcheck disable=SC2016  # literal fixture: backtick names never expand
write_narrative "$TREE" 'Fixture names `neurostrata_add_memory`, `neurostrata_synaptic_mesh_init`, `POST /diff-tour`, `neurostrata-mcp synaptic-mesh`, `neurostrata-mcp mesh`, `Cargo.toml`, `Cargo.lock` and `src/server.rs`.'
expect_exit "valid narrative resolves" "$TREE" 0 "every reference"

# --- 2. MCP tool references -------------------------------------------------

TREE="$WORK/tool"
make_tree "$TREE"
# shellcheck disable=SC2016  # literal fixture: backtick names never expand
write_narrative "$TREE" 'Fixture names `neurostrata_add_memory`.'
expect_exit "known MCP tool resolves" "$TREE" 0 "every reference"

TREE="$WORK/tool-bad"
make_tree "$TREE"
# shellcheck disable=SC2016  # literal fixture: backtick names never expand
write_narrative "$TREE" 'Fixture names `neurostrata_definitely_not_a_tool`.'
expect_exit "unknown MCP tool fails" "$TREE" 1 "unresolved MCP tool reference: neurostrata_definitely_not_a_tool"

TREE="$WORK/tool-mesh"
make_tree "$TREE"
# shellcheck disable=SC2016  # literal fixture: backtick names never expand
write_narrative "$TREE" 'Fixture names `neurostrata_synaptic_mesh_init`.'
expect_exit "runtime mesh tool resolves" "$TREE" 0 "every reference"

TREE="$WORK/tool-mesh-bad"
make_tree "$TREE"
# shellcheck disable=SC2016  # literal fixture: backtick names never expand
write_narrative "$TREE" 'Fixture names `neurostrata_synaptic_mesh_not_a_tool`.'
expect_exit "unknown mesh tool fails" "$TREE" 1 "unresolved MCP tool reference: neurostrata_synaptic_mesh_not_a_tool"

# --- 3. daemon route references ---------------------------------------------

TREE="$WORK/route"
make_tree "$TREE"
# shellcheck disable=SC2016  # literal fixture: backtick names never expand
write_narrative "$TREE" 'Fixture names `POST /diff-tour`.'
expect_exit "shipped route resolves" "$TREE" 0 "every reference"

TREE="$WORK/route-testonly"
make_tree "$TREE"
# shellcheck disable=SC2016  # literal fixture: backtick names never expand
write_narrative "$TREE" 'Fixture names `POST /dummy`.'
expect_exit "test-only route fails" "$TREE" 1 "unresolved daemon route reference: POST /dummy"

TREE="$WORK/route-bad"
make_tree "$TREE"
# shellcheck disable=SC2016  # literal fixture: backtick names never expand
write_narrative "$TREE" 'Fixture names `POST /not-a-shipped-route`.'
expect_exit "unknown route fails" "$TREE" 1 "unresolved daemon route reference: POST /not-a-shipped-route"

# --- 4. CLI subcommand references -------------------------------------------

TREE="$WORK/cli"
make_tree "$TREE"
# shellcheck disable=SC2016  # literal fixture: backtick names never expand
write_narrative "$TREE" 'Fixture names `neurostrata-mcp synaptic-mesh`.'
expect_exit "command with trailing attributes resolves" "$TREE" 0 "every reference"

TREE="$WORK/cli-alias"
make_tree "$TREE"
# shellcheck disable=SC2016  # literal fixture: backtick names never expand
write_narrative "$TREE" 'Fixture names `neurostrata-mcp mesh`.'
expect_exit "command alias resolves" "$TREE" 0 "every reference"

TREE="$WORK/cli-bad"
make_tree "$TREE"
# shellcheck disable=SC2016  # literal fixture: backtick names never expand
write_narrative "$TREE" 'Fixture names `neurostrata-mcp definitely-not-a-command`.'
expect_exit "unknown CLI command fails" "$TREE" 1 "unresolved CLI command reference: neurostrata-mcp definitely-not-a-command"

# --- 5. path references -----------------------------------------------------

TREE="$WORK/path"
make_tree "$TREE"
# shellcheck disable=SC2016  # literal fixture: backtick names never expand
write_narrative "$TREE" 'Fixture names `src/server.rs` and `Cargo.toml`.'
expect_exit "tree paths resolve" "$TREE" 0 "every reference"

TREE="$WORK/path-bad"
make_tree "$TREE"
# shellcheck disable=SC2016  # literal fixture: backtick names never expand
write_narrative "$TREE" 'Fixture names `src/parser/not_a_real_file.rs`.'
expect_exit "missing tree path fails" "$TREE" 1 "unresolved path reference: src/parser/not_a_real_file.rs"

TREE="$WORK/manifest-missing"
make_tree "$TREE"
rm -f "$TREE/Cargo.toml"
# shellcheck disable=SC2016  # literal fixture: backtick names never expand
write_narrative "$TREE" 'Fixture names `Cargo.toml`.'
expect_exit "missing root manifest fails" "$TREE" 1 "unresolved path reference: Cargo.toml"

TREE="$WORK/manifest-renamed"
make_tree "$TREE"
mv "$TREE/Cargo.toml" "$TREE/Cargo.toml.renamed"
# shellcheck disable=SC2016  # literal fixture: backtick names never expand
write_narrative "$TREE" 'Fixture names `Cargo.toml`.'
expect_exit "renamed root manifest fails" "$TREE" 1 "unresolved path reference: Cargo.toml"

TREE="$WORK/manifest-lock-missing"
make_tree "$TREE"
rm -f "$TREE/Cargo.lock"
# shellcheck disable=SC2016  # literal fixture: backtick names never expand
write_narrative "$TREE" 'Fixture names `Cargo.lock`.'
expect_exit "missing root lockfile fails" "$TREE" 1 "unresolved path reference: Cargo.lock"

# --- 6. the shipped narrative ------------------------------------------------

expect_exit "shipped 1.18.0 narrative resolves" "$REPO" 0 "every reference"

if grep -qF 'set-thesis|upsert-concept|tree|briefing|compare|gaps|advise' "$REPO/CHANGELOG.md"; then
  ok "changelog taxonomy synopsis lists tree"
else
  bad "changelog taxonomy synopsis lists tree" "the 1.18.0 synopsis does not name the shipped \`tree\` operation"
fi

# shellcheck disable=SC2016  # literal fixture: backtick names never expand
if grep -qF '(`tree`)' "$REPO/docs/releases/changelog/1.18.0.md"; then
  ok "release note names the tree operation"
else
  bad "release note names the tree operation" "docs/releases/changelog/1.18.0.md does not name \`tree\`"
fi

# --- 7. the pre-correction narrative still fails ----------------------------

TREE="$WORK/legacy"
make_tree "$TREE"
cat >"$TREE/CHANGELOG.md" <<'EOF'
# Fixture changelog

## [1.18.0] - 2026-11-15

### Added

- Legacy names `neurostrata_diff_review`, `POST /diff-review`, `neurostrata-mcp diff-review`, `neurostrata_twin`, `POST /twin`, `neurostrata-mcp twin` and `neurostrata_code_insights`.
EOF
: >"$TREE/docs/releases/changelog/1.18.0.md"
LEGACY_OUT="$(check "$TREE")"
LEGACY_RC=$?
if [ "$LEGACY_RC" -ne 1 ]; then
  bad "pre-correction narrative fails" "expected exit 1, got $LEGACY_RC
$LEGACY_OUT"
else
  ok "pre-correction narrative fails"
fi
for ref in neurostrata_diff_review "POST /diff-review" "neurostrata-mcp diff-review" neurostrata_twin "POST /twin" "neurostrata-mcp twin" neurostrata_code_insights; do
  if printf '%s\n' "$LEGACY_OUT" | grep -qF -- "$ref"; then
    ok "pre-correction narrative reports $ref"
  else
    bad "pre-correction narrative reports $ref" "$LEGACY_OUT"
  fi
done

printf '\nrelease reference self-test: %d passed, %d failed\n' "$passed" "$failed"
[ "$failed" -eq 0 ]
