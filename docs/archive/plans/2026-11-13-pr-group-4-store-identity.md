# PR Group 4: Store Identity, Sidecar Migration & Phase 8 Residuals Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Implement store-identity Phase B migration, `OpenProvenance` enum, shared width-reconcile helper, `schema_ready` fast-path `LockedUnknown` retry, concurrent-safe `write_file_atomically`, and CLI fallback write-store preparation (`#77`, `#96`, `#92`, `#91`, `#81`, `#88`, `#85`, `#84`, `#83`).

**Architecture:** Refactor `opened_via_recovery: bool` into `OpenProvenance`, unify the two width-reconcile helpers in `src/store/ladybug.rs`, retry `LockedUnknown` upgrade on the `schema_ready` fast path and record `has_data: true` when a verified `LockedUnknown` store with `rows > 0` matches `dimensions` and `serving == configured`, disambiguate temp filenames in `src/model_choice.rs`, and wire `prepare_fallback_write_store` into `NormalizeMetadata`, `Archive`, and `SetMetadata` in `src/main.rs`.

**Tech Stack:** Rust (`cargo test`).

---

### Task 1: Concurrent-safe `write_file_atomically` (`#92`) and `OpenProvenance` enum (`#84`)

**Files:**
- Modify: `src/model_choice.rs:190-218`
- Modify: `src/store/ladybug.rs:1200-1240, 2070-2200, 6245-6285`

- [x] **Step 1: Write unit tests for concurrent `write_file_atomically` calls and `OpenProvenance` variants**
- [x] **Step 2: Add atomic sequence counter to `write_file_atomically` temp filename and replace `opened_via_recovery: bool` with `open_provenance: OpenProvenance`**

### Task 2: Shared width-reconcile helper, 5-rel-table UUID fixture (`#83`, `#88`, `#96`), `schema_ready` fast-path retry (`#91`, `#81`), and Phase B data-bearing migration (`#77`)

**Files:**
- Modify: `src/store/ladybug.rs:1945-2010, 6015-6260, 19302-19410`

- [x] **Step 1: Write unit tests for 5-rel-table UUID attestation, `schema_ready` fast-path retry after transient failure/late serving bind, and Phase B `LockedUnknown` data-bearing (`rows > 0`, matching width & `serving == configured`) sidecar stamp**
- [x] **Step 2: Factor shared catalog inspection + drop/recreate helper, update `with_attestation_conn` to use `Uuid::new_v4()` and all 5 rel tables, add `LockedUnknown` retry to the `schema_ready` fast path in `init`, and stamp `has_data: true` for verified data-bearing `LockedUnknown` stores whose physical width equals `dimensions` and `serving == configured`**

### Task 3: Wire `prepare_fallback_write_store` into CLI `NormalizeMetadata`, `Archive`, and `SetMetadata` (`#96` P8-1, `#85`)

**Files:**
- Modify: `src/main.rs:4958-5212`

- [x] **Step 1: Write a test verifying `prepare_fallback_write_store` adopts a pristine store's configured model before `set_metadata` / `archive_memory` / `NormalizeMetadata` write paths**
- [x] **Step 2: Call `prepare_fallback_write_store(&vector_store, &embedder, &namespace).await?` in `Commands::NormalizeMetadata`, `Commands::Archive`, and `Commands::SetMetadata`**
