# Building neurostrata-mcp

cargo builds this, but not on its own: `lbug` (LadybugDB) compiles a C++ graph
engine from vendored source, so a build also needs CMake and a C++20 toolchain.
Both surface minutes in, as errors naming neither cargo nor NeuroStrata.

That is what the scripts in `scripts/` are for. They locate the toolchain, check
the versions this build is sensitive to, and hand off to cargo. They install
nothing.

| Route | Command |
| --- | --- |
| Linux / macOS | `scripts/build.sh` |
| Windows | `powershell -ExecutionPolicy Bypass -File scripts\build.ps1` |
| Container, no host toolchain | see [Dockerfile.build](../Dockerfile.build) |

`--check` / `-CheckOnly` reports the resolved toolchain and builds nothing.
Output lands in `target/release/` (`.exe` on Windows).

---

## What a build needs

**CMake 3.15 or newer**, the highest `cmake_minimum_required` in the vendored
tree. CMake 4 works: the one sub-3.5 declaration is in a re2 branch guarded by
`BUILD_SHARED_LIBS`, which is off here, so it is never evaluated. Built against
4.3.1 with MSVC and 4.4.0 with Clang, both with no policy override.

**A C++20 standard library with `<format>`.** `common/assert.h` includes it.
This is a toolset-version floor rather than a language-level one — the compilers
below all accept `-std=c++20`:

| | works | fails |
| --- | --- | --- |
| GCC | 13+ | 12 — `fatal error: format: No such file or directory` |
| MSVC | 14.40+ | 14.37 — `C1001` internal compiler error ~848 files in |
| Clang | Apple clang on macOS 15, arm64 and x64 | — |

`build.sh` compiles a probe instead of parsing `--version`, since the gap is in
the standard library rather than the compiler. The MSVC failure is a front-end
ICE in `PackExpander.cpp`, so a lower optimisation level does not avoid it.

**A generator.** Make or Ninja on Linux and macOS; on Windows, Ninja
specifically — see the quirks below.

**perl and pkg-config, on Linux only.** `openssl` is vendored there and its
`Configure` is perl. `Cargo.toml` gates the dependency out on Windows and Apple
targets, which have a system TLS stack.

Rust itself is often installed but off `PATH`, so both scripts check
`CARGO_HOME` and `~/.cargo/bin` before reporting cargo missing.
`NEUROSTRATA_CMAKE` and `NEUROSTRATA_NINJA` replace discovery for a tool and are
authoritative: an unusable one is an error rather than a fallback.

---

## Toolchain versions

Tested here:

| Component | Good | Bad |
| --- | --- | --- |
| Rust | 1.98.0 | — |
| CMake | 3.28.3, 3.31.12, 4.3.1, 4.4.0 | — |
| GCC | 13.3, 14 | 12.4 |
| MSVC | 14.44.35207, 14.51.36231 | 14.37 |
| Ninja | 1.11.1, 1.13.2 | — |
| Clang | Apple clang, macOS 15 arm64 and x64 | — |

The Good column is what CI builds on every push and pull request, across four
runners. The Bad column comes from local machines — CI has never been handed a
GCC 12 or an MSVC 14.37 to fail on.

The container route needs none of this on the host: `Dockerfile.build` pins
Debian trixie (GCC 14, CMake 3.31) and proves `<format>` at image build time. If
a container build dies with `unexpected EOF`, that is usually the VM running out
of memory; the file's header covers that and the named-volume setup.

---

## CI

| Workflow | Trigger | Publishes |
| --- | --- | --- |
| `ci.yml` | push, PR | nothing — a `--release --locked` build on linux, windows, and macOS arm64 and x64, plus a version assertion |
| `nightly.yml` | cron, dispatch | linux + windows amd64 → the `nightly` tag, prerelease |
| `release.yml` | `v*` tags | all five targets → that tag |

Both publishing workflows attach `SHA256SUMS`, and they target separate tags;
the nightly archives also carry `nightly` in their filenames.

Cross-compilation is impractical for releases — vendored OpenSSL, `cxx-build`,
and ONNX via `fastembed` — so each target builds on its own native runner.

Timings on GitHub-hosted runners, every run cold — nothing is cached between
them: macOS arm64 ~20 min, linux ~18-24 min, macOS x64 ~28 min, windows ~61 min.
The four run concurrently, so a full CI cycle is bounded by Windows.

---

## Windows quirks

None of this affects a Linux or macOS build.

**Nothing is on `PATH`.** Visual Studio keeps `cl.exe`, CMake and Ninja inside
its own tree, so none of them resolve on an ordinary shell even with the full
C++ workload installed — a Developer Command Prompt is a different environment.
`build.ps1` imports what `vcvars64.bat` produces rather than assembling `PATH`
itself, since `cl.exe` also needs `INCLUDE`, `LIB` and `LIBPATH`.

**CMake comes from Visual Studio.** VS 2026 bundles 4.3.1, and `vcvars64.bat`
puts it on `PATH`, so that is normally the one used — it builds this fine. For
a VS install without the CMake component, `build.ps1` also looks in
`%USERPROFILE%\.local\tools\`, where an extracted zip needs no admin rights and
no `PATH` change. `NEUROSTRATA_CMAKE` overrides both.

**Ninja is required rather than optional**, because the `cmake` crate selects
that generator here. Without it, configuration fails with `CMake was unable to
find a build program corresponding to "Ninja"`. Visual Studio bundles one.

**A debug build does not link.** `cargo build` without `--release` compiles the
vendored C++ with full debug info, and the resulting static `lbug.lib` passes the
4 GiB ceiling for a COFF image:

```
lbug.lib : fatal error LNK1248: image size (1000AA2B0) exceeds maximum allowable size (FFFFFFFF)
```

Release is unaffected — the same tree links to a 44.5 MB binary — so both scripts
and all three workflows build release on Windows.

**`-VcToolsVersion 14.44`** picks a specific toolset when several are installed.

**`C1056: cannot update the time date stamp field`** on a single `.obj` is a
file lock, characteristic of real-time antivirus rather than a toolchain
problem. Ninja resumes on a re-run.
