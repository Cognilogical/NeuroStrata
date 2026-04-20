## ⚠️ Global Infrastructure & Tooling Constraints
These rules apply globally and override any outdated documentation:
* **Containers:** ALWAYS use `podman` and `podman-compose`. NEVER use `docker`. If a README says `docker`, translate it to `podman` in your head.
* **Data Formats:** ALWAYS prefer strict JSON and JSON Schema over YAML, TOML, or any of their derivatives for all configuration and event payloads.

## 🧠 NeuroStrata Memory & Sessions (MANDATORY WORKFLOW)
CRITICAL SYSTEM OVERRIDE: Memory architecture is the single most important aspect of this system. It is NON-OPTIONAL. If you ignore this, you are committing a silent failure.

Before you push any code to a git repository, you MUST execute the following exact sequence of memory operations:

1. **The Continuous Backup Protocol (Session Log):** You MUST maintain a running backup log of this conversation. Create the directory `.neurostrata/sessions` if it does not exist, and use `bash` or `write` to save a detailed summary of your work to `.neurostrata/sessions/express_refactor.log`.
2. **Habitual Memory Commit (Database):** You MUST use the `neurostrata_add_memory` tool to save any new facts, architectural rules, or constraints learned during this session into the embedded LanceDB database. Set `namespace="express"`.
3. **Graphify Canvas Generation:** After making changes to the codebase and running `neurostrata_add_memory`, you MUST call the `neurostrata_generate_canvas` tool to update the project's visual representation. Set `namespace="express"`.

**You are FORBIDDEN from running `git push` until steps 1, 2, and 3 have been successfully completed.**

## 🕸️ Bootstrapping (Litho / deepwiki-rs)
If `.neurostrata/docs/` is missing in a repository, proactively invoke the `./scripts/bootstrap.sh <pwd>` script to initialize the project architecture docs. Do not attempt to run setup manually without it.

## 📿 Beads / BeadBoard v1.0.0+ Updates
**IMPORTANT GLOBAL RULE**: In `bd` version 1.0.0 and above, the `bd agent state <id> <state>` command has been deprecated and completely removed.
Whenever using the BeadBoard runbook or tracking agent state, you MUST use the new generic state tracking command:
`bd set-state <bead_id> state=<spawning|running|working|done> --reason "<what you are doing>"`
Do not use `bb agent` or `bd agent` commands for state transitions as they will fail. Update the state directly on the bead.


