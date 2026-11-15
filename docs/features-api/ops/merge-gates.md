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
finding badge (`**P1 Badge**`, `**P2 Badge**`). The Codex
`<!-- codex-code-review-finding:v1 -->` comment is not a marker by itself: the
badge inside the finding it annotates is the marker. The PR body is not scanned:
it often describes the gate itself. Each marker needs one receipt in a review or
comment:

- `fixed@<sha>` — the finding is fixed at that commit;
- `filed #<issue>` — the finding is tracked in an issue;
- `ruled: <reason>` — the maintainer explicitly accepts the residual.

`scripts/gate-receipt.sh --pr <n>` counts markers and receipts and fails when a
finding has no receipt. `.github/workflows/gate-receipt.yml` runs it as the
required `gate-receipt` check. A review body may contain both the marker and its
receipt. A receipt quoted as an example inside inline code is not a receipt.

Receipts are bound to their finding. In thread mode each unresolved review
thread needs its own `fixed@`/`filed`/maintainer `ruled:` reply: a receipt in
thread A does not clear a marker in thread B, and review/issue-comment bodies
form a separate pool that neither borrows nor lends receipts. Resolved threads
contribute nothing — neither markers nor receipts — so fixing one thread can
never authorize merging another unfixed finding. The legacy `--reviews-file`
mode keeps the older global receipt pool.

Inline findings are counted from unresolved review threads when the GraphQL
`reviewThreads` query answers. The query follows `pageInfo` cursors (at most 50
pages) and requests up to 100 comments per thread; if a thread reports more
comments than were returned, the script exits 1 instead of under-counting
receipts or findings. If GraphQL is unavailable, every inline finding counts
(over-gating) and the script says so on stderr; it never treats an unresolved
thread as clearance.

GitHub Actions has no thread-resolution trigger: `pull_request_review_thread`
is a webhook event only, and listing it in `on:` makes the workflow invalid. A
bare resolve/unresolve therefore does not re-run the check; it re-evaluates on
the next review, inline comment, or PR edit. A `fixed@` or `filed` reply is an
inline comment, so the normal receipt path re-runs the check without an
unrelated push. `issue_comment` and `workflow_dispatch` are deliberately not
subscribed: they would execute a PR-supplied checkout in a context with
default-branch authority.

For offline checks, `--reviews-file <path>` takes a legacy body fixture and
`--fixtures-dir <dir>` takes `reviews.json`, `issue-comments.json`, and either
`threads.json` or `pull-comments.json` with the same payload shapes as the API.
Every fixture file is validated before it is used: `reviews.json`,
`issue-comments.json`, and `pull-comments.json` must be arrays of objects with a
string `body`; `threads.json` must be an array of objects that carry
`isResolved` and/or `comments`, where `isResolved` is boolean when present and
`comments` is an array of objects with a string `body` when present. A file that
fails validation exits 2 instead of passing. A `threads.json` envelope may carry
`pageInfo`/`nodes` and `comments.totalCount` like the API; if it claims another
page or more comments than it contains, it is treated as truncated and exits 1.
`GATE_RECEIPT_REPO=owner/name` overrides the repository used by `--pr`, so a
fork checkout can check the upstream PR.

## Why

#127 and #142 were merged 13 minutes after gating reviews at the same reviewed
SHA, with every gate still live. The receipt check is a one-line, single-
identity-safe rule: it blocks only PRs with an unadjudicated marker, and it
does not depend on the reviewer and merger being different GitHub accounts.
