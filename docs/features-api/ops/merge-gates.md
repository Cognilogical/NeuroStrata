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
`DO-NOT-MERGE`, or `DO NOT MERGE` is a gating marker. The PR body is not
scanned: it often describes the gate itself. Each marker needs one receipt in a
review or comment:

- `fixed@<sha>` — the finding is fixed at that commit;
- `filed #<issue>` — the finding is tracked in an issue;
- `ruled: <reason>` — the maintainer explicitly accepts the residual.

`scripts/gate-receipt.sh --pr <n>` counts markers and receipts and fails when
receipts < markers. `.github/workflows/gate-receipt.yml` runs it as the required
`gate-receipt` check. A review body may contain both the marker and its receipt.

## Why

#127 and #142 were merged 13 minutes after gating reviews at the same reviewed
SHA, with every gate still live. The receipt check is a one-line, single-
identity-safe rule: it blocks only PRs with an unadjudicated marker, and it
does not depend on the reviewer and merger being different GitHub accounts.
