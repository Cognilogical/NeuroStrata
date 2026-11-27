# Prefrontal Cortex — Semantic Action Guard & Behavioral Validation

> **Why you care:** Prompt instructions like *"Never run docker"* or *"Never drop production tables"* fail the moment context compacts. The Prefrontal Cortex turns stored architectural rules into an active interceptor (`neurostrata_guard_validate`) that evaluates candidate agent actions semantically and returns a deterministic verdict before execution.

---

## 1. How the Prefrontal Cortex Works

Implemented in `src/guard/`, the Prefrontal Cortex acts as the inhibitory control center of NeuroStrata. Before an agent executes a shell command, file mutation, or infrastructure operation, the harness plugin or agent calls `neurostrata_guard_validate`:

```text
Candidate Action ("docker compose up -d")
       │
       ▼
┌──────────────────────────────────────────────────────────┐
│ 1. List Candidate Rules: `rule` + `guard_rule` rows with │
│    the guard fields, in the project and `global`         │
├──────────────────────────────────────────────────────────┤
│ 2. Fire Each Rule Whose `trigger_pattern` Matches        │
├──────────────────────────────────────────────────────────┤
│ 3. Optional Podman-Sandboxed Dry-Run Verification        │
└──────────────────────────────────────────────────────────┘
       │
       ▼
Named Verdict: DeterministicReject | SandboxReject | SandboxPassHighFidelity |
              SandboxPassLowFidelity | ApprovedFailOpen
              (with governing Rule ID & rationale)
```

---

## 2. Structured `guard_rule` Metadata

Any `rule` memory in the `global` or project namespace can carry structured `guard_rule` metadata that upgrades it from passive documentation into an enforced guard (`ENFORCED`):

```json
{
  "name": "neurostrata_guard_learn",
  "arguments": {
    "namespace": "global",
    "rule": {
      "id": "guard-docker-policy",
      "version": 1,
      "rule_class": "infrastructure",
      "trigger_pattern": "docker",
      "constraint_text": "Always use podman and podman-compose; never invoke docker.",
      "hit_count": 0,
      "status": "active"
    }
  }
}
```

`neurostrata_guard_learn` stores the rule as a `guard_rule` memory whose metadata carries exactly those six fields (`id`, `version`, `rule_class`, `trigger_pattern`, `hit_count`, `status`); the `constraint_text` becomes the memory content. `neurostrata_guard_list_rules` reads them back.

**There is no `metadata.guard_rule` sub-object.** The guard fields live directly on the rule memory's `metadata`, and `trigger_pattern` is matched as a **case-insensitive substring** of the action type or the payload — it is not a regular expression, so `docker` fires on any payload containing the word. An empty `trigger_pattern` never fires.

A `rule` memory can *also* carry the task-gate's honesty fields — `metadata.source` (required: `rule_without_source` fires without it), `metadata.enforcement` (`ENFORCED`, `PARTIAL`, or `NOT_ENFORCED`), and `metadata.guard` (the id of a wire from the closed registry in `src/task/wiring.rs`). Those three fields are what `neurostrata-mcp task validate` audits; they are a different mechanism from the semantic guard and are not what `neurostrata_guard_learn` writes.

### Honesty Verification in `task validate`
NeuroStrata prevents "paper rules": if a memory claims `enforcement: "ENFORCED"` in its metadata without a wired guard specification, `neurostrata-mcp task validate` flags it as `rule_overclaims_enforcement`.

---

## 3. Candidate Rules and Trigger Matching

Evaluation has two steps, and only the second decides a verdict:
1. **Candidate listing:** the guard lists every `guard_rule` row and every `rule` row that carries the guard fields (`id`, `version`, `rule_class`, `trigger_pattern`, `hit_count`, `status`), in the project namespace and in `global`. A `rule` row without those fields is skipped. Retired, mesh-conflicting and registry `rule` rows are skipped. The candidates are not ranked or cut by how close their text is to the action: the payload is not embedded.
2. **Deterministic Trigger Matching:** Each candidate rule carries a `trigger_pattern` (`|`-separated branches, matched case-insensitively). How a branch is matched depends on the action surface (`GuardValidator::matches_trigger` in `src/guard/validator.rs`):
   - **Action type:** fires when a branch appears in `action_type`.
   - **File-mutating actions (`write`, `edit`, `patch`, etc.):** extracts target file paths and `#!` shebang lines from JSON/patch/raw payloads. Extension branches (such as `.py`) match target filenames at the extension boundary, and command/interpreter branches match `#!` shebang lines or target filenames — prose inside `content`, `oldString`, or `newString` never false-positives command or extension rules.
   - **Shell actions (`bash`, `shell`, etc.):** extracts the command field from JSON tool payloads (ignoring metadata such as `description` or `workdir`), unwraps subshells (`sh -c`, `eval`, `$(...)`, backticks) and command wrappers (`env`, `sudo`, `uv run`, `poetry run`), and matches command branches at the invoked command surface rather than inside quoted prose arguments such as `git commit -m "..."`.
   - There is **no built-in list of banned binaries or destructive flags** — `rm -rf /`, `git push --force` and `DROP TABLE` are caught only if a rule's trigger pattern matches them.

An action that rephrases a prohibited operation without containing a rule's trigger pattern is **not** caught by the guard. Semantic closeness between a rule's text and the action plays no part in a verdict. To cover several phrasings, list them as `|`-separated branches in one `trigger_pattern` (for example `docker|dockerd`).

---

## 4. Podman-Sandboxed Verification

For high-risk operations or self-reinforced test checks, the guard subsystem integrates with rootless **Podman** containers to verify command behavior in an ephemeral sandbox without exposing the host filesystem or network.

The sandbox invocation (`src/guard/sandbox.rs`) is:

```bash
printf '%s\n' "<payload>" | podman run --name=neurostrata-sandbox-<uuid> --rm -i --cap-drop=ALL --pids-limit=256 --memory=512m --network=none -v=<cwd>:/workspace:ro --tmpfs=/tmp -w=/workspace alpine@sha256:bc21487101ae47172509130ee1092cc273a831968d64b617776d1f69c81e42ab sh -s
```

- `--rm` -- the container is ephemeral; nothing survives the run.
- `--name=neurostrata-sandbox-<uuid>` -- every run is named, so the timeout path can remove the container itself (`podman rm -f`) rather than only killing the `podman run` client.
- `-i` / `sh -s` -- the script payload is piped over standard input rather than passed as a `-c <payload>` command-line argument, keeping untrusted payloads off the host process command line (`neurostrata-911e`).
- The image is pinned by digest (`alpine@sha256:...`), never by tag, and the run passes `--cap-drop=ALL`, `--pids-limit=256`, and `--memory=512m`, so the payload gets no Linux capabilities and bounded pids and memory.
- `--network=none` -- no network namespace, so the sandbox cannot phone home.
- `-v=<cwd>:/workspace:ro` -- the project directory is mounted **read-only**; the sandbox cannot write to the host filesystem. (There is no `--read-only` container flag -- the read-only guarantee comes from the `:ro` mount.)
- `--tmpfs=/tmp` -- scratch space in memory, so a sandbox that writes to `/tmp` cannot touch the host.
- `-w=/workspace` -- the working directory inside the container.

Three properties bound the sandbox:

1. **Timeout.** The run is bounded by a **5-second** timeout; a container that outlives it is killed and the verdict is `SandboxReject`.
2. **Action-type scoping.** The sandbox only runs for the `bash` and `script` action types. Any other action type returns `ApprovedFailOpen` *after* the deterministic and semantic rule checks have passed -- the sandbox is not applicable to it, so it is neither run nor treated as a failure.
3. **Fail-closed and path-guarded.** If no container engine is available, the verdict is `DeterministicReject` -- never a silent pass. The working directory is canonicalized before it is mounted, and `/`, `/etc`, `/home`, `/var/run`, `/run`, `/private/var/run`, `/root`, `/sys`, `/dev`, `/proc` and `/boot` are refused outright (whole path components, so `/etcetera` and `/homework` stay ordinary project directories, while `/home/user/project` is refused per issue #410). The current user's home directory itself, any ancestor of it, and sensitive directories under it (`.ssh`, `.aws`, `.gnupg`, `.kube`, `.docker`, `.config`, `.password-store`, `.local/share/keyrings`, `Library/Keychains`, `Library/Application Support`) are refused as well; an ordinary project directory under home on a non-`/home` home layout (for example `/Users/alice/project`) stays mountable. The refusal only applies to the sandboxed `bash`/`script` action types -- other action types mount nothing and return `ApprovedFailOpen` as described above.

The verdict that comes back is one of `SandboxPassHighFidelity`, `SandboxPassLowFidelity`, or `SandboxReject`, each carrying the governing rule IDs and a rationale. The sandbox itself only ever returns `SandboxPassLowFidelity` or `SandboxReject`; `SandboxPassHighFidelity` is the validator's verdict for an action that passed every deterministic and semantic check when the sandbox was **not** requested (`use_sandbox` false, `src/guard/validator.rs`).
