# Disk Hygiene Runbook

`scripts/disk-hygiene.sh` reclaims the disk that long-running NeuroStrata
checkouts accumulate: leaked test and compiler temp directories, stale Cargo
`target/` directories in idle worktrees, stale incremental caches, and exited
Podman containers with dangling image layers.

It never deletes database data, and it never deletes Cargo's own dependency
artifacts.

## Running it

```bash
# Show the plan without deleting anything.
bash scripts/disk-hygiene.sh --dry-run

# Reclaim.
bash scripts/disk-hygiene.sh
```

The script resolves the repository root from its own checkout, so it cleans the
tree it ships with. If the root cannot be resolved it exits with status `2`
instead of silently doing nothing — a hygiene run that reports success while
having looked at the wrong directory is worse than no run at all.

## What it removes

| Source | Rule |
|--------|------|
| Temp directories | Directories under a temp base matching `ns-*`, `ns_*`, `neurostrata*`, `lbug_*`, `procmem-*`, `mcp-restore-*`, `receipt-test-*` or `rustc*` that are older than the age threshold. |
| OpenCode worktree targets | `target/` directories under the OpenCode worktree parent, when nothing inside changed within the idle threshold. |
| Repo-local worktree targets | `target/` directories under `<repo>/.worktrees/`, same idle rule. |
| Incremental caches | Directories under `<repo>/target/debug/incremental` older than the cache threshold. |
| Podman | Exited containers and dangling (`<none>`) image layers. |

Cargo's dependency artifacts are deliberately left alone. Cargo owns that
directory; deleting files from it turns the next build into a full rebuild
and buys nothing that `cargo clean` does not buy better.

## Safety rules

* **The active-build check is re-run immediately before every destructive
  step.** A build that starts while the script runs is never raced: the
  removal is skipped and reported. Zombie (`Z`) `cargo`/`rustc` processes do
  not count as an active build.
* **`--dry-run` deletes nothing.** It prints `would remove <path>` for every
  candidate.
* **Podman pruning can be disabled** by setting
  `NEUROSTRATA_DISK_HYGIENE_PODMAN` to the empty string.

## Configuration

| Variable | Default | Meaning |
|----------|---------|---------|
| `NEUROSTRATA_REPO_ROOT` | this checkout | Root to operate on. |
| `NEUROSTRATA_OPENCODE_WORKTREES` | `~/.local/share/opencode/worktree` | Parent of the OpenCode worktrees. |
| `NEUROSTRATA_DISK_HYGIENE_TMP_BASES` | `/tmp:/var/tmp/ns-tmp` | Colon-separated temp bases. |
| `NEUROSTRATA_DISK_HYGIENE_PGREP` | `pgrep` | Process lookup used for the active-build check. |
| `NEUROSTRATA_DISK_HYGIENE_PODMAN` | `podman` | Podman binary; empty string disables pruning. |
| `NEUROSTRATA_TMP_MIN_AGE_MINUTES` | `120` | Age threshold for temp directories. |
| `NEUROSTRATA_WORKTREE_TARGET_IDLE_DAYS` | `1` | Idle threshold for worktree `target/` directories. |
| `NEUROSTRATA_OLD_TEST_BIN_DAYS` | `2` | Age threshold for incremental caches. |

## Scheduling

The script installs no timer and the repository installs none either: the
unit files are committed under `scripts/systemd/` so they can be reviewed,
but enabling them is an operator decision.

| File | Purpose |
|------|---------|
| `scripts/systemd/neurostrata-disk-hygiene.service` | The oneshot unit; ships with `--dry-run`. |
| `scripts/systemd/neurostrata-disk-hygiene.timer` | Runs the unit every six hours. |

Install and enable them with:

```bash
mkdir -p ~/.config/systemd/user
cp scripts/systemd/neurostrata-disk-hygiene.{service,timer} ~/.config/systemd/user/
# Adjust the ExecStart path in the service to this checkout.
systemctl --user daemon-reload
systemctl --user enable --now neurostrata-disk-hygiene.timer
```

Look at what the first run would do (`journalctl --user -u
neurostrata-disk-hygiene.service`) and drop the `--dry-run` flag from the
unit once the plan looks right.

## Testing it

The script's self-test runs every destructive path against copies of the
fixtures in `scripts/fixtures/disk-hygiene/`, inside a scratch directory. It
never touches the real `/tmp`, the real worktree root or the real repository.

```bash
bash scripts/test-disk-hygiene.sh
```

The CI job `scripts (ubuntu)` runs the same self-test on every pull request.
