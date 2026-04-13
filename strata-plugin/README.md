# Strata OpenCode Plugin (`opencode-strata`)

This directory contains the native OpenCode TypeScript plugin designed to protect the Strata 3-Tier memory architecture against context compaction and legacy tool overrides.

## Why this exists

AI conversational clients (like OpenCode) use rolling context windows. Once the context limit is reached, older messages are "compacted" or discarded, resulting in severe amnesia for the agent.

Furthermore, some legacy agent tools (like the `opencode-beads` plugin) inject unauthorized instructions into the agent's system prompt (e.g., commanding the agent to use deprecated `bd remember` tools).

**The Strata Plugin solves this by hooking directly into the OpenCode lifecycle:**

1. **`experimental.chat.system.transform` (New Session Hook)**
   - Actively overrides legacy `opencode-beads` brainwashing and strictly commands the agent to use the `strata` MCP tools.
   - Enforces the **Strata Startup Protocol** on every new chat.

2. **`experimental.session.compacting` (Compaction Defense Hook)**
   - Intercepts the exact moment OpenCode is about to delete context.
   - Injects a severe "Task Completion & Compaction Defense" prompt, forcing the agent to perform a **Memory Review** and extract any new domain/business logic into the Qdrant database *before* the context is wiped.

## Build and Install

This plugin is automatically built and linked when you run the main repository installer (`mcp/install.sh`). 

If you need to build it manually:

```bash
cd strata-plugin
npm install
npm run build
npm link
```

Then, ensure your `~/.config/opencode/opencode.json` includes `"opencode-strata"` in the `"plugin"` array.