# Extending NeuroStrata: Connectors, Plugins & Interfaces

## Extension Philosophy

NeuroStrata is designed as a single-binary, local-first cognitive memory, ontology, and domain-expertise engine. Extensions enable interoperability with external systems while maintaining the core's integrity and performance characteristics.

## Extension Surfaces Overview

NeuroStrata provides three primary extension interfaces:

- **WorkflowConnector**: Pluggable workflow lifecycle management for task orchestration
- **HarnessPlugin**: Agent harness adapters for different execution environments, scaffolded through `createHarnessPlugin`
- **Embedder**: Vector store and embedding service interfaces
- **JudgmentProvider**: Domain-specific reasoning and decision-making adapters
- **DataConnector**: Southbound external-data-pool interface with trusted `pointer_id` resolution and fail-closed gates

These interfaces follow a pluggable architecture where implementations can be dynamically discovered and integrated without core modification. Every extension surface follows the Tool-Agnostic AI Development Pattern: a universal core (native binaries, REST/MCP, plain Markdown, CLI) with thin client shims.

The three tiers of the documentation set map onto the extension story directly:

- Tier 1 (`docs/vision/`) explains *why* the engine exists and what it must never trade away.
- Tier 2 (`docs/features-api/`) documents the internal features and the major MCP/HTTP API surface.
- Tier 3 (`docs/extend/`) — this directory — explains how third parties extend the engine without forking it.

The Central Agentic Kernel (`AgentKernel`) composes the workflow, memory, ontology, and data-connector ports behind typed traits; see `docs/features-api/agent-kernel-and-data-connectors.md`.
