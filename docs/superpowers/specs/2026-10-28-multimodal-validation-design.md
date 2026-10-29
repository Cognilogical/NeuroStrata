# Multimodal Validation Design

> Status: draft — pending Argon review.
> Task: `neurostrata-y062` (multimodal validation, OxiRS §1.18 content track).
> Companion: `docs/superpowers/specs/2026-10-27-shacl-shape-validation-design.md`, `docs/superpowers/specs/2026-10-27-neurostrata-rdf-semantic-layer-design.md`.

## 1. Problem

`src/multimodal/mod.rs` is a stub: `embed_text` is a byte-to-f32 passthrough, `validate_mime` is a hard-coded 4-type allowlist with no size/digest checks, and `describe` only reports byte length. NeuroStrata is text-first, but when memory payloads carry attachments (images/audio/documents), there is no validation of MIME type, size, digest, or dimension constraints, and no projection into RDF/SHACL. This invites unbounded payloads and unsafe content types.

## 2. Goal

Add an **in-house multimodal attachment validator** that:

- validates attachment MIME types against a configurable allowlist,
- enforces size and dimension caps,
- verifies content digests (sha256),
- validates image dimensions (width/height) without decoding full image (header-only parsing for PNG/JPEG/GIF/WebP),
- projects attachment metadata into RDF as typed predicates,
- is deterministic and LLM-free.

Core memory remains text-first; attachments are optional metadata.

## 3. Constraints

- No new crate dependencies for image parsing (header-only, in-house).
- No LLM.
- LadybugDB remains sole store; attachments are stored as metadata, not blobs (content stored by reference/URI).
- Deterministic validation: same bytes → same result.
- Privacy: digests only, never raw bytes in logs/export.
- Warn-first: invalid attachments produce warnings, not hard failures, unless `multimodal_strict = true`.

## 4. Scope

In scope:

- `Attachment` struct in `metadata.attachments`.
- MIME allowlist (configurable).
- Size cap (bytes).
- sha256 digest verification.
- Header-only dimension parsing for PNG, JPEG, GIF, WebP.
- Write-time validation hook in `upsert`.
- SHACL shape for `metadata.attachments`.
- RDF projection of attachment metadata.

Out of scope:

- Embedding multimodal content (no CLIP).
- Storing binary blobs in LadybugDB.
- Transcoding/conversion.

## 5. Approach

### 5.1 Data model (`metadata.attachments`)

```json
"attachments": [
  {
    "id": "att-<uuid>",
    "mime": "image/png",
    "size_bytes": 12345,
    "digest": "sha256:<hex>",
    "width": 640,
    "height": 480,
    "uri": "file:///... or https://...",
    "alt_text": "optional description"
  }
]
```

- `id` required, unique within memory.
- `mime` required.
- `size_bytes` required, `> 0`.
- `digest` required, format `sha256:<64 hex>`.
- `uri` required.
- `width`/`height` optional (required for image MIMEs when `multimodal_require_dimensions = true`).
- `alt_text` optional, max 500 chars.

### 5.2 MIME allowlist

Default allowlist (configurable via `multimodal_allowed_mimes: Vec<String>`):

- `text/plain`, `text/markdown`, `text/csv`
- `image/png`, `image/jpeg`, `image/gif`, `image/webp`
- `audio/wav`, `audio/mpeg`, `audio/ogg`
- `application/pdf`

Reject unknown MIMEs with `MultimodalWarning::UnsupportedMime`.

### 5.3 Size caps

- `multimodal_max_attachment_bytes: usize` (default 10 MiB).
- `multimodal_max_total_bytes: usize` (default 50 MiB per memory).
- `multimodal_max_attachments: usize` (default 20).
- `multimodal_max_width: u32` (default 8192).
- `multimodal_max_height: u32` (default 8192).

### 5.4 Digest verification

- `verify_digest(bytes: &[u8], expected: &str) -> bool` using `sha2` (already a dependency).
- At write time, if `multimodal_verify_digests = true` (default true) and bytes are available (via `uri` resolution), verify; if URI unresolvable, warn `DigestUnverifiable` (not failure).
- Resolver must read at most `multimodal_max_attachment_bytes + 1` bytes and fail if exceeded; verify `bytes.len() == att.size_bytes` else `ByteLengthMismatch`.
- Digest mismatch → `MultimodalWarning::DigestMismatch` (failure if strict).

### 5.5 Dimension parsing (header-only)

Implement `parse_dimensions(mime, bytes) -> Option<(u32,u32)>`:

- PNG: read IHDR chunk (bytes 16..24) → width/height (big-endian u32).
- JPEG: scan for SOF0/SOF2 markers (0xFFC0/0xFFC2), parse dimensions.
- GIF: logical screen descriptor (bytes 6..10) → width/height (little-endian u16).
- WebP: RIFF header + VP8/VP8L/VP8X chunk → dimensions.

No full decode; if parsing fails, return `None` (warn `DimensionsUnparseable`).
- If declared `width`/`height` present and parsed dimensions differ, emit `DimensionsMismatch`.
- If parsed dimensions exceed `multimodal_max_width`/`multimodal_max_height`, emit `DimensionsExceeded`.
- If image MIME requires dimensions and none declared/parsed, emit `DimensionsMissing` (failure if strict + `multimodal_require_dimensions`).

### 5.6 Validation (`src/multimodal/mod.rs`)

```rust
pub fn validate_attachment(att: &Attachment, config: &MultimodalConfig, bytes: Option<&[u8]>) -> Vec<MultimodalWarning>
pub fn validate_attachments(atts: &[Attachment], config, bytes_resolver) -> Vec<MultimodalWarning>
```

Warnings:

- `EmptyId`
- `EmptyUri`
- `UnsupportedMime { mime }`
- `SizeExceeded { size, cap }`
- `SizeZero`
- `TotalSizeExceeded { total, cap }`
- `CountExceeded { count, cap }`
- `DigestMalformed { digest }`
- `DigestMismatch { expected, actual }`
- `DigestUnverifiable { uri }`
- `DimensionsMissing { mime }`
- `DimensionsUnparseable { mime }`
- `DimensionsExceeded { width, height, max_width, max_height }`
- `DimensionsMismatch { declared: (u32,u32), parsed: (u32,u32) }`
- `AltTextTooLong { len, cap }`
- `DuplicateId { id }`
- `AttachmentsNotArray`
- `ByteLengthMismatch { declared, actual }`

### 5.7 Write-time hook

In `LadybugStore::upsert`:

- If `metadata.attachments` present, call `validate_attachments`.
- Append warnings to `UpsertReport.warnings` as `WriteShapeWarning` (consistent with SHACL).
- If `multimodal_strict = true` and any warning is a failure (`DigestMalformed`, `DigestMismatch`, `UnsupportedMime`, `SizeExceeded`, `SizeZero`, `TotalSizeExceeded`, `CountExceeded`, `DuplicateId`, `DimensionsExceeded`, `DimensionsMismatch`, `AltTextTooLong`, `EmptyId`, `EmptyUri`, `AttachmentsNotArray`, `ByteLengthMismatch`), reject the upsert. Since `VectorStore::upsert` returns `anyhow::Result<UpsertReport>` (no `UpsertError` enum), strict failure returns `anyhow::Error` with message `"ERROR [MULTIMODAL]: ..."` and aborts the upsert before commit (LadybugDB transaction rolled back). Non-strict mode appends warnings to `UpsertReport.warnings` (extended with `warnings: Vec<WriteShapeWarning>` per SHACL spec §5.3).

### 5.8 SHACL shape

Add `multimodal-shapes.v1.ttl`:

- `metadata.attachments` is an array of objects.
- Each object: `id` (string, required), `mime` (string, required, in allowlist via `sh:in`), `size_bytes` (integer, min 1), `digest` (string, pattern `^sha256:[0-9a-f]{64}$`), `uri` (string, required), `width`/`height` (integer, min 1), `alt_text` (string, max 500).

### 5.9 RDF projection

Project per attachment as indexed predicates (XSD datatypes):

- `ns:attachment.<i>.id` (xsd:string)
- `ns:attachment.<i>.mime` (xsd:string)
- `ns:attachment.<i>.sizeBytes` (xsd:integer)
- `ns:attachment.<i>.digest` (xsd:string)
- `ns:attachment.<i>.width` (xsd:integer, if present)
- `ns:attachment.<i>.height` (xsd:integer, if present)
- `ns:attachment.<i>.uri` (xsd:string)
- `ns:attachment.<i>.altText` (xsd:string, truncated to 500 chars)
- `ns:attachmentCount` (xsd:integer).

Privacy: `uri` may be a local file path. In `project_namespace_rdf` and federated export, attachment predicates are excluded unless `eligibility: "shareable"` AND `visible_to` includes `"*"` or the peer principal (ReBAC). Raw `metadata.attachments` JSON is never projected as an opaque literal (`metadata` is projected per-predicate per SHACL spec §5.1a). SynapticMesh publish strips attachment predicates entirely unless `eligibility: "shareable"` and `multimodal_share_attachments = true` (default false).

### 5.10 URI resolution

- `bytes_resolver: Option<&dyn Fn(&str) -> Option<Vec<u8>>>` provided by caller.
- Daemon provides a resolver for `file://` URIs canonicalized against project root + `Config::allowed_external_dirs` (reuse existing boundary; do not add `multimodal_allowed_roots`). Symlink escapes are rejected. Reads are capped at `multimodal_max_attachment_bytes + 1` and fail if exceeded.
- Remote HTTP fetch is NOT supported in core (drop `multimodal_fetch_remote`); callers must supply bytes out-of-band. This avoids SSRF and blocking Tokio.
- If no resolver, digest verification is skipped with `DigestUnverifiable`.

## 6. Interfaces

- `src/multimodal/mod.rs`: `Attachment`, `MultimodalConfig`, `MultimodalWarning`, `validate_attachment`, `validate_attachments`, `parse_dimensions`, `verify_digest`.
- `LadybugStore::upsert`: validation hook.
- SHACL: `src/schemas/shacl/multimodal-shapes.v1.ttl`.
- RDF: projection predicates (§5.9).
- Config: `multimodal_strict`, `multimodal_allowed_mimes`, `multimodal_max_attachment_bytes`, `multimodal_max_total_bytes`, `multimodal_max_attachments`, `multimodal_max_width`, `multimodal_max_height`, `multimodal_verify_digests`, `multimodal_require_dimensions`, `multimodal_share_attachments` (default false). (No `multimodal_allowed_roots` or `multimodal_fetch_remote`; reuse `Config::allowed_external_dirs`.)

## 7. Edge cases

- Empty attachments: no warnings.
- Unknown MIME: warning (failure if strict).
- Oversized: warning (failure if strict).
- Digest mismatch: failure if strict.
- URI unresolvable: warn `DigestUnverifiable`, never failure.
- Dimension parse failure: warn, never failure (unless strict + require_dimensions).
- Duplicate id: failure if strict.
- Privacy: federated export excludes attachment predicates unless shareable.

## 8. Testing

- Unit: MIME allowlist.
- Unit: size caps.
- Unit: digest verification (match/mismatch/malformed).
- Unit: dimension parsing for PNG/JPEG/GIF/WebP (header-only).
- Unit: duplicate id detection.
- Integration: upsert with invalid attachment produces warnings (strict rejects).
- Integration: SHACL validates attachment shape.
- Integration: RDF projection emits `ns:attachment.*` predicates.

## 9. Acceptance

Multimodal attachment validation available; MIME/size/digest/dimension checks; SHACL shape; RDF projection; warn-first (strict optional); no LLM; no new deps.

## 10. Files touched (expected)

- `src/multimodal/mod.rs`: full implementation. Legacy `embed_text`, `validate_mime`, `describe` are removed (replaced by `validate_attachment(s)`); any callers updated.
- `src/rdf/shacl.rs`: extend `validate_memory` with attachment shape checks (no `.ttl` loader; shapes are Rust predicates, consistent with existing module).
- `src/store/ladybug.rs`: upsert validation hook.
- `src/rdf/projection.rs`: attachment predicates (§5.9).
- `src/schemas/shacl/multimodal-shapes.v1.ttl`: optional documentation only; runtime validation is in `src/rdf/shacl.rs`.
- `Cargo.toml`: no change (sha2 already present).