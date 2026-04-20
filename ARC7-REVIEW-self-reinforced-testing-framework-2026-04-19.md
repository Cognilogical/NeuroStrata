# Architectural Review Report — Self-Reinforced Testing Framework (SRTF)

**Subject:** docs/architecture/self_reinforced_testing_framework.md
**Date:** 2026-04-19
**Mode:** Document Review
**Panel:** Context Master (Gemini 3 Pro) · The Architect (Claude Sonnet 4.6) · Security Sentinel (OpenAI o4) · Product Visionary (GPT-5.2) · Creative Strategist (GPT-5.3-Codex) · The Optimizer (GPT-5.3-Codex) · The Naysayer (Claude Sonnet 4.6)

---

## Final Recommendation: Block

The Self-Reinforced Testing Framework (SRTF) provides an excellent conceptual approach to empirically tuning agent instructions. However, the proposed "Black-Box Isolation" strategy is fundamentally flawed. Relying on directory isolation (`/tmp/neurostrata-sandbox`) and configuration overrides while the Subject Agent retains arbitrary shell execution capabilities (`bash` tool) introduces a critical sandbox escape vulnerability. The agent could easily cross-contaminate the global memory or host system. Furthermore, the evaluation loop risks severe cost overruns and prompt overfitting. The framework must be blocked from implementation until the sandbox is properly containerized using `podman`.

---

## Findings Summary

| Severity | Count |
|----------|-------|
| Critical | 1     |
| Major    | 2     |
| Minor    | 1     |
| Info     | 1     |

---

## Critical Issues (Must Address)

### SEC-01: Sandbox Escape via Shell Execution (A04 Insecure Design)
- **Severity:** Critical
- **Source:** Security Sentinel, The Architect
- **Description:** The "Black-Box Isolation" relies entirely on file paths (`/tmp/`) and localized config files (`.config/opencode/opencode.json`). Because the Subject Agent possesses a `bash` tool for repository operations, it can easily execute commands outside the sandbox directory, read the host's actual `~/.config/opencode`, or interact with the global LanceDB instance. This is not true black-box isolation and violates the core requirement of preventing cross-contamination.
- **Recommendation:** Implement true containerization. As per the global infrastructure constraints, ALWAYS use `podman` to spin up an ephemeral container for the Subject Agent. Mount only the required target repository and the isolated LanceDB instance into the container. 

---

## Major Issues (Should Address)

### ARCH-01: Risk of Prompt Overfitting
- **Severity:** Major
- **Source:** The Naysayer
- **Description:** The Evaluation Loop (Step 4 & 5) mutates the `SKILL.md` until "100% Axiom compliance rate is achieved." Iteratively rewriting instructions against a single testbed (`expressjs/express`) will likely result in heavily overfitted prompts where the agent performs the tool calls to satisfy the evaluator without actually understanding the architecture.
- **Recommendation:** Implement a validation set. Once the instructions achieve high compliance on the primary testbed, they must be validated against a completely different codebase (e.g., a Python microservice) to ensure the agent's behavior is generalized, not overfitted.

### OPT-01: Unbounded Evaluation Costs
- **Severity:** Major
- **Source:** The Optimizer
- **Description:** Running continuous full-agent loops with complex tasks on a medium-to-large codebase like Express.js will consume massive amounts of tokens. The current design lacks a budget or token-limit constraint for the evaluation loop.
- **Recommendation:** Introduce a token budget per trial and an overall cost circuit breaker in the Evaluation Loop. Consider using smaller, synthetic mock repositories for initial prompt mutations before graduating to real-world codebases like Express.js.

---

## Minor Suggestions (Nice to Have)

### PROD-01: Redundant Axiom Checks
- **Severity:** Minor
- **Source:** Creative Strategist
- **Description:** The "Graph Generation Axiom" forces the agent to autonomously execute `neurostrata_generate_canvas`. This could be automated as a post-hook on the LanceDB ingestion rather than spending agent inference cycles remembering to do it.
- **Recommendation:** Consider removing Axiom 2 from the agent's required instructions and instead trigger graph generation programmatically when the vector store updates.

---

## Informational Notes

### INFO-01: Express.js as a Testbed
- **Source:** Product Visionary
- **Description:** Express.js is a great starting point, but it primarily represents JavaScript monoliths. To ensure NeuroStrata works globally, the testing framework should eventually include multi-language microservice architectures.

---

## What Was Done Well

* The concept of empirically testing and reinforcing LLM agent instructions is highly innovative and addresses a major pain point in agent reliability.
* The 5 Verification Axioms perfectly capture the mandatory lifecycle of a NeuroStrata agent.
* Dynamic MCP server configuration via environment variables is a clean and standard approach to routing traffic.

---

## Action Items

- [ ] Redesign the isolation architecture to use `podman` containers instead of `/tmp/` directories.
- [ ] Add a cost/token circuit breaker to the Evaluation Loop.
- [ ] Incorporate a secondary "validation" repository into the evaluation phase to prevent prompt overfitting.
- [ ] Re-evaluate whether Axiom 2 (Graph Generation) should be automated rather than agent-driven.

---

*Generated by ARC-7 Panel · 2026-04-19*