# gate-receipt fetch-layer fixtures

Each directory is fed to `scripts/gate-receipt.sh --fixtures-dir <dir>` so the
self-test exercises the same branching as `--pr` without network access.

- `reviews.json`, `issue-comments.json` — arrays of `{body, author_association}`.
- `threads.json` — review threads: either
  `[{"isResolved": false, "comments": [{"body": "...", "author_association": "NONE"}]}]`
  or a GraphQL envelope `{"pageInfo": {"hasNextPage": false}, "nodes": [...]}`
  where `comments` may be `{"totalCount": 2, "nodes": [...]}`.
  Its presence means "GraphQL answered"; only unresolved threads gate, and
  receipts are per thread.
- `pull-comments.json` — fallback when `threads.json` is absent: every inline
  finding counts and the script prints the over-gating notice.

| Directory | Shape |
|---|---|
| `p1-unresolved` | one unresolved Codex P1 thread, no receipt → fail |
| `p1-resolved` | same finding marked resolved → no marker |
| `p1-filed` | unresolved P1 thread with a `filed #207` reply → cleared |
| `envelope-thread-receipt` | envelope-shaped single page with `totalCount` and a reply → cleared |
| `resolved-receipt-does-not-clear-an-unresolved-finding` | resolved P1 with a receipt + unresolved P1 without → fail |
| `unresolved-receipt-is-thread-local` | thread A receipt, thread B marker → fail |
| `each-unresolved-thread-has-its-own-receipt` | every unresolved thread receipted → cleared |
| `top-level-marker-not-cleared-by-thread-receipt` | top-level `CHANGES-NEEDED`, receipt only in a thread → fail |
| `thread-marker-not-cleared-by-top-level-receipt` | thread badge, receipt only at top level → fail |
| `quoted-receipt-is-not-a-receipt` | badge body quotes `` `fixed@abcdef1` `` in inline code → fail |
| `pr-201` | #201-shaped: 3 P1 + 1 P2 unresolved threads, no receipts → fail |
| `pr-204` | #204-shaped: 2 unresolved P2 threads, no receipts → fail |
| `pr-204-fallback` | same two P2 findings via `pull-comments.json` (no threads) → fail with notice |
| `quoted-prose` | badge quoted inside prose plus `filed #207` → cleared, no crash |
| `truncated-threads` | envelope with `pageInfo.hasNextPage: true` → exit 1 |
| `truncated-comments` | thread with `comments.totalCount: 101` but one node → exit 1 |
