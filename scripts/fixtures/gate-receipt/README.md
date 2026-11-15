# gate-receipt fetch-layer fixtures

Each directory is fed to `scripts/gate-receipt.sh --fixtures-dir <dir>` so the
self-test exercises the same branching as `--pr` without network access.

- `reviews.json`, `issue-comments.json` — arrays of `{body, author_association}`.
- `threads.json` — GraphQL-shaped review threads:
  `[{"isResolved": false, "comments": [{"body": "...", "author_association": "NONE"}]}]`.
  Its presence means "GraphQL answered"; only unresolved threads gate.
- `pull-comments.json` — fallback when `threads.json` is absent: every inline
  finding counts and the script prints the over-gating notice.

| Directory | Shape |
|---|---|
| `p1-unresolved` | one unresolved Codex P1 thread, no receipt → must fail |
| `p1-resolved` | same finding marked resolved → no marker |
| `p1-filed` | unresolved P1 thread with a `filed #207` reply → cleared |
| `pr-201` | #201-shaped: 3 P1 + 1 P2 unresolved threads, no receipts → fail |
| `pr-204` | #204-shaped: 2 unresolved P2 threads, no receipts → fail |
| `pr-204-fallback` | same two P2 findings via `pull-comments.json` (no threads) → fail with notice |
| `quoted-prose` | badge quoted inside prose plus `filed #207` → cleared, no crash |
