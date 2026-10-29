# DID / Signed Mesh Records Design

> Status: revised per Argon review — pending re-review.
> Task: `neurostrata-39v2` (DID / signed mesh records, OxiRS §1.12).
> Companion: `docs/superpowers/specs/2026-10-10-synaptic-mesh-design.md`, `docs/superpowers/specs/2026-10-27-rdf-canonical-memory-data-model-migration-design.md`.

## 1. Problem

`src/did.rs` is an in-house HMAC-SHA256 pseudo-signing placeholder (not real asymmetric crypto). Mesh records (`synaptic-mesh-record/v1`) carry no signature, signer, or public key, so import cannot verify provenance. `import.rs::mesh_provenance` hardcodes `created_by_verified: false`. The OxiRS §1.12 spec requires signed mesh records; this spec replaces the placeholder with real Ed25519 and integrates signing into the SynapticMesh publish/import paths.

## 2. Goal

Add **optional Ed25519 signing and verification of mesh records**:

- Sign mesh records on publish; verify on import.
- Store signer DID and signature in the record (backward-compatible with unsigned records).
- Update `mesh_provenance` (`created_by_verified`) based on verification.
- No blocking import on missing signature (warn only); invalid/corrupt signatures or signer mismatch are rejected into `report.ignored` with a structured reason (never marked verified).
- Single store; no LLM.

## 3. Constraints

- LadybugDB remains the sole store.
- Unsigned records remain valid (backward compatible).
- Missing signature: warn only, import proceeds, `created_by_verified: false`.
- Invalid/corrupt signature or signer mismatch: reject into `report.ignored` with structured reason; never mark verified.
- No LLM.
- Key material never committed to the repo; loaded from a configured path outside the repo.
- Honesty surface: `ApplyReport` / `format_apply_report` report verification counts and warnings deterministically.

## 4. Scope

In scope:
- Real Ed25519 signing/verification via the existing `openssl` dependency (`openssl::pkey::PKey::ed25519`, `openssl::sign::Signer`, `openssl::verify::Verifier`) — no new crate.
- DID method: `did:key` encoding the Ed25519 public key (multicodec + multibase, self-resolving; no registry needed).
- `MeshRecord` schema additions: `signature: Option<String>` (base64), `signer_did: Option<String>`, `public_key: Option<String>` (base64), all `#[serde(default, skip_serializing_if = "Option::is_none")]`.
- Canonical signing payload definition.
- Key management and signing flow in `worker.rs`.
- Verification flow and `mesh_provenance` update in `import.rs`.
- `ApplyReport` / `format_apply_report` reporting.

Out of scope:
- Key rotation / revocation (future).
- DID resolution beyond `did:key`.

## 5. Approach

### 5.1 Cryptography (`src/did.rs`)
Replace the HMAC pseudo-signing with Ed25519 using the existing `openssl` crate (no new dependency):
- `generate_keypair() -> (PKey<Private>, String)` uses `PKey::generate_ed25519()` and returns the private key handle plus its `did:key` string.
- `signer_did(pubkey: &[u8]) -> String` encodes the public key as `did:key:z6Mk...` (multicodec prefix `[0xed, 0x01]` + multibase base58btc). Base58btc is implemented as a small self-contained encoder in `src/did.rs` (no `bs58` crate; `Cargo.toml` has none).
- `sign(payload: &[u8], privkey: &PKey<Private>) -> Vec<u8>` uses `Signer::new_without_digest(privkey)` then `sign_oneshot`.
- `verify(payload: &[u8], pubkey: &[u8], sig: &[u8]) -> bool` uses `Verifier::new_without_digest(pubkey_pkey)` then `verify_oneshot`.
- Base64 encoding/decoding of signatures and public keys is implemented as a small self-contained encoder in `src/did.rs` (no `base64` crate; `Cargo.toml` has none).
- `openssl` is currently target-gated in `Cargo.toml` under `[target.'cfg(not(target_vendor = "apple"))'.dependencies]`. Move `openssl` to unconditional `[dependencies]` (or specify the Apple-target fallback) so Ed25519 signing works on all targets. Document the change in §10.
- No shared-secret HMAC; asymmetric only.

### 5.2 Canonical signing payload
The signature covers a deterministic byte representation of the record **excluding only** `signature`. Canonical bytes = `serde_json::to_vec` of a struct containing all `MeshRecord` fields except `signature`: `(schema_version, id, namespace, memory_type, lifecycle, content, metadata, rdf, relationships, update_id, origin_mesh_epoch, trust, signer_did, public_key)`, with sorted JSON object keys (via `serde_json::to_vec` with `BTreeMap`-ordered fields or a canonical serializer). The `signature` field itself is excluded from the signed payload. This prevents replay across namespaces, type mutation, and `rdf` tampering.

### 5.3 `MeshRecord` schema (`src/synaptic_mesh/record.rs`)
Add to `MeshRecord`:
```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub signature: Option<String>,      // base64 Ed25519 signature
#[serde(default, skip_serializing_if = "Option::is_none")]
pub signer_did: Option<String>,     // did:key:...
#[serde(default, skip_serializing_if = "Option::is_none")]
pub public_key: Option<String>,     // base64 Ed25519 public key
```
All v1 records without these fields remain valid.

### 5.4 Key management
- Private key stored at `<db_path_parent>/mesh_keys/<instance>.key` (PEM/PKCS8), mode 0600, outside the repo, never committed (`.gitignore`).
- Public key / DID published in `MeshManifest` (`manifest.json`) under `signers: Vec<SignerEntry { did, public_key }>` with `#[serde(default, skip_serializing_if = "Vec::is_empty")]` so existing manifests without a `signers` key remain valid.
- Config: `mesh_signing_key_path: Option<PathBuf>` (default `<db_path_parent>/mesh_keys/default.key`); if absent, signing disabled (warn once).

### 5.5 Publish path (`src/synaptic_mesh/worker.rs`)
- In `build_record` / `drain_once`: if a signing key is configured, compute canonical payload bytes, sign with Ed25519, set `signature`, `signer_did`, `public_key` on the record before writing to outbox.
- If signing key absent, record is unsigned (no fields set); no error.
- Signing failure (key load error) → log warning, publish unsigned (warn-first).

### 5.6 Import path (`src/synaptic_mesh/import.rs`)
- On import, for each record:
  - If `signature` is `None`: proceed (unsigned), `created_by_verified: false`, no warning beyond "unsigned".
  - If `signature` present:
    - Resolve signer public key: from `record.public_key` (verify it matches `signer_did` via `did:key` decoding) or from `MeshManifest.signers`.
    - Identity-binding rule: `created_by_verified` is set to `true` **only if** `signer_did == record.lifecycle.created_by` (or, when `MeshManifest.signers` is non-empty, `signer_did` is listed in `MeshManifest.signers` AND equals `record.lifecycle.created_by`). Arbitrary self-generated keypairs that do not match `lifecycle.created_by` are treated as `signer_mismatch` and never marked verified.
    - Verify Ed25519 signature over canonical payload bytes.
    - If valid and identity-bound: set `mesh_provenance.created_by_verified = true`, store `signer_did` as `verified_signer_did`.
    - If invalid/corrupt/mismatch/identity-unbound: reject into `report.ignored` with structured reason `"invalid_signature"`, `"signer_mismatch"`, or `"identity_unbound"`; never mark verified; do not apply row.
- `mesh_provenance` (line ~968) updated: `created_by_verified` reflects verification result; `claimed_created_by` retained for audit.

### 5.7 Reporting (`ApplyReport` / `format_apply_report`)
- Add fields to `ApplyReport`: `signed_verified: usize`, `signed_invalid: usize`, `unsigned: usize`.
- `format_apply_report` (line ~234) emits deterministic lines:
  - `signed verified: {n}`
  - `signed invalid: {n}` (with per-record reasons in verbose mode)
  - `unsigned: {n}`
- Honesty: explicit-zero status lines when counts are 0.

### 5.8 RDF canonical integration
- RDF canonical spec §5.2a mesh wire profile includes `ns:signature`, `ns:signerDid`, `ns:publicKey` as optional typed literals (base64 strings) in `mesh-record-shapes.v1.ttl` (optional, not required).

## 6. Interfaces

- `src/did.rs`: `generate_keypair`, `signer_did`, `sign`, `verify`.
- `src/synaptic_mesh/record.rs`: `signature`, `signer_did`, `public_key` fields.
- `src/synaptic_mesh/worker.rs`: signing flow in `build_record`/`drain_once`.
- `src/synaptic_mesh/import.rs`: verification flow; `mesh_provenance` update; `ApplyReport` fields.
- `MeshManifest`: `signers: Vec<SignerEntry { did, public_key }>`.
- Config: `mesh_signing_key_path: Option<PathBuf>`.

## 7. Edge cases

- Unsigned record: valid, `created_by_verified: false`, no warning beyond "unsigned".
- Valid signature: `created_by_verified: true`.
- Invalid signature: rejected into `report.ignored` with reason; not applied.
- Signer mismatch (public key doesn't match `signer_did`): rejected.
- Missing public key (can't resolve): treated as invalid signature; rejected.
- Signing key absent: records published unsigned; warn once at daemon start.
- Key load failure: warn, publish unsigned.

## 8. Testing

- Unit: Ed25519 sign/verify round-trip.
- Unit: `did:key` encode/decode public key.
- Unit: canonical payload excludes signature field.
- Integration: publish signed record → import → `created_by_verified: true`.
- Integration: tampered record → rejected into `report.ignored`.
- Integration: unsigned record → imported, `created_by_verified: false`.
- Reporting: `ApplyReport` counts correct; explicit zeros.

## 9. Acceptance

Mesh records can be signed with Ed25519 on publish and verified on import; signer DID stored; `mesh_provenance.created_by_verified` reflects verification; invalid signatures rejected (not marked verified); unsigned records remain valid; `ApplyReport` reports counts with explicit zeros; no HMAC placeholder remains; no new crate beyond existing `openssl`.

## 10. Files touched (expected)

- `src/did.rs`: replace HMAC with Ed25519.
- `src/synaptic_mesh/record.rs`: add `signature`, `signer_did`, `public_key`.
- `src/synaptic_mesh/worker.rs`: signing flow.
- `src/synaptic_mesh/import.rs`: verification flow, `mesh_provenance` update, `ApplyReport` fields.
- `src/synaptic_mesh/manifest.rs`: `signers` list.
- `.gitignore`: `mesh_keys/`.
- `Cargo.toml`: move `openssl` from target-gated to unconditional `[dependencies]` (Apple fallback documented); no new crates (`base64`/`bs58` implemented in-house).