# Merge gates

`main` is governed by the repository ruleset `secure-rule-set` (`16846790`),
whose canonical declarative specification is tracked in
`scripts/main-ruleset.json` and validated/applied by
`scripts/enforce-main-ruleset.sh` (self-tested in `scripts/test-main-ruleset.sh`
under the `scripts (ubuntu)` CI job).

## Required state

- Required checks: `build (ubuntu-latest)`, `test (ubuntu)`, `gate-receipt`.
- Required conversation resolution (`required_review_thread_resolution: true`).
- Require approval of the last push (`require_last_push_approval: true`).
- Non-fast-forward and branch-deletion blocks (`non_fast_forward`, `deletion`).
- Remove `bypass_mode: always` for `RepositoryRole` 2/4/5 and `Integration`
  actors; retain only the single audited `OrganizationAdmin` break-glass
  identity (`scripts/enforce-main-ruleset.sh --apply`).
- Agents never call `gh pr merge`; the owner performs the merge.

## Gate receipt

A review or comment is a gating marker when it carries one of these forms:

- `CHANGES-NEEDED`, `CHANGES NEEDED`, `CHANGES_NEEDED`;
- `DO-NOT-MERGE`, `DO NOT MERGE`, `DO_NOT_MERGE`;
- a P1/P2 finding badge: `P1 Badge`, `P1-Badge`, `P1Badge` (and the `P2`
  equivalents `P2 Badge`, `P2-Badge`, `P2Badge`), including the shields.io
  image form and the `**P1 Badge**` / `**P2 Badge**` spellings;
- a bold `**P1**` / `**P2**`.

The underscore is not a badge separator, so `P1_Badge` and `P2_Badge` are **not**
markers, and `P0` never gates: `P0 Badge`, `P0-Badge`, `P0Badge` and `**P0**`
are ordinary prose. One regex (`marker_re` in `scripts/gate-receipt.sh`) is the
single engine for every mode, and `scripts/test-gate-receipt.sh` fails when the
engine and this list drift apart.

The Codex `<!-- codex-code-review-finding:v1 -->` comment is not a marker by itself: the
badge inside the finding it annotates is the marker. One tokenizer strips, in
every mode: fenced blocks opened with ``` or ~~~ (any run length, and unclosed
fences run to EOF), lines indented with four spaces or a tab, and inline code
spans delimited by one or more backticks (the longest opening run wins). A
marker or receipt inside any of those is not evidence. A badge quoted in prose
(`> ...`) still counts; the same badge inside a code span or a fence does not.
The PR body is not
scanned: it often describes the gate itself.

Each marker needs one receipt:

- `fixed@<sha>` — the finding is fixed at that commit, which must exist and be
  the PR head or an ancestor of it;
- `filed #<issue>` — the finding is tracked in an existing issue in this repo;
- `ruled: <reason>` — the maintainer explicitly accepts the residual.

`fixed@` and `filed` must be authored by the PR author or a maintainer
(`OWNER` / `MEMBER` / `COLLABORATOR`). An association-less (`NONE`) account
never counts, and the comment that carries the finding cannot supply its own
`fixed@`/`filed`: those must be a separate reply or body. `ruled:` is the
exception — a maintainer may rule in the finding's own body, because that is an
explicit acceptance rather than a claim of repair.
In `--pr` mode the script validates every commit and issue target against the
API; a `filed` number that resolves to a pull request does not count as an
issue. Fixture and legacy modes enforce authorship only and say on stderr that
targets were not validated (fixtures may supply `valid-targets.json` to
simulate that check).

`scripts/gate-receipt.sh --pr <n>` counts markers and receipts and fails when a
finding has no receipt. `.github/workflows/gate-receipt.yml` runs it on PR,
review, and inline-review-comment events; the workflow checks out the **base**
SHA (`github.event.pull_request.base.sha`), so the gate runs the base branch's
script and a pull request cannot edit the gate that grades it.
`scripts/main-ruleset.json` requires the `gate-receipt`, `build (ubuntu-latest)`,
and `test (ubuntu)` status checks on `main`, and
`scripts/enforce-main-ruleset.sh --live-check` / `--apply` verifies and enforces
that ruleset against GitHub (`repos/<owner>/<repo>/rulesets/16846790`).

Threads are bound to their findings. Every review thread, resolved or not, must
carry its own `fixed@`/`filed`/maintainer `ruled:` reply: a receipt in thread A
does not clear a marker in thread B. Resolution alone never clears a finding: a
resolved thread without a valid receipt stays outstanding.

Top-level review and issue-comment bodies are a **pooled count**, not per-body:
their markers and receipts are summed, so a surplus receipt can clear a later
marker in the same pool. The legacy `--reviews-file` shim also keeps a global,
same-body pool.

Inline findings are counted from review threads when the GraphQL
`reviewThreads` query answers. The query follows `pageInfo` cursors (at most 50
pages) and requests up to 100 comments per thread; if a thread reports more
comments than were returned, the script exits 1 instead of under-counting
receipts or findings.

Every page is validated before it is accepted: the response must be a JSON
object without GraphQL `errors`, `data.repository.pullRequest.reviewThreads`
must be an object, `pageInfo.hasNextPage` must be a boolean, a page that claims
another page must carry a non-empty `endCursor`, `nodes` must be an array, and
every node must carry a boolean `isResolved` and a `comments` object with a
numeric `totalCount` and an array of comment objects with string bodies. An
HTTP 200 body that fails any of these checks is not a page: the script refuses
it and takes the fail-closed fallback below, because reading a malformed page
as "no findings" would under-count findings and over-clear the gate.

When GraphQL is unavailable, the script **fails closed**. Without the threads
there is no way to bind a receipt to the finding it answers, so an inline
finding cannot be cleared by a receipt posted elsewhere: the script announces
`review-thread resolution unavailable` on stderr and exits 1 when any inline
comment carries a marker. The announcement belongs to that failure: when no
inline comment carries a marker there is nothing to announce. Pooling receipts
across inline comments is gone —
that binding was looser than thread mode and could clear an unrelated finding.
When no inline comment carries a marker, only the top-level pool is accounted.
An unresolved thread is never treated as clearance.

GitHub Actions has no thread-resolution trigger: `pull_request_review_thread`
is a webhook event only, and listing it in `on:` makes the workflow invalid. A
bare resolve/unresolve therefore does not re-run the check; it re-evaluates on
the next review, inline comment, or PR edit. An inline `fixed@`/`filed` reply is
a review-comment event, so the normal receipt path re-runs the check.

A receipt posted as a top-level issue comment re-runs it too: the workflow
subscribes `issue_comment`. That event is a privileged default-branch context
and carries no `pull_request` payload, so a `resolve` step looks the PR up
through the API (`repos/<owner>/<repo>/pulls/<n>`), takes its **base** SHA, and
the checkout uses that base SHA — never the caller's PR head. The gate therefore
always runs base-branch code, and a privileged event cannot be used to run
review-supplied code with default-branch authority. Comments on plain issues
are skipped (`github.event.issue.pull_request`), the permissions stay
read-only, and `actions/checkout` is pinned to a commit SHA.

For offline checks, `--reviews-file <path>` takes a legacy body fixture and
`--fixtures-dir <dir>` takes `reviews.json`, `issue-comments.json`, and either
`threads.json` or `pull-comments.json` with the same payload shapes as the API.
`scripts/test-gate-receipt.sh` drives the accounting branch from the fixture
corpus in `scripts/fixtures/gate-receipt/`, and
`scripts/test-gate-receipt-live.sh` drives the fetch branch from `--pr` against
a stubbed `gh` (exact call argv, review-thread pagination, truncation refusal,
the 50-page cap, per-endpoint fetch failures, receipt-target validation, the
repository override, and the fail-closed fallback). Both run in the
`scripts (ubuntu)` CI job.
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
