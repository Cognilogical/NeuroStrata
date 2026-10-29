# DID / Signed Mesh Records Design

## Goal
Optionally sign mesh records with Ed25519 and verify on import, storing signer DID. Backward-compatible with unsigned records.

## Architecture
- Module: `src/did.rs` (in-house HMAC-SHA256 pseudo-signing placeholder; upgrade to Ed25519 when crate available).
- `signer_id(seed)`, `sign(record,key)`, `verify(record,key,sig)`.

## Constraints
- Unsigned records remain valid.
- No blocking of import on missing signature (warn only).

## Testing
- Round-trip sign/verify.
- DID prefix.