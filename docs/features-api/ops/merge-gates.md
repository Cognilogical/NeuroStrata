# Merge gates

`main` is governed by the repository ruleset `secure-rule-set`. It currently
requires one approval, but admin/role/integration actors bypass it `always`,
thread resolution is off, and no status checks are required. A gating review
posted as `COMMENTED` therefore cannot block a merge.

## Required state

- Required checks: `build (ubuntu-latest)`, `test (ubuntu)`, `gate-receipt`.
- Required conversation resolution.
- Require approval of the last push.
- Remove `bypass_mode: always` for OrgAdmin, RepositoryRole 2/4/5, and
  integrations. The owner may still merge, but only after the checks pass.
- Agents never call `gh pr merge`; the owner performs the merge.

## Gate receipt

A review or comment that says `CHANGES-NEEDED`, `CHANGES NEEDED`,
`DO-NOT-MERGE`, or `DO NOT MERGE` is a gating marker, and so is a P1 or P2
finding badge (`**P1 Badge**`, `**P2 Badge**`, the bare `P1-Badge` and
shields.io forms) or a bold `**P1**` / `**P2**`. The Codex
`<!-- codex-code-review-finding:v1 -->` comment is not a marker by itself: the
badge inside the finding it annotates is the marker. Fenced code blocks and
inline code are ignored everywhere, so a marker or receipt quoted in a fence or
backtick span is not evidence. The PR body is not scanned: it often describes
the gate itself.

Each marker needs one receipt:

- `fixed@<sha>` — the finding is fixed at that commit, which must exist and be
  the PR head or an ancestor of it;
- `filed #<issue>` — the finding is tracked in an existing issue in this repo;
- `ruled: <reason>` — the maintainer explicitly accepts the residual.

`fixed@` and `filed` must be authored by the PR author or a maintainer
(`OWNER` / `MEMBER` / `COLLABORATOR`). An association-less (`NONE`) account
never counts, and the comment that carries the finding cannot receipt itself:
the receipt must be a separate reply or body. `ruled:` remains maintainer-only.
In `--pr` mode the script validates every commit and issue target against the
API; fixture and legacy modes enforce authorship only and say on stderr that
targets were not validated (fixtures may supply `valid-targets.json` to
simulate that check).

`scripts/gate-receipt.sh --pr <n>` counts markers and receipts and fails when a
finding has no receipt. `.github/workflows/gate-receipt.yml` runs it on PR,
review, and inline-review-comment events; the ruleset does **not** currently
require the `gate-receipt` check, so while its result is visible it is not an
enforced gate until the settings in "Required state" land.

Receipts are bound to their finding. Every review thread, resolved or not, must
carry its own `fixed@`/`filed`/maintainer `ruled:` reply: a receipt in thread A
does not clear a marker in thread B, and review/issue-comment bodies form a
separate pool that neither borrows nor lends receipts. Resolution alone never
clears a finding: a resolved thread without a valid receipt stays outstanding.
The fallback used when GraphQL is unavailable pools receipts globally across all
inline comments (over-gating) and announces it; the legacy `--reviews-file` shim
keeps a global, same-body pool.

Inline findings are counted from review threads when the GraphQL
`reviewThreads` query answers. The query follows `pageInfo` cursors (at most 50
pages) and requests up to 100 comments per thread; if a thread reports more
comments than were returned, the script exits 1 instead of under-counting
receipts or findings. If GraphQL is unavailable, every inline finding counts
(over-gating) and the script says so on stderr; it never treats an unresolved
thread as clearance.

GitHub Actions has no thread-resolution trigger: `pull_request_review_thread`
is a webhook event only, and listing it in `on:` makes the workflow invalid. A
bare resolve/unresolve therefore does not re-run the check; it re-evaluates on
the next review, inline comment, or PR edit. An inline `fixed@`/`filed` reply is
a review-comment event, so the normal receipt path re-runs the check. A receipt
posted as a top-level issue comment does not re-run it — `issue_comment` and
`workflow_dispatch` are deliberately not subscribed, because they would execute
a PR-supplied checkout with default-branch authority — though such a receipt
still counts the next time the check does run.

For offline checks, `--reviews-file <path>` takes a legacy body fixture and
`--fixtures-dir <dir>` takes `reviews.json`, `issue-comments.json`, and either
`threads.json` or `pull-comments.json` with the same payload shapes as the API.
Every fixture file is validated before it is used: `reviews.json`,
`issue-comments.json`, and `pull-comments.json` must be arrays of objects with a
string `body`; `threads.json` must be an array of objects that carry
`isResolved` and/or `comments`, where `isResolved` is boolean when present and
`comments` is an array of objects with a string `body` when present. A file that
fails validation exits 2 instead of passing. Fixture receipts obey the same
authorship rule as live ones (`NONE` is rejected). A `threads.json` envelope may
carry `pageInfo`/`nodes` and `comments.totalCount` like the API; if it claims
another page or more comments than it contains, it is treated as truncated and
exits 1.

`GATE_RECEIPT_REPO=owner/name` overrides the repository used by `--pr`, so a
fork checkout can check the upstream PR.

## Why

#127 and #142 were merged 13 minutes after gating reviews at the same reviewed
SHA, with every gate still live. The receipt check is a one-line, single-
identity-safe rule: it blocks only PRs with an unadjudicated marker, and it
does not depend on the reviewer and merger being different GitHub accounts.
