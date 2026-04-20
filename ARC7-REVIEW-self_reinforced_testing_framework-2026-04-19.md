# Architectural Review Report — Self-Reinforced Testing Framework (SRTF)

**Subject:** docs/architecture/self_reinforced_testing_framework.md
**Date:** 2026-04-19
**Mode:** Document Review
**Panel:** Context Master (Gemini 3 Pro) · The Architect (Claude Sonnet 4.6) · Security Sentinel (OpenAI o4) · Product Visionary (GPT-5.2) · Creative Strategist (GPT-5.3-Codex) · The Optimizer (GPT-5.3-Codex) · The Naysayer (Claude Sonnet 4.6)

---

## Final Recommendation: Request Changes

The panel has reviewed the updated Self-Reinforced Testing Framework (SRTF) architecture. While the move to strict Black-Box Isolation via Podman containers provides a strong foundation for filesystem and network isolation, the Security Sentinel has identified a remaining critical flaw that fails to fully mitigate the SEC-01 sandbox escape vulnerability. 

---

## Findings Summary

| Severity | Count |
|----------|-------|
| Critical | 1     |
| Major    | 1     |
| Minor    | 2     |
| Info     | 1     |

---

## Critical Issues (Must Address)

### 1: Incomplete Mitigation of SEC-01 Sandbox Escape (A04-insecure design)
- **Severity:** Critical
- **Source:** Security Sentinel
- **Description:** While Podman provides filesystem and network isolation, the architecture relies on running the agent inside the container. If the agent inside the container is granted access to the Docker socket or the host's Podman socket to manage its own containers (Docker-in-Docker or Podman-in-Podman), a malicious or compromised agent could use this socket to break out of the container and execute commands on the host system. The document does not explicitly state whether the container will run in privileged mode or if the host's container socket will be mounted. Given the agent's need to potentially run other tools or containers as part of its tasks, this is a highly probable vector for sandbox escape.
- **Recommendation:** Explicitly document the container security context. State that the container MUST NOT be run in `--privileged` mode. Ensure that the host's Podman/Docker socket is NEVER mounted inside the evaluation container. If the agent requires containerization capabilities during the test, use a secure rootless Podman-in-Podman configuration with strictly mapped user namespaces, or provide a mocked environment.

---

## Major Issues (Should Address)

### 2: Unclear Evaluation Audit Mechanism
- **Severity:** Major
- **Source:** The Architect, The Naysayer
- **Description:** Section 4, Step 3 mentions the Evaluator extracts isolated files or "runs a query command inside the container before exit". This is vague and introduces brittleness. Relying on executing commands inside a container just before it terminates is prone to race conditions and failure if the agent crashes the container unexpectedly. 
- **Recommendation:** Standardize the audit mechanism. The Evaluator should mount a dedicated, isolated volume to the container for the vector database and logs. Once the container naturally exits or is forcefully stopped after a timeout, the Evaluator inspects the data on that specific volume from the host side, ensuring a reliable and crash-proof audit trail.

---

## Minor Suggestions (Nice to Have)

### 3: Abstract the Target Testbed
- **Severity:** Minor
- **Source:** Creative Strategist
- **Description:** Hardcoding `expressjs/express` as the single target testbed in the architecture document limits the perceived scope of the framework.
- **Recommendation:** Define a generic "Testbed Profile" interface and use Express as just one example. The framework should support swapping out testbeds easily to evaluate the agent across different language ecosystems (e.g., Python/Django, Rust/Axum).

### 4: Performance Overhead of Container Churn
- **Severity:** Minor
- **Source:** The Optimizer
- **Description:** Rebuilding the container image and running a full `npm install` (implied by Express) for every single mutation iteration in the evaluation loop will be extremely slow.
- **Recommendation:** Optimize the Dockerfile/Containerfile to heavily cache dependencies and only inject the `SKILL.md` / `AGENTS.md` files in the final layer. Alternatively, use a base image with the target repo already cloned and dependencies installed.

---

## Informational Notes

### 5: Align Axiom 1 with new documentation
- **Source:** The Architect
- **Description:** Axiom 1 references `neurostrata-mcp ingest`. Ensure this command aligns with the latest `neurostrata` skill definitions, as tool names frequently evolve.

---

## What Was Done Well

- Clear definition of the 5 Core Verification Axioms provides a solid, measurable foundation for agent evaluation.
- The shift to Podman for network and filesystem isolation correctly addresses the port conflict issues with the host's `neurostrata-mcp` server.
- The concept of an autonomous evaluation loop for prompt/skill tuning is highly innovative.

---

## Panel Breakdown

### The Architect (Claude Sonnet 4.6)
- **Recommendation:** Request Changes
- **Summary:** The core architecture is sound and the 5 Axioms are well-defined. However, the mechanism for auditing the container post-run needs to be more robust and deterministic.
- **Findings:** 1 Major, 1 Info

### Security Sentinel (OpenAI o4)
- **Recommendation:** Block
- **Summary:** The SEC-01 vulnerability is not fully mitigated. The document must explicitly prohibit mounting the container socket and running in privileged mode to prevent sandbox escapes.
- **Findings:** 1 Critical

### Product Visionary (GPT-5.2)
- **Recommendation:** Approve
- **Summary:** This framework will drastically accelerate the iteration speed for tuning our agent prompts. The focus on measurable axioms ensures ROI on the tuning process.
- **Findings:** 0

### Creative Strategist (GPT-5.3-Codex)
- **Recommendation:** Approve with Conditions
- **Summary:** Great concept. We should ensure the framework isn't hardcoded to only test Express.js, as we need to prove the agent works across multiple languages.
- **Findings:** 1 Minor

### The Optimizer (GPT-5.3-Codex)
- **Recommendation:** Approve with Conditions
- **Summary:** The evaluation loop will be bottlenecked by container build times if not carefully optimized. Image layer caching is crucial here.
- **Findings:** 1 Minor

### The Naysayer (Claude Sonnet 4.6)
- **Recommendation:** Request Changes
- **Summary:** I foresee the Evaluator agent frequently failing to audit the Subject agent if the Subject gets stuck in an infinite loop or crashes the container before the "query command" can run. External volume inspection is mandatory.
- **Findings:** 1 Major (shared with Architect)

---

## Dissenting Opinions

Panel was unanimous in agreeing that the framework is a positive direction, provided the security and audit mechanisms are solidified. The Security Sentinel correctly exercised veto power to block based on the incomplete SEC-01 mitigation.

---

## Action Items

- [ ] Update Section 3 to explicitly ban `--privileged` mode and host socket mounting.
- [ ] Update Section 4, Step 3 to mandate external volume inspection for the audit phase rather than running commands inside the container.
- [ ] Optimize the container build process to cache target repository dependencies.
- [ ] Generalize the Target Testbed section to support multiple repositories.

---

*Generated by ARC-7 Panel · 2026-04-19*