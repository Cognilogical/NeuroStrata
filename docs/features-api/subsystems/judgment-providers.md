---
domain: "judgment_providers"
description: "Typed decision providers, Laya/Jev protocol facts, and evaluation constraints for optional memory deduplication judgments."
governs_paths:
  - "src/judgment/"
  - "src/config.rs"
  - "src/server.rs"
---
# Judgment providers: Laya and Jev evaluation notes

**Status:** research only; no provider or model-order decision adopted.
**Reviewed:** 2026-10-18.

## Current integration boundary

NeuroStrata's `JudgmentProvider` currently serves semantic duplicate checks,
not a general agent-facing System One API. The concrete TypeSafe adapter in
`src/judgment/typesafe_jev.rs` calls `/evaluate`, pins `jev-1.13`, and reads
`answers[*].probability` plus optional `reasoning`. Current public TypeSafe
System One docs instead describe `/v1/systemone` and typed `noul` answers.
Validate both protocols with fixtures before treating the existing adapter as
compatible with current System One or Laya.

The shared `embed_dedup_and_upsert` path rejects a write when a duplicate is
reported. Task-completion extraction uses this same path. A false positive can
therefore suppress new knowledge or interrupt task completion; this decision
path needs a higher bar than a non-blocking classifier suggestion.

## Laya facts and limits

The upstream Laya repository publishes `laya-onnx`, a Rust SDK for local ONNX
inference over typed `choice`, `score`, and `noul` questions. It runs on CPU by
default, with documented optional CUDA and DirectML features, and expects model
artifacts on disk or in its cache. The SDK is inference-only; the upstream
Python package provides fine-tuning workflows using labeled decisions and
held-out calibration/evaluation. Do not treat accelerator options proposed in
an architect report as verified SDK support unless upstream confirms them.

Upstream benchmark results are not NeuroStrata evidence. The upstream
`BENCHMARKS.md` reports its base checkpoint near 0.36 accuracy on its
typed-decisions benchmark against a 0.461 majority baseline; the 0.766 result
belongs to a fine-tuned checkpoint on that benchmark. Transfer to memory-pair
deduplication remains unknown. `laya-ai.com` is an independent community
resource; prefer upstream repository artifacts when resolving benchmark or
implementation claims.

## Evaluation constraints (not an implementation plan)

- Start with the existing duplicate-judgment use case; do not create a generic
  System One surface without a concrete caller.
- Compare frozen Laya against similarity-only and current Jev behavior on the
  same labeled, held-out memory pairs. Split by source/fact family to prevent
  paraphrase leakage. Include hard negatives such as negation, changed values,
  supersession, and scope differences.
- Treat upstream thresholds as proposals, not established acceptance criteria.
  Report precision, recall, calibration, confidence intervals, natural
  prevalence, and CPU latency/resource use. Separate a non-blocking shadow
  pilot from any later write-blocking decision.
- Do not train on user memories by default. Any customer-controlled training or
  evaluation needs explicit consent and local data handling; do not upload
  raw examples or resulting weights by default.
- Jev is an explicit alternate/opt-in, not an automatic fallback from local
  Laya. A credential alone is not consent to transmit memory content, and
  confidence values from distinct models are not interchangeable.
- Store credentials as environment/secret references rather than new plaintext
  config values. Keep local model download/install explicit; do not fetch large
  weights during inference or silently replace a failed local model with a
  hosted call.

## Primary references

- Laya upstream: https://github.com/NandhaKishorM/laya
- Rust SDK: https://github.com/NandhaKishorM/laya/tree/main/laya-rust
- Fine-tuning guide: https://github.com/NandhaKishorM/laya/blob/main/docs/finetune.md
- Laya benchmarks: https://github.com/NandhaKishorM/laya/blob/main/BENCHMARKS.md
- TypeSafe System One API: https://github.com/typesafe-docs/typesafeai/blob/main/docs/09-api-reference.md
- Independent architecture reports: `docs/architecture/2026-10-18-k3-laya-judgment-provider-verdict.md`, `docs/designs/2026-10-18-laya-openai-verdict.md`
