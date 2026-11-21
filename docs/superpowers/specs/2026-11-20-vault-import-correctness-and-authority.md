# 2026-11-20 — Vault Import Correctness & Path Authority (W-A)

> **Baseline**: `cognilogical/main` @ `216e9d56` > **Issues**: #335, #336 (P1), #337 (P2/security)
> **Branch**: `fix/wa-vault-import-correctness` > **Workflow**: spec → red evals → minimal fix → green evals → regression suites

---

## 1. Scope

One tranche over the shared vault import pipeline (`src/vault_ld/vault_sync.rs`
and `VaultNote` helpers in `src/vault_ld/mod.rs`). The three defects share the
same read → plan → write path, so they are specified and tested together to
prevent fixing one by re-breaking another.

Non-goals: export behavior (already covered by #297 tests), taxonomy,
kernel/connector durability, guard extraction, and any vault format change.

---

## 2. #335 — Task state must survive a vault round-trip

### Problem

`import_vault_with_limits` rebuilds the replacement head metadata from scratch
and preserves only `visible_to` / `eligibility` from the prior head. Task rows
are written by `write_task_row` as `{task: {...}}` with no
`canonical_sha256`, so the byte-identical no-op check can never match a task
row. Every reimport — including an unchanged round-trip — replaces the head
and drops `metadata.task`. `task::gate` then reads the completed task as
`open`, and the live claim's `session_id` is gone.

### Invariant

A vault round-trip must not change task status, session claim, assignee,
blockers, priority, labels, or history on the live head.

### Behavior specification

1. **Unchanged reimport is a no-op.** When the parsed note's fingerprint
   equals the prior head's identity, no head replacement and no history row
   is written — for task rows as well as ordinary rows. The prior head's
   identity is its `metadata.canonical_sha256` when present, otherwise the
   fingerprint derived from the prior payload (content, epistemic tier,
   canonical related set) using the same derivation as
   `VaultNote::from_search_result`.
2. **Changed reimport preserves structured state.** The replacement head starts
   from the prior head's metadata, minus keys the import owns or that the store
   derives, then overlays the import-owned values. `metadata.task` is preserved
   verbatim.
   - Import-owned (never copied from prior): `vault_source`, `stub`,
     `canonical_sha256`, `related`, `valid_from`, `supersedes`.
   - Edge renderings are import-owned too: `related_to`, `relates_to`,
     `governs`, `depends_on` are removed from the copy. The vault note's
     parsed link set is authoritative; keeping an alternate edge field would
     resurrect links deleted from the vault because `from_payload` unions all
     four fields on the next export.
   - Store/bitemporal-derived (never copied): `facets`, `facets_bitemporal`,
     `concept_iri`, `topology_original`, `valid_to`, `superseded_by`,
     `archived_by_vault_import`.
   - Everything else (including `task`, `visible_to`, `eligibility`, and future
     structured fields) is preserved.
3. Existing non-task behavior is unchanged: same fingerprint logic, same
   history retirement, same sovereignty checks.

### Evals (must fail on baseline)

- `import_unchanged_task_roundtrip_is_a_noop_and_keeps_task_state`
  - Seed a `memory_type: "task"` head with `{task: {status:"done",
session_id:"sess-1", assignee:"agent-a", blocked_by:["other"],
history:[...]}}`, no `canonical_sha256`.
  - Write its exported form unchanged into the vault, import.
  - Assert `notes_imported == 0`, head content/type unchanged, and
    `metadata.task` equal to the seeded value (status done, session intact).
- `import_task_edit_preserves_task_state`
  - Same seed, changed body in the vault.
  - Assert new head content is the edited body, `metadata.task` preserved,
    `supersedes` points at the retired copy.
- `import_reconciles_preserved_edge_fields_with_edited_links`
  - Seed a prior head with `related`, `related_to`, `governs`, `depends_on`
    and an unrelated `custom_state`.
  - Reimport an edited note keeping only one link.
  - Assert the new head's edge fields are exactly the parsed link set (no
    stale alternates), `custom_state` survives, and `from_payload`'s exported
    related set does not resurrect deleted links.

---

## 3. #336 — A WikiLink stub must never replace an existing head

### Problem

`unresolved_wiki_targets` only knows the notes parsed from the vault. When a
linked memory exists in the store but its file is absent from the vault
(partial export, deleted file, parse/validation rejection), the target is
reported missing and the stub loop mints a placeholder under the existing ID.
Planning then upserts it over the live head, replacing real content with
`Stub node for unresolved WikiLink [[target]]` and the type with
`structural`.

### Invariant

Stub minting is additive only. It may create a node that does not exist; it
may never replace an existing head in the namespace.

### Behavior specification

1. **Store resolution before minting.** For each unresolved target, read the
   target's head from the store. If a head exists, do not mint a stub; record
   an explicit import note/error naming the target (the target stays
   unresolved in the vault, which is the truthful state).
2. **Planning backstop.** If a stub is nevertheless planned for an ID with a
   prior head (e.g. a concurrent write between resolution and planning), skip
   the plan and record an explicit refusal. A stub verdict must never reach
   the write phase over an existing head.
3. Store read errors are reported and the target is not minted (fail closed —
   absence of evidence is not evidence of absence).
4. Genuinely absent targets (no head in the namespace) still mint exactly as
   before, with `stub: true` and `structural` type.

### Evals (must fail on baseline)

- `import_does_not_stub_over_existing_head_absent_from_vault`
  - Seed `existing-target` owned by the importing principal with real content
    and `fact` type; place one vault note linking to it; no
    `existing-target.md`.
  - Assert: import succeeds for the linking note, the target head still has
    the original content and type, `metadata.stub` is not true, no
    `supersedes`, and the result records the unresolved target.
- Existing `import_mints_stubs_and_preserves_prior_head` remains green
  (true-missing stub path).

---

## 4. #337 — Import must not read through symlinks outside the authority boundary

### Problem

`canonicalize_vault_path` checks only the vault root. Phase 1 then calls
`std::fs::metadata` and `std::fs::read_to_string` on each candidate; both
follow symlinks, and no candidate's resolved path is checked against the
project root / `allowed_external_dirs`. A `.md` symlink planted in an
authorized vault can import a file from anywhere the process can read.

### Invariant

Every byte the importer reads comes from a path that resolves inside the
project root or `allowed_external_dirs` (J6 authority), and the path used to
read is the resolved path that was authorized.

### Behavior specification

1. Resolve each candidate before reading it, using the same authority
   predicate as the vault root (`daemon::check_caller_path` with the same
   `project_root` and `allowed_external_dirs`).
2. Read metadata and content from the resolved path, not the raw entry path,
   so the authorization and the read cannot be split by a symlink swap.
3. A candidate that resolves outside the boundary is skipped and recorded as
   an explicit refusal naming the file. Unreadable/broken entries keep their
   existing "skipping unreadable" behavior.
4. In-bound regular notes (and in-bound symlinks that resolve inside the
   authorized boundary) keep importing.

### Evals (must fail on baseline)

- `import_refuses_md_symlink_outside_authorized_roots`
  - Vault inside the project root; a valid note written outside the root and
    symlinked into the vault as `escape.md`.
  - Assert: the outside note is not in the store, the subsequent in-vault note
    still imports, and the result records a refusal naming the escaping file.
- Existing `import_continues_past_unreadable_file` stays green (broken
  symlink yields the same single unreadable-file error, not a boundary error).

---

## 5. Acceptance

- The four new evals are red at the baseline commit and green after the fix.
- All existing `vault_ld` tests stay green, including #296 preservation, #297
  export symlink refusal, #284 caps/unreadable entries, and stub minting.
- `cargo test --bin neurostrata-mcp vault_ld` (or the equivalent module
  filter) passes; the final branch runs the daemon bin suite.
- No public API/CLI/HTTP contract change; import result `errors` now carries
  the new refusal/unresolved entries.

## 6. Doc gate

`src/vault_ld/*` is not a watched doc surface. No co-change is required; the
operator-facing behavior is already described in
`docs/features-api/ops/obsidian.md` and is unchanged in shape (refusals are
reported through the existing `errors` channel).
