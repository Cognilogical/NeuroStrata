# OpenCode Strata Plugin

This is the native OpenCode plugin for the Strata 3-Tier Memory Architecture.

It provides lifecycle hooks into OpenCode to solve two critical AI Agent problems:
1. **The Cold Start Problem:** It dynamically injects the Strata "Startup Protocol" (forcing agents to check `.sessions/`) directly into the agent's base system prompt, replacing the need to hardcode rules in `AGENTS.md`. It also natively overrides conflicting memory systems (like `opencode-beads`).
2. **The Compaction Problem:** It hooks into OpenCode's `experimental.session.compacting` lifecycle event to forcefully prompt the agent to execute a Memory Review *before* its context is wiped.
