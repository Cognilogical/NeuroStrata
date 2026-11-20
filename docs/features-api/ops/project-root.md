# Project root: setting it and correcting legacy data

A project's root is the folder that holds `.NeuroStrata/config.json`, a small
JSON file naming the project's namespace:

```json
{ "version": 1, "namespace": "example-project", "source_id": "example-project" }
```

`source_id` is optional. The file holds no absolute paths, so it stays valid
after a move or rename. It is different from the user config at
`~/.config/neurostrata/config.json`.

Only four commands write it: `neurostrata-mcp hooks install`,
`neurostrata-mcp root set`, `neurostrata_task_setup` and
`neurostrata_bootstrap`. Other calls (`ingest`, `append_log`,
`get_snapshot`, task claim, `memories_for_file`, `add_memory`) only add a
one-line hint when the project has no marker yet.

## Steps

1. **After upgrading, run `neurostrata-mcp hooks install` in each git clone.**
   For a project that is not a git checkout, call `neurostrata_task_setup`
   instead; `hooks install` needs git. It writes `.NeuroStrata/config.json`
   at the top of the main checkout (also when run from a linked worktree),
   or it tells you what to choose:
   - Which namespace: when no harness config, `NEUROSTRATA_NAMESPACE` or
     session ledger names exactly one namespace, it lists the candidates.
     Run `neurostrata-mcp hooks install --namespace <ns>`. The hooks are
     installed either way.
   - Which root: when the namespace's cached root is a subfolder, it offers
     `neurostrata-mcp root set <subfolder> --namespace <ns>` (keeps today's
     stored keys) and `neurostrata-mcp root set <top> --namespace <ns>`
     (correct the data first, steps 2 to 5).
   - When another namespace's marker already covers the folder, it installs
     nothing and points at `root set ... --replace`.
   - In a second clone of a project whose cached root is the first clone
     (which has its own marker), it writes the marker in this clone and
     leaves the cached root on the first clone. `root set` in this clone
     moves it.
2. **Before moving a project folder or changing its root, run
   `neurostrata-mcp root audit`.** Memories stored with absolute paths stop
   matching after a move, so correct them first. When the new root is inside
   the current one or contains it, `root set`, `hooks install` and setup run
   the same check and refuse a root change that would stop memories
   matching; they list their ids. `root set --force` changes the root anyway.
3. **Apply each printed `neurostrata_supersede_memory` call.** The old
   version stays as history, with the reason recorded. An agent can work
   through the list.
4. **Handle the "needs your decision" rows.** If the file moved, supersede
   the memory with its new path. If the file was deleted, decide whether the
   memory still applies: supersede it with corrected text, or archive it.
5. **Re-ingest from the root** (`neurostrata-mcp ingest <root> <ns>`) so code
   nodes match the root.
6. **Check:** `neurostrata-mcp root audit` exits 0, and
   `neurostrata_memories_for_file` on a few files returns their memories.

## Commit the file or keep it per clone

NeuroStrata does not commit the file or edit `.gitignore`. Each clone sets
its own root. When the root is a subfolder, or a split (a subfolder that
belongs to another namespace), `root set` recommends committing the file so
other clones use the same root. When the file is neither committed nor
ignored, `hooks install` suggests one or the other.

## `root set`

```bash
neurostrata-mcp root set <dir> --namespace <ns> [--source-id <id>] [--replace] [--force]
```

- Writes the marker (in the main checkout when `<dir>` is in a linked
  worktree) and sets the namespace's cached root. The command itself writes
  every file; a running daemon only updates the cached root, and with no
  daemon the command opens the store itself.
- Refuses the home folder, a drive or filesystem root, and a non-git folder
  with git checkouts below it.
- Refuses a folder that holds another namespace's marker, or a second marker
  for the same namespace in the checkout, unless `--replace`. With
  `--replace` it publishes the new marker, updates the registry, then removes
  the old marker. If it is interrupted, `root show` lists both markers and
  rerunning the same command finishes.
- `--source-id` records the id in the marker. It binds the id
  (`project-context bind`) only when the id is unbound or bound to a folder
  that no longer exists; a live binding is never replaced.
- A split (`<dir>` under another namespace's marker) is allowed.

`neurostrata-mcp root show [dir]` reports which marker applies, the cached
root, and other markers in the checkout.
