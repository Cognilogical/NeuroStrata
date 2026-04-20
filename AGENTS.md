# Agent Instructions

This project uses **bd** (beads) for issue tracking. Run `bd prime` for full workflow context.

## Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work atomically
bd close <id>         # Complete work
bd dolt push          # Push beads data to remote
```

## Non-Interactive Shell Commands

**ALWAYS use non-interactive flags** with file operations to avoid hanging on confirmation prompts.

Shell commands like `cp`, `mv`, and `rm` may be aliased to include `-i` (interactive) mode on some systems, causing the agent to hang indefinitely waiting for y/n input.

**Use these forms instead:**
```bash
# Force overwrite without prompting
cp -f source dest           # NOT: cp source dest
mv -f source dest           # NOT: mv source dest
rm -f file                  # NOT: rm file

# For recursive operations
rm -rf directory            # NOT: rm -r directory
cp -rf source dest          # NOT: cp -r source dest
```

**Other commands that may prompt:**
- `scp` - use `-o BatchMode=yes` for non-interactive
- `ssh` - use `-o BatchMode=yes` to fail instead of prompting
- `apt-get` - use `-y` flag
- `brew` - use `HOMEBREW_NO_AUTO_UPDATE=1` env var

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:ca08a54f -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### MANDATORY STARTUP WORKFLOW
**CRITICAL**: You MUST NOT start writing code, modifying files, or executing a task until you have officially tracked it in Beads.
1. Check for existing work: `bd ready`
2. If the user's request matches an existing issue, claim it: `bd update <id> --claim`
3. If the user's request is new, create it FIRST: `bd create --title="<Summary>" --description="<Details>" --type=task` and then claim it.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

## Session Completion

**When ending a work session**, you MUST complete ALL steps below. Work is NOT complete until `git push` succeeds.

### 🛡️ The 3 Resilient Soft Locks (Standalone Memory Enforcement)
Because NeuroStrata must function as a standalone MVP without an OS-level intercept, you MUST adhere to these three behavioral forcing functions to ensure knowledge is extracted despite context window bloat:

1. **The Pre-Commit/Pre-Push Hook (Behavioral Forcing):** We tie the logging requirement directly to the definition of "Done." A `git pre-push` hook is installed via `scripts/install_hooks.sh` that will BLOCK your push if the NeuroStrata DB hasn't been updated recently. If your push is blocked, you MUST run `neurostrata_add_memory` before retrying.
2. **The Checklist Abstraction (`bd` Integration):** When transitioning a bead from `working` to `done`, your closing summary MUST be accompanied by a call to `neurostrata_add_memory` to summarize the architectural decisions made during that ticket.
3. **The "Breath" Prompt (Periodic Context Checks):** For long, multi-step tasks, the context window gets dense. If a task takes more than 3-5 steps, you MUST pause, summarize the current architectural state, and commit it to Tier 3 (Task Stratum) memory before proceeding to the next major phase.

**MANDATORY WORKFLOW:**

1. **Extract Knowledge (Habitual Memory Commit)** - You MUST run `neurostrata_add_memory` to save any new facts, fixes, constraints, or hallucinations learned during this session BEFORE doing anything else.
2. **File issues for remaining work** - Create issues for anything that needs follow-up
3. **Run quality gates** (if code changed) - Tests, linters, builds
4. **Update issue status** - Close finished work, update in-progress items
5. **PUSH TO REMOTE** - This is MANDATORY:
   ```bash
   git pull --rebase
   bd dolt push
   git push
   git status  # MUST show "up to date with origin"
   ```
6. **Clean up** - Clear stashes, prune remote branches
7. **Verify** - All changes committed AND pushed
8. **Hand off** - Provide context for next session

**CRITICAL RULES:**
- Work is NOT complete until knowledge is extracted to NeuroStrata AND `git push` succeeds
- NEVER stop before pushing - that leaves work stranded locally
- NEVER say "ready to push when you are" - YOU must push
- If push fails, resolve and retry until it succeeds
<!-- END BEADS INTEGRATION -->
