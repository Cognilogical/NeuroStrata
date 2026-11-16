# disk-hygiene fixtures

Fake trees for `scripts/test-disk-hygiene.sh`. Nothing here is ever deleted in
place: the test copies the whole tree into a scratch directory (`mktemp -d`)
and points the script at the copy through its environment overrides.

The committed copy keeps its mtimes, so the test re-ages the files it wants
treated as idle (`touch -d '3 days ago'`) and leaves the "fresh" ones alone.

| Path | Age | Expected |
|------|-----|----------|
| `repo/target/debug/deps/stale-test-binary` | 3 days | **survives** - cargo owns its dependency artifacts; the script never deletes them |
| `repo/target/debug/incremental/stale-cache/` | 3 days | removed (stale incremental cache) |
| `repo/target/debug/incremental/fresh-cache/` | fresh | survives |
| `repo/.worktrees/wt-stale/target/` | 3 days | removed (idle repo-local worktree target) |
| `repo/.worktrees/wt-fresh/target/` | fresh | survives |
| `worktrees/1ebe42/wt-stale/target/` | 3 days | removed (idle OpenCode worktree target) |
| `worktrees/1ebe42/wt-fresh/target/` | fresh | survives |
| `tmp-base/ns-stale/` | 3 days | removed (leaked test temp dir) |
| `tmp-base/rustc-stale/` | 3 days | removed (leaked compiler temp dir) |
| `tmp-base/ns-fresh/` | fresh | survives (too young) |
| `tmp-base/keep-me/` | 3 days | survives (name is not a cleanup pattern) |

`repo/` is turned into a git repository by the test (`git init`) so the script
can resolve its own root from `git rev-parse --git-common-dir`; the fixture
itself is committed without a nested `.git` directory.
