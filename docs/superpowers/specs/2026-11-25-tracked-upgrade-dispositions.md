# 2026-11-25 — Tracked Upgrade Dispositions (#339)

> **Baseline**: `cognilogical/main` @ `951e165e` > **Issue**: #339 — record the upgrades this repository deliberately has not
> taken, and pin the current state with an offline drift gate.
> **Gate**: `tests/dependency_tracked_upgrades.rs` (`tracked-upgrades` eval suite)
> **Status**: two items are deferred upstream; one is deferred to the next LTS;
> one already landed in #340.

---

## 1. Scope

Four upgrades were surfaced while closing the dependency and CI advisories.
They are not defects today, but each would silently rot without a recorded
decision and a check that fails when the surrounding state moves:

| Item              | Current state                                                         | Disposition                      | Owner surface                                   |
| ----------------- | --------------------------------------------------------------------- | -------------------------------- | ----------------------------------------------- |
| `oxiri`           | root pin `0.2`, `spareval 0.2.9` edge `0.2.11`                        | **Blocked** by `spareval 0.2.9`  | `Cargo.toml`, `Cargo.lock`                      |
| `Dockerfile.base` | `ubuntu:24.04` + `libicu74`                                           | **Deferred** to Ubuntu 26.04 LTS | `Dockerfile.base`                               |
| `TypeScript`      | `~6.0.3` in all 8 manifests                                           | **Deferred** to TypeScript 7.1   | `plugins/*/package.json`, `web-ui/package.json` |
| artifact actions  | upload `v7.0.2`, download `v8.0.2`, checkout `v7.0.1`, all SHA-pinned | **Landed** in #340               | `.github/workflows/*.yml`                       |

The gate is intentionally offline: it reads committed files only, performs no
network I/O, and runs no package manager. That keeps the decision reproducible
in CI and in an air-gapped worktree.

---

## 2. `oxiri 0.3` — blocked by `spareval 0.2.9`

`Cargo.toml` pins the root `oxiri` edge at `0.2`, which resolves to
`oxiri 0.2.11`. `spareval 0.2.9` — the SPARQL evaluator on the same SPARQL 1.2
feature set — declares its own `oxiri 0.2.11` edge in `Cargo.lock`. Moving the
root pin to `oxiri 0.3` without a matching `spareval` release would put two
incompatible `oxiri` bands in one graph and force a duplicate-version split
across `oxrdf`/`spargebra`/`sparesults`.

`oxiri 0.3.1` is already present in the lockfile, but only through the separate
`rudof_*` graph; it is not reachable from `spareval` or the root pin.

**Unblock trigger (all must hold):**

1. A `spareval` release publishes a dependency edge on the `oxiri 0.3` band
   (and the matching `oxrdf`/`spargebra`/`sparesults` line).
2. `cargo update -p oxiri` resolves one band for both the root and `spareval`
   with no duplicate `oxiri` package in the final graph.
3. The `oxiri_pin_matches_the_spareval_dependency_edge` test is updated in the
   same change as the root pin and passes.

**Evidence commands:**

```bash
grep -n 'oxiri' Cargo.toml
cargo tree -i oxiri@0.2.11
cargo tree -i oxiri@0.3.1
sed -n '/name = "spareval"/,/^$/p' Cargo.lock
```

`cargo test --locked --test dependency_tracked_upgrades -- --test-threads=1`
fails with a band-mismatch message if `spareval` moves to `0.3` while the root
pin stays on `0.2`, so the upgrade cannot land half-done.

---

## 3. `Dockerfile.base` — deferred to Ubuntu 26.04 LTS

`Dockerfile.base` builds on `ubuntu:24.04` and installs `libicu74`, the ICU
package that release ships. Jumping to `ubuntu:26.04` before its LTS point
release would move the base image, the ICU package name, and the runner image
in one step, with no current consumer forcing the change.

The file has **no CI consumer**: no workflow and no script references
`Dockerfile.base`. `Dockerfile.build` is the build image referenced by
`scripts/build.sh`, and it is a separate file. Keeping the base image unused
means the only cost of waiting is periodic drift, which this record owns.

**Unblock trigger (all must hold):**

1. Ubuntu 26.04 reaches its LTS release and the runner images used by
   `.github/workflows/*.yml` are available on that base.
2. The ICU package name for 26.04 is confirmed (the `libicu74` → next-package
   rename is explicit in the diff, not implied).
3. `Dockerfile.base` still has zero CI consumers, or a consumer is added
   deliberately in the same change.
4. The `dockerfile_base_*` tests are updated together with the base image.

**Evidence commands:**

```bash
grep -n -E 'FROM|libicu' Dockerfile.base
grep -rn 'Dockerfile.base' .github/workflows scripts || echo 'no CI consumer'
grep -n 'Dockerfile' scripts/build.sh
```

---

## 4. `TypeScript ~6.0.3` — deferred to 7.1

All eight TypeScript manifests pin `~6.0.3`:

- `plugins/core/package.json`
- `plugins/claude/package.json`
- `plugins/codex/package.json`
- `plugins/hermes/package.json`
- `plugins/opencode/package.json`
- `plugins/github-cli/package.json`
- `plugins/openclaw/package.json`
- `web-ui/package.json`

A partial bump is the failure mode this gate prevents: one manifest on
TypeScript 7 while the others stay on 6 produces inconsistent plugin builds and
lets the web UI and the plugin SDK disagree about the compiler. TypeScript 7.1
is the target because it is the first 7.x line the toolchain will adopt; 7.0 is
deliberately skipped rather than tracked as an intermediate step.

**Current blocker status:** adopting TypeScript 7 is blocked today. The
unblock trigger below is not met: it requires TypeScript 7.1 to be released
and the pinned `typescript-eslint`/`vite`/plugin build toolchain to support
it, and this record does not record either condition as satisfied. Until both
hold, there is no supported path to 7.x and all eight manifests stay on
`~6.0.3`.

**Unblock trigger (all must hold):**

1. TypeScript 7.1 is released and the pinned `typescript-eslint`/`vite`/plugin
   build toolchain supports it.
2. All eight manifests move to the same 7.1 range in one change; no manifest
   is left on `~6.0.3` and none selects `7.x` alone.
3. `npm run build` (or the per-plugin equivalent) passes for the web UI and
   every plugin package with the new compiler.
4. The `typescript_pins_are_6_0_3_across_plugins_and_web_ui` test is updated
   in the same change.

**Evidence commands:**

```bash
for f in plugins/*/package.json web-ui/package.json; do
  printf '%s: ' "$f"
  jq -r '.devDependencies.typescript // .dependencies.typescript' "$f"
done
```

---

## 5. Artifact actions — landed in #340

`#340` (`3d2d3a68`, "chore(ci): bump artifact actions to upload v7.0.2 and
download v8.0.2") moved every artifact and checkout reference in
`.github/workflows/*.yml` to full 40-character commit SHAs:

| Action                      | Version  | SHA                                        |
| --------------------------- | -------- | ------------------------------------------ |
| `actions/upload-artifact`   | `v7.0.2` | `cf430e030ddbb5b0abf93d22962f4752f3646cd9` |
| `actions/download-artifact` | `v8.0.2` | `9000827ccba6bdab643e8b6fd33ac0654aef8333` |
| `actions/checkout`          | `v7.0.1` | `3d3c42e5aac5ba805825da76410c181273ba90b1` |

This item is **closed**; the gate keeps it closed. Floating tags are rejected,
because a moved tag would silently change what CI executes. The inline
`# vN.N.N` comment is part of the contract: a SHA alone cannot tell a reviewer
which release it corresponds to.

**Maintenance trigger:** any Dependabot or manual bump must update the SHA and
the version comment together. `upload-artifact` stays on the v7 line and
`download-artifact` on the v8 line until a deliberate major migration change
updates this record and the test.

**Evidence commands:**

```bash
grep -rn -E 'uses: actions/(upload-artifact|download-artifact|checkout)@' .github/workflows
git show --stat 3d2d3a68
```

---

## 6. Gate and eval registration

The offline gate lives in `tests/dependency_tracked_upgrades.rs` and is
registered as the `tracked-upgrades` suite in `eval/registry.json` with
`thresholds: { "ignored": 0 }`.

```bash
cargo test --locked --test dependency_tracked_upgrades -- --test-threads=1
bash scripts/remediation-eval.sh tracked-upgrades --json /var/tmp/eval-339-tracked-upgrades.json
```

Revisit this record whenever a trigger fires; do not widen a pin or move a base
image without updating the gate in the same change.
