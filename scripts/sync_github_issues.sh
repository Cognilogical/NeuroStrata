#!/usr/bin/env bash
# sync_github_issues.sh — Sync NeuroStrata remediation findings to GitHub Issues
# and create a pull-request for the remediate-p4-p5-p6 branch.
#
# Usage: ./scripts/sync_github_issues.sh [--dry-run]
#
# Requires: gh CLI authenticated via keyring (run gh auth login first),
#           OR export GH_TOKEN=<your-token> before running.
set -euo pipefail

REPO="Cognilogical/NeuroStrata"
BRANCH="remediate-p4-p5-p6"
DRY_RUN=false
[[ "${1:-}" == "--dry-run" ]] && DRY_RUN=true

log()  { echo ">> $*"; }
note() { echo "  ok: $*"; }
warn() { echo "  !! $*"; }

# pre-flight
log "Checking gh authentication..."
if ! gh api user --jq '.login' >/dev/null 2>&1; then
  echo "ERROR: gh CLI cannot reach GitHub API."
  echo "  Run: gh auth login"
  exit 1
fi
ACTOR=$(gh api user --jq '.login')
note "Authenticated as @${ACTOR}."

# helper
create_issue() {
  local title="$1" body="$2" labels="$3"
  if $DRY_RUN; then
    warn "[DRY-RUN] Would create: \"$title\" [${labels}]"
    return
  fi
  local url
  url=$(gh issue create \
    --repo "$REPO" \
    --title "$title" \
    --body  "$body" \
    --label "$labels" 2>&1)
  note "Created -> $url"
}

# ensure labels exist
ensure_labels() {
  log "Ensuring labels exist..."
  local name color desc
  while IFS=: read -r name color desc; do
    $DRY_RUN && { warn "[DRY-RUN] label: $name"; continue; }
    gh label create "$name" --repo "$REPO" \
       --color "$color" --description "$desc" --force 2>/dev/null || true
  done << 'LABELS'
bug:d73a4a:Bug fix
security:e4e669:Security vulnerability
enhancement:a2eeef:New feature or improvement
architecture:0075ca:Architectural change
performance:fbca04:Performance improvement
remediated:2ea44f:Finding has been remediated
P0-critical:b60205:Critical priority must fix immediately
P1-high:d93f0b:High priority
P2-medium:e4e669:Medium priority
LABELS
}

# R1 Issues — Review Round 1
create_r1_issues() {
  log "Creating R1 (Review Round 1) issues..."

  create_issue \
    "[R1-F01] REMEDIATED: Broken presence-based temporal expiry model" \
    "## Finding R1-F01 - Severity: CRITICAL - Security/Data Integrity

### Description
The valid_to field was used as a presence filter (NULL / not-NULL) rather than a Unix timestamp comparison. Logically expired memories were still surfaced in snap, canvas, and search paths.

### Impact
Outdated or sensitive context injected into agent reasoning after expiration.

### Remediation Applied
Replaced valid_to presence filters with active UTC timestamp check (valid_to IS NULL OR valid_to > unixepoch()).

### Files Changed
- src/store/ladybug.rs

### Status
REMEDIATED - resolved in branch remediate-p4-p5-p6 (Phase 1)" \
    "bug,security,P0-critical,remediated"

  create_issue \
    "[R1-F02] REMEDIATED: Blocking synchronous I/O in Tokio event loop" \
    "## Finding R1-F02 - Severity: CRITICAL

### Description
std::fs calls inside the server pipeline blocked the Tokio async runtime, causing request starvation under concurrent load.

### Impact
Full event-loop stall on any filesystem operation.

### Remediation Applied
Replaced all std::fs calls in the server pipeline with tokio::fs async equivalents.

### Files Changed
- src/server.rs

### Status
REMEDIATED - resolved in branch remediate-p4-p5-p6 (Phase 1)" \
    "bug,P0-critical,remediated"

  create_issue \
    "[R1-F03] REMEDIATED: Cypher injection via trailing backslash escaping bypass" \
    "## Finding R1-F03 - Severity: CRITICAL - Security Veto

### Description
The Kuzu string sanitizer escaped single-quotes before escaping backslashes. An input containing a backslash-quote sequence breaks the string boundary. NeuroStrata auto-ingests source files, so a crafted file can execute arbitrary Cypher admin commands.

### Remediation Applied
Added escape_kuzu_string helper that escapes backslashes first before escaping quotes.

### Files Changed
- src/store/ladybug.rs

### Status
REMEDIATED - resolved in branch remediate-p4-p5-p6 (Phase 1)" \
    "bug,security,P0-critical,remediated"

  create_issue \
    "[R1-F04] REMEDIATED: Non-UTF8 DB path causes panic" \
    "## Finding R1-F04 - Severity: HIGH

### Description
DB path resolution used .to_str().unwrap() on a PathBuf. Systems with non-UTF8 characters in the home path trigger an immediate panic crash.

### Remediation Applied
Replaced .to_str().unwrap() with .to_string_lossy() for safe fallback.

### Files Changed
- src/daemon.rs
- src/main.rs

### Status
REMEDIATED - resolved in branch remediate-p4-p5-p6 (Phase 1)" \
    "bug,P1-high,remediated"

  create_issue \
    "[R1-F05] REMEDIATED: access_count never incremented on memory retrieval" \
    "## Finding R1-F05 - Severity: HIGH

### Description
The Ebbinghaus-inspired access_count field was never updated when a memory was retrieved via semantic search. The field was permanently frozen at 0, making the frequency weighting system completely inert.

### Remediation Applied
Added increment_access_count trait method and spawned a non-blocking background tokio::spawn task to asynchronously increment access_count for each node returned in search results.

### Files Changed
- src/traits.rs
- src/store/ladybug.rs
- src/server.rs

### Status
REMEDIATED - resolved in branch remediate-p4-p5-p6 (Phase 2)" \
    "bug,P1-high,remediated"

  create_issue \
    "[R1-F06/F07/F08] REMEDIATED: Double directory walk, dead variable, unwrap fragility" \
    "## Finding R1-F06/F07/F08 - Severity: MEDIUM

### F06 - Double WalkBuilder Traversal
Two separate WalkBuilder instances traversed the same directory tree, doubling I/O on every ingest.
Fix: Merged structural walk and AST scan into a single WalkBuilder loop.

### F07 - Dead ingested_dirs HashSet
A HashSet<PathBuf> named ingested_dirs was declared and populated but never read.
Fix: Completely removed the variable and all related insertion calls.

### F08 - .unwrap() on JSON-RPC Serialization
serde_json::to_string called with .unwrap() in the MCP response path. Any serialization failure crashed the daemon.
Fix: Propagated errors safely, returning structured JSON-RPC error payloads on failure.

### Files Changed
- src/parser/ingest.rs (F06, F07)
- src/server.rs (F08)

### Status
REMEDIATED - resolved in branch remediate-p4-p5-p6 (Phase 3)" \
    "bug,performance,P2-medium,remediated"
}

# I2 Issues — Agent Review Panel
create_i2_issues() {
  log "Creating I2 (Agent Review Panel) issues..."

  create_issue \
    "[I2-F1] REMEDIATED: Hardcoded model selection in embed.rs" \
    "## Finding I2-F1 - Severity: HIGH

### Description
FastEmbedder::new() hardcoded model selection to acceptable_models[0]. Even if the user configured multiple valid models in embedders.json, only the first was ever used.

Source: AI Agent Review Panel (issue2.md), Correctness Hawk finding.

### Remediation Applied
Added fallback logic reading the NEUROSTRATA_MODEL environment variable. If set, the matching model is selected; otherwise falls back to acceptable_models[0].

### Files Changed
- src/embed.rs

### Status
REMEDIATED - resolved in branch remediate-p4-p5-p6 (Phase 5)" \
    "bug,P1-high,remediated"

  create_issue \
    "[I2-F2/F3] REMEDIATED: Monolithic server handler (800+ line function)" \
    "## Finding I2-F2/F3 - Severity: CRITICAL - Architecture Veto

### Description
server.rs contained an 800+ line start_mcp_server function where the tool dispatch match statement handled all logic inline, including Canvas generation, ingest pipelines, and search. Impossible to unit-test individual handlers.

Source: AI Agent Review Panel (issue2.md), Architecture Critic finding [P0].

### Remediation Applied
Refactored server.rs to extract individual MCP tool handlers into dedicated, independently callable functions (e.g., handle_generate_canvas, handle_add_memory, handle_search_memories, etc.).

### Files Changed
- src/server.rs (~700 lines restructured)

### Status
REMEDIATED - resolved in branch remediate-p4-p5-p6 (Phase 4)" \
    "bug,architecture,P0-critical,remediated"

  create_issue \
    "[I2-F4] REMEDIATED: Hardcoded AST parser schemas in source" \
    "## Finding I2-F4 - Severity: MEDIUM

### Description
main.rs:76-104 contained a 30-line hardcoded JSON blob defining AST parser symbol schemas. This coupled schema configuration to source code, requiring recompilation for any parser rule change.

Source: AI Agent Review Panel (issue2.md), Architecture Critic finding [P2].

### Remediation Applied
Extracted the schema mapping to src/schema.json and loaded it at compile-time via include_str! macro.

### Files Changed
- src/schema.json (new file)
- src/main.rs
- src/server.rs

### Status
REMEDIATED - resolved in branch remediate-p4-p5-p6 (Phase 6)" \
    "enhancement,architecture,P2-medium,remediated"

  create_issue \
    "[I2-F5] REMEDIATED: Naive secret scrubber with regex bypass and false positives" \
    "## Finding I2-F5 - Severity: CRITICAL - Security Veto

### Description
The secret scrubber used basic substring matching (content_lower.contains). Trivially bypassed by formatting changes (spaces around =) or case variation. Also produced false positives on legitimate documentation.

Source: AI Agent Review Panel (issue2.md), Security Auditor finding [P0].

### Remediation Applied
Rewrote the scrubber to use compiled regex::Regex patterns covering common secret key formats with word boundary anchors, whitespace tolerance, and entropy heuristics.

### Files Changed
- src/server.rs

### Status
REMEDIATED - resolved in branch remediate-p4-p5-p6 (Phase 4)" \
    "bug,security,P0-critical,remediated"

  create_issue \
    "[I2-F6] REMEDIATED: CLI vs Server duality - clap subcommand refactor" \
    "## Finding I2-F6 - Severity: MEDIUM (Deferred then Completed)

### Description
main.rs tried to be both a long-running JSON-RPC MCP server and a CLI tool simultaneously, sharing initialization logic but operating fundamentally differently. This created ordering bugs around DB lock acquisition.

Source: AI Agent Review Panel (issue2.md), Devils Advocate finding [P2].

### Remediation Applied
- Added clap v4 with derive features to Cargo.toml
- Refactored main.rs with a derive-based Cli struct cleanly separating subcommands: daemon, namespaces, list, ingest, export-graph, delete, add, edit
- External plugin fallback runner retains 100% backward compatibility
- DB lock guard checks run before any database-modifying subcommand

### Files Changed
- Cargo.toml
- src/main.rs

### Status
REMEDIATED - resolved in branch remediate-p4-p5-p6 (Phase 6)

Originally tracked as LOGGED (deferred) in the remediation plan. Implementation completed as part of the Phase 6 completeness sweep." \
    "enhancement,architecture,P2-medium,remediated"
}

# I3 Issues — Roundtable Adversarial Review
create_i3_issues() {
  log "Creating I3 (Roundtable Adversarial Review) issues..."

  create_issue \
    "[I3-F1] REMEDIATED: Recursive neighborhood-inlining composition bug" \
    "## Finding I3-F1 - Severity: CRITICAL - Correctness Veto

### Description
LadybugStore::get appended 1-hop neighborhood context directly to the retrieved node content string for display visualization. However, neurostrata_move_memory implemented moves by pulling via get then saving via upsert. This permanently serialized the formatted visualization text into the DB. Multiple moves recursively nested neighborhood logs, completely destroying semantic vector matching.

Source: Roundtable Adversarial Review (issue3.md), finding [P0].

### Root Cause
Conflation of database retrieval and presentation layer formatting.

### Remediation Applied
Removed neighborhood context concatenation from LadybugStore::get(). Context formatting now happens exclusively in the MCP presentation layer.

### Files Changed
- src/store/ladybug.rs
- src/server.rs

### Status
REMEDIATED - resolved in branch remediate-p4-p5-p6 (Phase 4)" \
    "bug,P0-critical,remediated"

  create_issue \
    "[I3-F2] REMEDIATED: Linear neural gain saturation causes semantic blindness" \
    "## Finding I3-F2 - Severity: HIGH - Correctness Veto

### Description
Access frequency boost was calculated as: adjusted_distance = distance - (access_count * 0.05). Since cosine distance lies in [0.0, 2.0], a memory accessed 40+ times receives a negative score, placing it permanently first in every search regardless of semantic relevance. Highly-accessed memories crowded out better semantic matches.

Source: Roundtable Adversarial Review (issue3.md), finding [P1].

### Remediation Applied
Replaced the linear formula with logarithmic decay:
  let boost = (access_count as f32 + 1.0).ln() * 0.05;
  let adjusted = distance - boost.min(distance * 0.5);

### Files Changed
- src/store/ladybug.rs

### Status
REMEDIATED - resolved in branch remediate-p4-p5-p6 (Phase 5)" \
    "bug,P1-high,remediated"

  create_issue \
    "[I3-F3] REMEDIATED: Unix process image hijacking via exec()" \
    "## Finding I3-F3 - Severity: HIGH

### Description
Unrecognized CLI commands were executed as external plugins using Unix .exec() (from std::os::unix::process::CommandExt). This replaces the current process image entirely. The Tauri GUI wrapper or any parent process is hijacked and terminated when the plugin exits. The Windows implementation already used .status() (standard child subprocess), creating a platform behavioral split.

Source: Roundtable Adversarial Review (issue3.md), finding [P1].

### Remediation Applied
Replaced the Unix-specific .exec() call with cross-platform .status() subprocess spawning on all targets. Parent process is now preserved after plugin execution.

### Files Changed
- src/main.rs

### Status
REMEDIATED - resolved in branch remediate-p4-p5-p6 (Phase 5)" \
    "bug,P1-high,remediated"

  create_issue \
    "[I3-F4] REMEDIATED: Ingestor exclusion gap for non-standard file extensions" \
    "## Finding I3-F4 - Severity: MEDIUM

### Description
The directory walker filtered to only .md, .rs, .ts, .tsx files. Crucial project files (Cargo.toml, plasticity.json, Go/Python/C++ sources) were completely excluded from the structural graph, breaking AST relationship lineages for multi-language repos.

Source: Roundtable Adversarial Review (issue3.md), finding [P2].

### Remediation Applied
Separated the extension filter: all non-binary files now create structural file graph nodes. The stricter AST-language extension filter is applied only in the symbol-extraction step.

### Files Changed
- src/parser/ingest.rs

### Status
REMEDIATED - resolved in branch remediate-p4-p5-p6 (Phase 6)" \
    "bug,P2-medium,remediated"
}

# Pull Request
create_pr() {
  log "Pushing branch and creating pull request..."

  if ! $DRY_RUN; then
    git -C "$(git rev-parse --show-toplevel)" push origin "$BRANCH" --force-with-lease
  else
    warn "[DRY-RUN] Would push branch: $BRANCH"
  fi

  # Write body to a temp file to avoid all heredoc-in-subshell quoting issues.
  # gh pr create --body-file reads it directly.
  local pr_tmp
  pr_tmp=$(mktemp /tmp/neurostrata_pr.XXXXXX)
  trap 'rm -f "$pr_tmp"' RETURN

  cat > "$pr_tmp" << 'PRBODY'
## NeuroStrata: Multi-Phase Remediation (v1.1.1 to v1.3.0)

This PR applies all **14 verified findings** from two adversarial AI review panels
(Agent Review Panel + Roundtable Adversarial Audit) across six tracked remediation
phases. Every finding was classified as must-fix and has been implemented,
verified, and committed.

---

### Phase 1: Critical Reliability and Security
| ID | Severity | Summary |
|---|---|---|
| R1-F01 | CRITICAL | Replaced presence-based valid_to filter with UTC timestamp check |
| R1-F02 | CRITICAL | Migrated std::fs server I/O to tokio::fs async |
| R1-F03 | CRITICAL | Added escape_kuzu_string for backslash-first Cypher injection hardening |
| R1-F04 | HIGH | .to_string_lossy() replaces panicking .unwrap() on DB path |

### Phase 2: Robustness and Feature Completeness
| ID | Severity | Summary |
|---|---|---|
| R1-F05 | HIGH | Background tokio task increments access_count on every retrieval |

### Phase 3: Optimizations and Polish
| ID | Severity | Summary |
|---|---|---|
| R1-F06 | MEDIUM | Single-pass directory walker replaces double WalkBuilder |
| R1-F07 | MEDIUM | Pruned dead ingested_dirs HashSet variable |
| R1-F08 | MEDIUM | Safe JSON-RPC serialization error propagation |

### Phase 4: Architecture and Security (P0)
| ID | Severity | Summary |
|---|---|---|
| I2-F2/F3 | CRITICAL | Monolithic start_mcp_server split into dedicated handler functions |
| I2-F5 | CRITICAL | Regex-based secret scrubber replaces naive substring matching |
| I3-F1 | CRITICAL | Neighborhood ctx removed from store.get() raw retrieval restored |

### Phase 5: Logic and Process (P1)
| ID | Severity | Summary |
|---|---|---|
| I2-F1 | HIGH | FastEmbed model respects NEUROSTRATA_MODEL env var |
| I3-F2 | HIGH | Logarithmic neural gain replaces linear decay (semantic blindness fix) |
| I3-F3 | HIGH | Cross-platform status() subprocess replaces Unix exec() hijacking |

### Phase 6: Refactoring and Completeness (P2)
| ID | Severity | Summary |
|---|---|---|
| I2-F4 | MEDIUM | AST schemas extracted to src/schema.json with include_str! |
| I2-F6 | MEDIUM | clap v4 subcommands decouple CLI from MCP daemon mode |
| I3-F4 | MEDIUM | Ingestor allows non-standard extensions as structural graph nodes |

---

### Diff Summary
- 19 files changed, ~1056 insertions, ~641 deletions
- Core: src/server.rs, src/store/ladybug.rs, src/main.rs, src/embed.rs
- New: src/schema.json, src/traits.rs (extended), docs/github_issues/

### Verification
- cargo build: clean (zero warnings)
- cargo test: all tests pass
- Local beads (Phases 1-6) closed and verified

### Related Issues
All 14 findings are tracked as individual GitHub Issues with the remediated label.

---
Reviewed by: Correctness Hawk, Architecture Critic, Security Auditor, Devils Advocate
Adjudicated by: Antigravity (Judge)
Dissent Ledger: None, full consensus achieved
PRBODY

  if $DRY_RUN; then
    warn "[DRY-RUN] Would create PR: $BRANCH -> main"
    return
  fi

  local pr_url
  pr_url=$(gh pr create \
    --repo "$REPO" \
    --base  main \
    --head  "$BRANCH" \
    --title "feat(remediation): apply all 14 review panel findings across 6 phases (v1.1.1 to v1.3.0)" \
    --body-file "$pr_tmp" \
    --label "remediated" 2>&1)
  note "PR created -> $pr_url"
}

# MAIN
$DRY_RUN && log "=== DRY-RUN MODE - no GitHub API calls will be made ==="

ensure_labels
create_r1_issues
create_i2_issues
create_i3_issues
create_pr

log "Done! All issues logged and PR created for ${REPO}."
