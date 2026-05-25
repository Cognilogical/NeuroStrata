#!/bin/bash
# 🧠 NeuroStrata — GitHub Issue Synchronizer & Status Consolidation Utility
# This script logs individual tracking issues for findings and updates review reports.

set -e

REPO="Cognilogical/NeuroStrata"

echo "Checking GitHub authentication status..."
if ! GITHUB_TOKEN="" gh auth status >/dev/null 2>&1; then
    echo "⚠️ Warning: gh is not authenticated or GITHUB_TOKEN is invalid."
    echo "We will dump the consolidated issues to local files under docs/github_issues/ instead."
    mkdir -p docs/github_issues
    
    # Dump issue 1: CLI vs Server Duality
    cat << 'EOF' > docs/github_issues/issue_i2_f6.md
Title: [REMEDIATED] [I2-F6] CLI vs Server Duality Refactor with Clap Subcommands
Labels: enhancement, architecture
Body:
### Description
The NeuroStrata command-line parser in `src/main.rs` was refactored to use `clap` subcommands cleanly separating CLI actions (`namespaces`, `list`, `ingest`, `export-graph`, `delete`, `add`, `edit`) from `daemon` mode.

### Changes Made
- Added `clap = { version = "4", features = ["derive"] }` to `Cargo.toml`.
- Refactored `src/main.rs` to parse options using a derive-based `Cli` struct.
- Implemented robust fallback logic that dynamically executes unrecognized subcommands as external plugins, maintaining 100% backward compatibility.
- Safely integrated daemon lock checks before executing database-modifying commands.

### Status
- **REMEDIATED** in local branch `remediate-p4-p5-p6`.
EOF

    # Dump review report comments
    cat << 'EOF' > docs/github_issues/reports_consolidation.md
Comment for Issue #2 (AI Agent Review Panel Report) and Issue #3 (Roundtable AI Review Report):
--------------------------------------------------------------------------------
🧠 **NeuroStrata Remediation & Verification Complete!**

All 14 findings (including all P0, P1, and P2 defects) identified in the Agent and Roundtable review reports have been successfully integrated, resolved, and verified locally under branch `remediate-p4-p5-p6`.

### Summary of Resolutions
1. **R1-F01 (Critical):** Replaced presence-based temporal filters with active UTC timestamp checks.
2. **R1-F02 (Critical):** Migrated synchronous `std::fs` server I/O to non-blocking `tokio::fs`.
3. **R1-F03 (Critical):** Added escaping of backslashes (`\\`) prior to quotes in Kuzu string sanitization.
4. **R1-F04 (High):** Replaced panicking `.to_str().unwrap()` on non-UTF8 DB paths with safe lossy conversion.
5. **R1-F05 (High):** Added background async tasks updating node `access_count` on retrieval.
6. **R1-F06/F07/F08 (Medium):** Merged double directory crawler passes, removed dead variables, and propagated JSON-RPC serialization errors.
7. **I2-F1 (High):** Configured AI model selector to respect the `NEUROSTRATA_MODEL` environment variable.
8. **I2-F2/F3 (Critical):** Factored large monolithic server tools mapping statement into individual handoff methods.
9. **I2-F4/F5 (Critical/Medium):** Extracted hardcoded symbol schemas to `schema.json` and introduced robust regex secret scrubbing filters.
10. **I2-F6 (Medium):** Decoupled CLI subcommands from stdio MCP mode using `clap` while retaining external plugin fallback runner execution.
11. **I3-F1 (Critical):** Removed display neighborhood formatting from base `store.get()` database queries, resolving nested move pollution bugs.
12. **I3-F2 (High):** Modulated Ebbinghaus semantic gain saturation boost using logarithmic scaling.
13. **I3-F3 (High):** Replaced Unix `exec()` hijack with cross-platform subprocess spawning.
14. **I3-F4 (Medium):** Patched crawler inclusion gaps to permit structural indexing of non-standard file extensions.

All code builds cleanly and local beads are finalized. Ready for merge to main!
--------------------------------------------------------------------------------
EOF
    echo "✓ Local fallback issues successfully dumped to docs/github_issues/"
    exit 0
fi

# If authenticated, try to log using gh CLI
echo "Authenticated! Creating issue on GitHub..."

# Create CLI Duality Issue
gh issue create --repo "$REPO" \
    --title "[REMEDIATED] [I2-F6] CLI vs Server Duality Refactor with Clap Subcommands" \
    --label "enhancement" \
    --label "architecture" \
    --body "Refactored the main entrypoint in src/main.rs using clap subcommands to cleanly separate MCP daemon server from CLI operations, maintaining backwards-compatible plugin execution fallback."

# Add comment to Issue 2 and Issue 3
COMMENT_BODY="🧠 **NeuroStrata Remediation & Verification Complete!** All 14 findings have been successfully resolved in branch remediate-p4-p5-p6."

echo "Commenting on Issue #2..."
gh issue comment 2 --repo "$REPO" --body "$COMMENT_BODY"

echo "Commenting on Issue #3..."
gh issue comment 3 --repo "$REPO" --body "$COMMENT_BODY"

echo "✓ Issue synchronization complete!"
