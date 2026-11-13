# Post-panel finding register

Baseline: `332441c1` (`cognilogical/main`). Source: panel final report and issues #147–#150.
Every item has one work package, one spec, one acceptance test, and an owner.

| ID | Issue | Severity | Current evidence | Spec | WP | Acceptance |
|---|---|---|---|---|---|---|
| F1 | #147 | P1 | `src/secrets.rs:252-256,441-456`; `src/parser/ingest.rs:603-628,978-985`; `src/ingest_jobs.rs:34-99` | secret-precision | 1.1 | clean source shapes ingest; seeded secrets skip; `files_skipped` reported |
| F2 | #148 | P1 | `src/parser/ingest.rs:797-801,1262-1266,1292-1370`; `src/store/ladybug.rs:7423-7436` | ingest-skip | 1.2 | 5 unchanged ingests with one skipped file keep generation count 1 |
| F3 | #149 | P1 | `src/store/ladybug.rs:4826-4829,4906-4935,4993-5000,6626-6631`; `src/fts/mod.rs:55-75` | lexical-generation | 1.3 | exactly current IDs; deleted/secret rows absent; route parity |
| F4 | #150 | P1-PROC | #142 review `b8c8570c` → merged `05d9de84`; #127 review `1299d022` → merged `d2eb1d4c`; gates live | hygiene-repair + lexical-generation | 1.3/1.4/0.3 | F1/F5/F2 and G1/G3/G4/G5 fixed or ruled; new SHA re-review; receipt gate |
| F5 | #150 | P2 | `src/secrets.rs:358-366,621-646` | secret-precision | 1.1 | `<ghp_…>`/`${ghp_…}` rejected |
| F6 | #150 | P2 | `src/server.rs:4211-4218,4317-4318`; `src/store/ladybug.rs:4366` | hygiene-repair | 1.4 | partial counts; watermark unchanged on Err |
| F7 | #150 | P2 | `src/parser/ingest.rs:1129-1135,1162,1292,1348` | ingest-skip | 2.3 | transient summary forces reparse |
| F8 | #150 | P2 | `src/server.rs:7798-7816`; `src/rebac.rs:168-208` | ownership | 2.2 | set_metadata requires write auth; owner parity |
| F9 | #150 | P2 | `src/store/ladybug.rs:4706`; `src/fts/mod.rs:625-646`; `src/rebac.rs:168-208` | lexical-generation | 1.3 | both lexical modes use canonical owner |
| F10 | #150 | P2 | `src/parser/generation.rs:113`; `src/storage/snapshot.rs:32-37`; `src/store/ladybug.rs:2013-2025` | upgrade-preflight | 0.5 | low-space preflight; 2× sizing; upgrade docs |

Known non-findings (do not re-litigate): PAR-01 sentinel, PAR-02 hydration predicates, PAR-03 chunk overlap, carry abort, sidecar no-cure paths, Cypher escaping, buffer-pool precedence, arena env parsing.

Process record: `main` is governed by ruleset `secure-rule-set`; it requires 1 approval but admin/roles/integrations bypass `always`, thread resolution is off, and no status checks are required. #127/#142 were merged 13 minutes after gating reviews at the same SHA.
