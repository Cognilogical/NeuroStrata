# NeuroCortex + NeuroStrata Integration

## Overview
NeuroCortex (Cognitive Deterministic Engine) and NeuroStrata (3-Tier Memory Architecture) have historically operated as separate MCP servers. However, they share a highly symbiotic relationship: NeuroCortex validates state-mutating actions and guards against hallucinations, while NeuroStrata stores the project's permanent architectural memory. 

To improve token efficiency, prevent context compaction crashes, and dramatically reduce the complexity of `AGENTS.md`, NeuroCortex now natively supports a cognitive scratchpad tool (`neurocortex_think`), replacing the need for external sequential thinking MCPs or verbose prompt engineering.

## 🧠 The `neurocortex_think` Tool
You are provided with a dedicated tool called `neurocortex_think`. This tool allows you to:
1. Process complex logic and form hypotheses.
2. Plan verifications *before* executing state-mutating commands (like `bash` or `write`).
3. Refine rules before committing them to NeuroStrata memory.

**How it solves token exhaustion:** Unlike traditional sequential thinking tools that echo your entire thought history back in JSON, `neurocortex_think` returns a highly compressed, single-line confirmation (e.g., `Thought logged successfully. Step 1 of 3. Proceed.`). This provides the structural benefit of chain-of-thought reasoning without bloating the context window and triggering emergency orchestrator crashes.

## 📋 The Simplified Agent Mandate

With `neurocortex_think` handling the sequential reasoning structurally, the massive, text-heavy `🧩 SEQUENTIAL THINKING MANDATE` previously required in `AGENTS.md` and `SKILL.md` can be deleted.

**You can replace the legacy mandate with this single rule:**
> *"You must use the `neurocortex_think` tool to structure your thoughts, form hypotheses, and plan verifications BEFORE executing any state-mutating commands or writing permanent memories."*

## 🔄 Environment Feature Flags (Graceful Degradation)

Because NeuroCortex and NeuroStrata can technically be installed independently, agents must verify their available capabilities before assuming combined functionality.

**The Capability Check:**
When initializing a new session or entering a new repository, the agent must check its `tools/list` capabilities:
1. **If both are present:** The agent must use `neurocortex_think` to plan its architecture, use `neurostrata_add_memory` to save it, and use `local_guard_validate` to execute it safely.
2. **If NeuroStrata is missing:** The agent falls back to local memory files (`MEMORY.md` or `.beads` logic) but still uses `neurocortex_think` for safety guardrails.
3. **If NeuroCortex is missing:** The agent loses the native cognitive scratchpad. It must manually fall back to raw Markdown output for its sequential thinking (Analysis -> Hypothesis -> Verification -> Conclusion) before executing unprotected `bash` commands.

## 🤝 The Future: A Unified Cognitive Deterministic Engine
Currently, NeuroCortex maintains its own behavioral rule database, while NeuroStrata maintains the Kuzu Engram database. 
Given their overlapping domains, combining NeuroCortex and NeuroStrata into a single, unified MCP binary is the logical architectural endpoint. 

**Benefits of merging:**
* **Single Binary:** One unified MCP server (less resource overhead).
* **Shared Storage:** Merging the Behavioral Constraints (NeuroCortex) with the Semantic Engrams (NeuroStrata) allows the guardrail to directly validate actions against project-specific architectural rules natively.
* **Streamlined Context:** A single system orchestrating both *what* the agent is allowed to do, and *how* it remembers what it did.
