# K3 Co-Architect Verdict: Laya as Optional First-Line System One Model

**Date:** 2026-10-18
**Seat:** K3 (design-only, frontend/UI specialist)
**Verdict:** **PURSUE — gated**. Optional Cargo feature + benchmark gate. Not default.

## 1. Verified Repo Facts

- `JudgmentProvider` trait at `src/judgment/mod.rs:24` — model-agnostic, single method
  `judge_duplicates` + `health_check` + `name`.
- Sole concrete impl `TypeSafeJevProvider` (`src/judgment/typesafe_jev.rs`):
  - hardcoded `base_url = https://api.typesafe.ai/v1`
  - POSTs `/evaluate` with `{model:"jev-1.13", questions:{...}}`
  - parses `answers[*].probability` (NOT `{type:noul,noul:...}` as brief stated)
- Enable signal: `config.judgment_api_key.is_some()` (`mod.rs:79,108`); mock otherwise. *(the enable-signal check now reads `src/judgment/mod.rs:79` and `:110`; 2026-10-21)*
- Config: `judgment_api_key: Option<String>` plaintext in
  `~/.config/neurostrata/config.json` (`src/config.rs:11`). *(the field is at `src/config.rs:13` and the path is built at `src/config.rs:99-102`; 2026-10-21)*
- Circuit breaker, timeout, confidence threshold already present ✓
- No Laya references in tree.

## 2. Brief-vs-Repo Discrepancy

Brief claims Jev `/evaluate` returns `{type:noul}` and that Laya "supports the same
`/v1/systemone` wire protocol." Repo shows current client speaks an older
`/evaluate` + `answers[*].probability` shape. **∴ Adapter work is required regardless
of provider ordering.** Do not assume wire compatibility for free.

## 3. Soundness of Fallback (free Laya first, paid Jev optional)

Sound **as an option**, unsound **as a default today**.

- Base Laya ~0.36 vs 0.461 majority baseline → worse than disabled. Would regress
  trust if shipped as first-line.
- Fine-tuned ~0.766 promising but off-domain; dedup on memory payloads ≠ benchmark
  distribution.
- Owner's "local-free first, hosted-paid optional" direction is sound for privacy &
  cost — **only if a quality gate passes.**

## 4. Recommended Architecture

```
JudgmentProvider (trait, unchanged)
├─ MockJudgmentProvider     (default; no key, no model)
├─ TypeSafeJevProvider      (existing; adapt to /v1/systemone)
└─ LayaProvider             (NEW, cargo feature `judgment-laya`)
    ├─ in-process via laya-onnx (v1, CPU default)
    └─ sidecar HTTP /v1/systemone (later phase)
```

### Config schema (strict JSON)

```json
"deduplication": {
  "provider": "mock" | "jev" | "laya",
  "judgment_api_key": "…",                  // only when provider=jev
  "laya": {
    "model_path": "~/.config/neurostrata/models/laya-*.onnx",
    "accelerator": "cpu" | "cuda" | "rocm",
    "max_batch": 8,
    "timeout_ms": 2000
  }
}
```

- Keep `judgment_api_key` presence as legacy alias → `provider="jev"` with
  deprecation warning.
- Key handling: `NEUROSTRATA_JEV_API_KEY` env first, config file second; warn on
  plaintext world-readable.

## 5. Boundaries & Failure Policy

- Scope: System One = typed decision inference only. No generative, no chat, no
  autonomous LLM. Trait stays `judge_duplicates` only.
- **All failure modes fail-soft to `None` (no dedup). Never block `add_memory`:**
  - Model file missing/corrupt → log once, return None, circuit-break
  - OOM on load → disable provider for session, log
  - Inference timeout → existing CB path
  - Accelerator requested but unavailable → CPU fallback + warning (never error)
- **Never** auto-download weights at runtime. Explicit
  `neurostrata-mcp laya fetch <model>` with pinned checksums.

## 6. Training-Data Strategy (sensitive user memories)

- Default: **no training on user memories.** Ship a pre-trained dedup checkpoint.
- Opt-in fine-tuning only: `neurostrata laya tune --consent`, runs locally, never
  leaves machine, no telemetry.
- Labeled data: user-confirmed accept/reject duplicate decisions, stored as a
  separate local dataset — not in main memory DB.
- Mandatory held-out calibration split + eval split before accepting a tuned model;
  versioned model registry.

## 7. Benchmark Gate (must pass before `provider="laya"` leaves experimental)

- Corpus: ≥500 labeled dedup pairs from real NeuroStrata use (opt-in, anonymized).
- Precision ≥0.85 at recall ≥0.70 on held-out; calibration ECE <0.05.
- p99 latency ≤150 ms/candidate CPU (5 candidates ≤750 ms, inside 5 s timeout).
- Beat Mock baseline AND within 5 points of Jev on same corpus; else keep Jev
  recommended.

## 8. Packaging

- Cargo feature `judgment-laya`, **off by default** (adds `laya-onnx` +
  onnxruntime — heavy).
- Long-term prefer **separate `neurostrata-laya` sidecar binary** over in-process:
  keeps MCP server lean, enables remote/GPU hosts. In-process acceptable for v1
  behind the feature.

## 9. Phased Plan

1. **Phase 0 (this verdict):** approve direction, write ADR.
2. **Phase 1:** config → explicit `provider` enum; key→env; adapt Jev provider to
   `/v1/systemone` (per current TypeSafe docs). Ship without Laya.
3. **Phase 2:** `judgment-laya` feature, in-process CPU-only,
   `provider="laya"`. Run benchmark gate on real corpus.
4. **Phase 3:** gate passes → document Laya as recommended-free option; fails →
   keep Jev recommended, Laya stays experimental or dropped.
5. **Phase 4 (optional):** extract sidecar, add accelerator support.

## 10. Bottom Line

**Do not make Laya the default on day one.** Base-model 0.36 is below the
disabled-baseline for a dedup feature. Pursue the integration behind a feature flag
and a benchmark gate; keep Jev as the quality reference.
