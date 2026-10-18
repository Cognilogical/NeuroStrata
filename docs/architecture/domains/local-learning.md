---
domain: "local_learning"
description: "Local experience capture, task-specific learning data, Laya training, and evidence-gated promotion in NeuroStrata."
governs_paths:
  - "src/server.rs"
  - "src/store/ladybug.rs"
  - "src/judgment/"
  - "src/task/"
  - "src/events/"
  - "src/config.rs"
---
# Local self-improvement

**Status:** owner-directed design; written spec awaiting review. No product
implementation is implied. The full design is
[`docs/superpowers/specs/2026-10-18-local-self-improvement-design.md`](../../superpowers/specs/2026-10-18-local-self-improvement-design.md).

## Source facts and design constraints

- Learning stays local. Laya and LadybugDB are part of the same local data
  boundary. Training does not call Jev; Jev is a separately selected provider.
- Learning targets have different labels and metrics. Share a local observation
  envelope, but keep task-specific datasets, models, and evaluation gates.
- Do not treat search exposure, task completion, graph links, superseding, or
  archiving as ground-truth semantic duplicate labels. Missing outcome means
  unlabeled.
- `LadybugStore::search` returns a lower-is-closer `array_distance`, reduced by
  usage/provenance boosts; GraphRAG expansions use sentinel score `10.0`. The
  current dedup filter `SearchResult.score >= 0.85` therefore does not measure
  similarity. Dedup needs a dedicated unboosted raw-distance query, and its
  thresholds must be measured per embedder.
- Procedure candidates must match procedure type and trigger and must be active
  and unspent. Task/structural rows and machine-ingested `symbol`/`code_ast`
  declarations are not dedup candidates.
- Upstream Laya training uses Python; NeuroStrata's proposed inference path is
  Rust/ONNX. Verify checkpoint export and prediction parity before enabling
  background model promotion.

## Learning loop shape

Capture immutable, point-in-time decision observations and separately derived
outcomes in local append-only JSONL logs. Keep candidate payload snapshots by
content hash. Preserve label provenance; weak or synthetic labels may inform
training but cannot be presented as Gold results. Train Laya locally in batches
only after the dataset and runtime gates pass. Run candidates in shadow mode;
promote only against appropriate held-out baselines, with rollback to the
previous known-good model.

The first vertical slice covers memory deduplication. Expand to retrieval,
procedures, and task outcomes one learner at a time after the shared observation
and evaluation path demonstrates value.
