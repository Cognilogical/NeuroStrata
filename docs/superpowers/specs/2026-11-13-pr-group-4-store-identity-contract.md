# PR Group 4: Store Identity, Sidecar Migration & Phase 8 Residuals — Contract (#172)

Status: Consolidated remediation for `#77`, `#96`, `#92`, `#91`, `#81`, `#88`, `#85`, `#84`, `#83` against `main` (`9e3b2fb`).

## Contracts

### C1 — `OpenProvenance` enum replaces `opened_via_recovery: bool` (`#84`)
`LadybugStore` tracks open provenance via:
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenProvenance {
    Verified,
    ChecksumConfigMismatch,
    UnverifiedWalReplay,
    DiscardedUnreadableWal,
}
```
with `OpenProvenance::is_verified(self) -> bool` (`matches!(self, Self::Verified)`). Width reconciliation and `LockedUnknown` empty-store upgrade gate on `self.open_provenance.is_verified()`.

### C2 — Shared width-reconcile helper and 5-rel-table UUID attestation fixture (`#83`, `#88`, `#96` P8-4/P8-5)
`reconcile_empty_locked_unknown_width` and `reconcile_pristine_embedding_width` share a single catalog-inspection and drop/recreate helper (`inspect_memory_embedding_and_count` + `drop_and_recreate_memory_schema`) with distinct policy wrappers (fail-open `Ok(false)` for `LockedUnknown` attestation vs fail-closed `Err(...)` for recorded-pristine reconciliation). `with_attestation_conn` uses a unique UUID temp path and all 5 production rel tables (`RELATES_TO`, `CONTAINS`, `GOVERNS`, `EXTRACTED_FROM`, `DEPENDS_ON`).

### C3 — `schema_ready` fast-path retry and Phase B migration for `LockedUnknown` stores (`#91`, `#81`, `#77`)
- When `init` hits the `schema_ready` fast path (`src/store/ladybug.rs:6221`) and `cached_store_state()` is still `LockedUnknown`, if `open_provenance.is_verified()` and `serving_matches_configured` holds, `init` invokes `reattest_and_upgrade_empty_locked_unknown_store_state(self.dimensions).await` so a transient sidecar failure or late-bound serving descriptor upgrades on the next `init` without requiring a process restart (`#91`, `#81`).
- For data-bearing `LockedUnknown` stores (`rows > 0`, physical width == `dimensions`, `serving == configured`), `attest_and_stamp` (or explicit migration when `serving_matches_configured` and physical width matches `dimensions`) records a `has_data: true` sidecar (`#77`) so legacy data-bearing stores enter `Recorded` identity protection once opened under a matching configured+serving model of matching width.

### C4 — CLI fallback write arms (`SetMetadata`, `NormalizeMetadata`, `Archive`) call `prepare_fallback_write_store` (`#96` P8-1, `#85`)
In `src/main.rs`, the daemon-down fallback arms for `Commands::NormalizeMetadata`, `Commands::Archive`, and `Commands::SetMetadata` build the embedder and call `prepare_fallback_write_store(&vector_store, &embedder, &namespace).await?` before mutating the store, and a dispatch-seam test verifies this ordering.

### C5 — Unique temp suffix in `model_choice::write_file_atomically` (`#92`)
`write_file_atomically` in `src/model_choice.rs` uses an atomic counter + thread/UUID disambiguator in the temp file name (`format!("{file_name}.tmp-{}-{seq}", std::process::id())`) so concurrent atomic writes within the same process never clobber the same `.tmp-<pid>` file.

## Tests

| Test | Contract |
|---|---|
| `open_provenance_variants_gate_reconciliation_and_upgrade` | C1 |
| `shared_width_reconcile_and_five_rel_table_uuid_attestation` | C2 |
| `schema_ready_fast_path_retries_locked_unknown_upgrade_and_migrates_data_bearing` | C3 |
| `fallback_write_arms_prepare_store_before_mutating` | C4 |
| `write_file_atomically_concurrent_same_process_safety` | C5 |
