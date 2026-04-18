# NeuroStrata OpenCode Plugin (`opencode-neurostrata`)

This directory contains the native OpenCode TypeScript plugin designed to protect the NeuroStrata 3-Tier memory architecture against context compaction.

## Why this exists

AI conversational clients (like OpenCode) use rolling context windows. Once the context limit is reached, older messages are "compacted" or discarded, resulting in severe amnesia for the agent.

**The NeuroStrata Plugin solves this by hooking directly into the OpenCode lifecycle:**

1. **`experimental.chat.system.transform` (New Session Hook)**
   - Enforces the **NeuroStrata Startup Protocol** on every new chat.
   - Embeds aggressive safety constraints to prevent database destruction.

2. **`experimental.session.compacting` (Compaction Defense Hook)**
   - Intercepts the exact moment OpenCode is about to delete context.
   - Injects a severe "Task Completion & Compaction Defense" prompt, forcing the agent to perform a **Memory Review** and extract any new domain/business logic into the Qdrant database *before* the context is wiped.

## Build and Install

This plugin is automatically built and linked when you run the main repository installer (`mcp/install.sh`). 

If you need to build it manually:

```bash
cd neurostrata-plugin
npm install
npm run build
npm pack
```

Then, ensure your `~/.config/opencode/opencode.json` includes the absolute path to the extracted plugin folder in the `"plugin"` array.