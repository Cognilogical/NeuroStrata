# Agent Harness Plugin Interface

## `@neurostrata/plugin-core` Architecture

`@neurostrata/plugin-core` is the zero-dependency harness plugin bundle shipped under `plugins/core/`. It provides a universal adapter that wraps the NeuroStrata daemon/MCP server and exposes hooks for agent harnesses:

- `createHarnessPlugin` — factory that builds a plugin instance for a specific harness
- `handleHookEvent` — dispatches hook events (PreToolUse, PostToolUse, SessionStart, etc.) to the daemon
- `resolveProjectRoot` — walks up from a working directory to find the project root
- `discoverProjectNamespace` — reads `.NeuroStrata/config.json` (or falls back to directory name) to discover the project namespace

The plugin is harness-agnostic: the same `createHarnessPlugin` function works for OpenCode, Claude, Hermes, and Codex because it only depends on the harness hook interface, not on harness internals.

## Implementing a New Harness Adapter

To add a new harness adapter:

1. Create a new entry under `plugins/<harness>/` mirroring the structure of `plugins/core/`
2. Call `createHarnessPlugin(pluginId, harnessName)` to obtain a plugin object
3. Register the plugin's hooks with the target harness using `handleHookEvent` as the handler
4. Ensure `resolveProjectRoot` and `discoverProjectNamespace` are wired into the harness's project-root discovery

The adapter must not hardcode the project name or namespace; both are discovered at runtime from the workspace filesystem.