# gate-receipt fetch-layer fixtures

Each directory is fed to `scripts/gate-receipt.sh --fixtures-dir <dir>` so the
self-test exercises the same accounting branch as `--pr` without network
access.

- `reviews.json`, `issue-comments.json` — arrays of
  `{body, author_association, author?}`.
- `threads.json` — review threads: either
  `[{"isResolved": false, "comments": [{"body": "...", "author_association": "CONTRIBUTOR"}]}]`
  or a GraphQL envelope `{"pageInfo": {"hasNextPage": false}, "nodes": [...]}`
  where `comments` may be `{"totalCount": 2, "nodes": [...]}`.
  Its presence means "GraphQL answered". Every thread, resolved or not, gates
  its own markers and needs its own receipt.
- `pull-comments.json` — fallback when `threads.json` is absent. The fallback
  fails closed: an inline finding cannot be bound to its thread, so it stays
  outstanding and the script says so on stderr. When no inline comment carries
  a marker, only the top-level pool is accounted.
- `valid-targets.json` — optional `{shas: [..], issues: [..],
  pull_requests: [..]}`. When present, `fixed@`/`filed` receipts must name
  one of these targets, simulating the `--pr` API validation. Numbers in
  `pull_requests` resolve through the issues endpoint but are pull
  requests, not issues, so they are subtracted from `issues` and never
  validate a `filed` receipt.
- `EVIDENCE.json` — the corpus stamp: `corpus_generation` (bumped whenever a
  fixture is added, removed, or changed), `base_sha` (the commit the
  expectations were computed against), and the exact fixture list. The
  self-test fails when the stamp and the directory listing disagree, so a
  verdict computed against an older corpus cannot be replayed as current
  evidence.

Markers and receipts are read from the same tokenized text: ```/~~~ fences (any
run, unclosed to EOF), 4-space/tab indented lines, and one-or-more-backtick
spans are stripped in every mode.

Receipt comments must be authored by a non-`NONE` association (fixture mode has
no PR author), and `fixed@`/`filed` never count in the same comment as the
finding. Without `valid-targets.json` the script prints that commit/issue
targets were not validated.

| Directory | Shape |
|---|---|
| `p1-unresolved` | unresolved Codex P1 thread, no receipt → fail |
| `p1-resolved` | resolved P1 without a receipt → still fail |
| `p1-resolved-filed` | resolved P1 with a `filed #207` reply → cleared |
| `p1-filed` | unresolved P1 with a `filed #207` reply → cleared |
| `envelope-thread-receipt` | envelope page with `totalCount` and a reply → cleared |
| `resolved-receipt-does-not-clear-an-unresolved-finding` | resolved P1 receipt + unresolved P1 without → fail |
| `unresolved-receipt-is-thread-local` | thread A receipt, thread B marker → fail |
| `each-unresolved-thread-has-its-own-receipt` | every unresolved thread receipted → cleared |
| `top-level-marker-not-cleared-by-thread-receipt` | top-level `CHANGES-NEEDED`, receipt only in a thread → fail |
| `thread-marker-not-cleared-by-top-level-receipt` | thread badge, receipt only at top level → fail |
| `quoted-receipt-is-not-a-receipt` | body quotes `` `fixed@abcdef1` `` in inline code → fail |
| `double-backtick-receipt` | ``fixed@abcdef1`` in a double-backtick span, valid target → still fail |
| `self-receipt-in-finding-body` | badge + unquoted `fixed@abcdef1` in the same comment → fail |
| `none-author-receipt` | `filed #207` from `NONE` → fail |
| `unvalidated-sha-receipt` | `fixed@0000000` not in `valid-targets.json` → fail |
| `unknown-issue-receipt` | `filed #999999` not in `valid-targets.json` → fail |
| `validated-target-receipt` | `fixed@abcdef1` in `valid-targets.json` → cleared |
| `pr-number-receipt` | `filed #218` naming a pull request, not an issue → fail |
| `pr-201` | #201 thread surface: 3 P1 + 1 P2 unresolved, no receipts → fail |
| `pr-204` | #204 shape: 2 unresolved P2 threads, no receipts → fail |
| `pr-204-fallback` | two P2 findings via `pull-comments.json`, no thread → fail closed with notice |
| `quoted-prose` | badge quoted in prose plus `filed #207` → cleared, no crash |
| `pooled-clearance` | inline badge + receipt in another comment → fail closed, no pooled clearance |
| `fallback-comment-only-marker` | marker only in a top-level comment, no inline finding → fail |
| `fallback-comment-only-marker-receipted` | the same marker with a receipt → cleared |
| `near-form-p1-underscore` | `P1_Badge` (underscore is not a separator) → not a marker |
| `p0-never-gates` | `**P0**`, `P0 Badge`, `P0-Badge` → never gates |
| `parser-parity-threads` | the same body on the thread surface → 3 markers, 3 outstanding |
| `parser-parity-fallback` | the same body on the fallback surface → 3 markers, 3 outstanding |
| `parser-parity-thread-body` | the same body inside a thread → 3 markers, 3 outstanding |
| `truncated-threads` | envelope with `pageInfo.hasNextPage: true` → exit 1 |
| `truncated-comments` | thread with `comments.totalCount: 101` but one node → exit 1 |
