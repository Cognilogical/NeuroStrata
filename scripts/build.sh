#!/usr/bin/env bash
#
# Build neurostrata-mcp on Linux and macOS.
#
# Cargo cannot build this project on its own. lbug (LadybugDB) compiles a C++
# graph engine from vendored source, so the build needs CMake and a C++20
# toolchain, and on Linux the vendored OpenSSL build needs perl and pkg-config.
# None of that is announced by cargo: each one surfaces minutes in, as an error
# that names neither cargo nor NeuroStrata.
#
# This script installs nothing and does not replace cargo. It checks the
# prerequisites that are known to break the build, then hands off to cargo.
#
# The checks, and why each exists:
#
#   C++20 <format>   GCC 12 fails with "fatal error: format: No such file or
#                    directory" because lbug's common/assert.h includes
#                    <format>. GCC 13+ is proven (14 in the container image).
#                    The check compiles a probe rather than parsing --version,
#                    because "which compiler" and "which standard library" are
#                    separable and it is the library that is missing.
#
#   CMake >= 3.15    the highest cmake_minimum_required in lbug's tree. CMake 4
#                    is fine: the one sub-3.5 declaration sits in a re2 branch
#                    guarded by BUILD_SHARED_LIBS, which is off for this build.
#
#   perl,            Only on Linux, and only because openssl is vendored there.
#   pkg-config       Windows uses SChannel and Apple targets use
#                    Security.framework, so Cargo.toml gates the dependency out
#                    on both and these are not needed.
#
# Usage:
#   scripts/build.sh                 release build
#   scripts/build.sh --check         run the checks, print the toolchain, stop
#   scripts/build.sh --debug         debug build
#   scripts/build.sh --no-locked     allow resolving away from Cargo.lock
#   scripts/build.sh -- --features x anything after -- goes to cargo
#
# Environment:
#   CXX                      C++ compiler to probe and use (default: c++)
#
# There is also a container route that needs no host toolchain at all; see
# Dockerfile.build.

set -euo pipefail

profile="release"
check_only=0
locked="--locked"
cargo_passthrough=()

while [ $# -gt 0 ]; do
    case "$1" in
        --debug)     profile="debug"; shift ;;
        --release)   profile="release"; shift ;;
        --check)     check_only=1; shift ;;
        --no-locked) locked=""; shift ;;
        -h|--help)   sed -n '2,50p' "$0" | sed 's/^#\{1,2\} \{0,1\}//'; exit 0 ;;
        --)          shift; cargo_passthrough+=("$@"); break ;;
        *)           cargo_passthrough+=("$1"); shift ;;
    esac
done

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
os="$(uname -s)"

# Git Bash / MSYS / Cygwin run this file happily but build for the MSVC target,
# where the toolchain lives inside Visual Studio's tree and none of the checks
# below apply. Send those shells to the script that knows how to find it.
case "$os" in
    MINGW*|MSYS*|CYGWIN*)
        printf 'This is a POSIX shell on Windows. Use the Windows build script instead:\n\n' >&2
        printf '    powershell -ExecutionPolicy Bypass -File scripts\\build.ps1\n\n' >&2
        printf 'It locates the MSVC toolchain, CMake and Ninja, which are not on an\n' >&2
        printf 'ordinary Windows PATH even when Visual Studio is installed.\n' >&2
        exit 2
        ;;
esac

if [ -t 1 ]; then
    c_red=$'\033[31m'; c_yellow=$'\033[33m'; c_cyan=$'\033[36m'
    c_green=$'\033[32m'; c_dim=$'\033[90m'; c_off=$'\033[0m'
else
    c_red=""; c_yellow=""; c_cyan=""; c_green=""; c_dim=""; c_off=""
fi

step() { printf '%s==> %s%s\n' "$c_cyan" "$1" "$c_off"; }

# fail <message> [hint...]
fail() {
    printf '\n%sERROR: %s%s\n' "$c_red" "$1" "$c_off" >&2
    shift
    for hint in "$@"; do
        printf '%s       %s%s\n' "$c_yellow" "$hint" "$c_off" >&2
    done
    exit 1
}

# Extracts the first x.y or x.y.z from a tool's --version output.
version_of() {
    "$@" 2>/dev/null | head -n 1 | grep -oE '[0-9]+\.[0-9]+(\.[0-9]+)?' | head -n 1
}

printf '\n%sneurostrata-mcp build (%s)%s\n' "$c_green" "$os" "$c_off"
printf 'repository: %s\n' "$repo_root"

# ---------------------------------------------------------------------------
# 1. cargo
#
# Rust is commonly installed but off PATH, so `command -v` failing is not
# evidence that it is absent. Check the standard homes too.
# ---------------------------------------------------------------------------
step "Locating cargo"

cargo_bin=""
if command -v cargo >/dev/null 2>&1; then
    cargo_bin="$(command -v cargo)"
else
    for home_dir in "${CARGO_HOME:-}" "$HOME/.cargo"; do
        [ -n "$home_dir" ] || continue
        if [ -x "$home_dir/bin/cargo" ]; then
            cargo_bin="$home_dir/bin/cargo"
            PATH="$home_dir/bin:$PATH"
            export PATH
            break
        fi
    done
fi

[ -n "$cargo_bin" ] || fail "cargo was not found." \
    "Install the Rust toolchain from https://rustup.rs." \
    "If Rust is already installed it is simply off PATH: add \$HOME/.cargo/bin to" \
    "PATH, or source \$HOME/.cargo/env, and run this again."

cargo_version="$("$cargo_bin" --version)"

# ---------------------------------------------------------------------------
# 2. CMake
# ---------------------------------------------------------------------------
step "Checking CMake"

command -v cmake >/dev/null 2>&1 || fail "CMake was not found." \
    "lbug (LadybugDB) compiles its C++ engine with CMake. Without it the build" \
    'panics with "is `cmake` not installed?" several minutes in.' \
    "" \
    "  Debian/Ubuntu   sudo apt-get install cmake" \
    "  Fedora          sudo dnf install cmake" \
    "  macOS           brew install cmake"

cmake_version="$(version_of cmake --version)"
[ -n "$cmake_version" ] || fail "cmake is on PATH but did not report a version."

cmake_major="${cmake_version%%.*}"
cmake_minor="$(printf '%s' "$cmake_version" | cut -d. -f2)"

if [ "$cmake_major" -lt 3 ] || { [ "$cmake_major" -eq 3 ] && [ "$cmake_minor" -lt 15 ]; }; then
    fail "CMake $cmake_version is older than 3.15." \
        "3.15 is the highest cmake_minimum_required in lbug's vendored tree."
fi

# ---------------------------------------------------------------------------
# 3. A C++20 standard library with <format>
#
# Compiled, not inferred from a version string. This is the check that catches
# GCC 12, whose failure names a missing header and nothing else.
# ---------------------------------------------------------------------------
step "Checking the C++ toolchain"

cxx="${CXX:-c++}"
command -v "$cxx" >/dev/null 2>&1 || fail "No C++ compiler found (looked for '$cxx')." \
    "  Debian/Ubuntu   sudo apt-get install build-essential" \
    "  Fedora          sudo dnf install gcc-c++" \
    "  macOS           xcode-select --install" \
    "" \
    "Set CXX to point at a specific compiler."

cxx_version="$("$cxx" --version 2>/dev/null | head -n 1)"

probe_dir="$(mktemp -d)"
trap 'rm -rf "$probe_dir"' EXIT
cat > "$probe_dir/probe.cpp" <<'PROBE'
#include <format>
#include <string>
int main() { return std::format("{}", 1) == "1" ? 0 : 1; }
PROBE

if ! "$cxx" -std=c++20 "$probe_dir/probe.cpp" -o "$probe_dir/probe" >"$probe_dir/probe.log" 2>&1 \
   || ! "$probe_dir/probe"; then
    probe_output="$(head -n 6 "$probe_dir/probe.log" 2>/dev/null || true)"
    hints=(
        "lbug's common/assert.h includes <format>, a C++20 header. GCC 12 does not"
        "ship it and fails with \"fatal error: format: No such file or directory\"."
        "GCC 13+ works; the container image uses GCC 14."
        ""
        "  Debian/Ubuntu   sudo apt-get install g++-13   (then CXX=g++-13)"
        "  macOS           a current Xcode command line tools release"
        ""
        "Or build in a container instead, which needs no host toolchain:"
        "  podman build -f Dockerfile.build -t neurostrata-build ."
    )
    if [ -n "$probe_output" ]; then
        hints+=("" "The probe said:")
        while IFS= read -r line; do hints+=("  $line"); done <<< "$probe_output"
    fi
    fail "$cxx cannot compile a C++20 <format> program." "${hints[@]}"
fi

# ---------------------------------------------------------------------------
# 4. A build tool for CMake to drive
# ---------------------------------------------------------------------------
step "Checking the CMake generator"

generator=""
generator_version=""
if command -v ninja >/dev/null 2>&1; then
    generator="ninja"
    generator_version="$(version_of ninja --version)"
elif command -v make >/dev/null 2>&1; then
    generator="make"
    generator_version="$(version_of make --version)"
else
    fail "Neither ninja nor make was found." \
        "CMake needs a build tool to drive. Without one, configuration fails with" \
        "\"CMake was unable to find a build program\" -- a message naming neither" \
        "cargo nor NeuroStrata." \
        "" \
        "  Debian/Ubuntu   sudo apt-get install ninja-build" \
        "  Fedora          sudo dnf install ninja-build" \
        "  macOS           brew install ninja"
fi

# ---------------------------------------------------------------------------
# 5. Linux only: the vendored OpenSSL build
# ---------------------------------------------------------------------------
if [ "$os" = "Linux" ]; then
    step "Checking the vendored OpenSSL prerequisites"
    missing=()
    command -v perl >/dev/null 2>&1 || missing+=("perl")
    command -v pkg-config >/dev/null 2>&1 || missing+=("pkg-config")
    if [ "${#missing[@]}" -gt 0 ]; then
        fail "Missing on Linux: ${missing[*]}" \
            "openssl is a vendored dependency on this target -- it is compiled from" \
            "source, and its Configure script is written in perl. Cargo.toml gates the" \
            "dependency out on Windows and Apple targets, which have a system TLS stack," \
            "so this applies to Linux only." \
            "" \
            "  Debian/Ubuntu   sudo apt-get install perl pkg-config" \
            "  Fedora          sudo dnf install perl pkgconf-pkg-config"
    fi
fi

# ---------------------------------------------------------------------------
# 6. Report
# ---------------------------------------------------------------------------
printf '\nToolchain\n'
printf '  cargo   %s\n' "$cargo_version"
printf '%s          %s%s\n' "$c_dim" "$cargo_bin" "$c_off"
printf '  c++     %s\n' "$cxx_version"
printf '%s          %s%s\n' "$c_dim" "$(command -v "$cxx")" "$c_off"
printf '  cmake   %s\n' "$cmake_version"
printf '  %-7s %s\n' "$generator" "$generator_version"
printf '\n'

if [ "$check_only" -eq 1 ]; then
    printf '%sChecks passed. --check was set, so nothing was built.%s\n' "$c_green" "$c_off"
    exit 0
fi

# ---------------------------------------------------------------------------
# 7. Build
# ---------------------------------------------------------------------------
cargo_args=(build)
if [ "$profile" = "release" ]; then cargo_args+=(--release); fi
if [ -n "$locked" ]; then cargo_args+=("$locked"); fi
if [ "${#cargo_passthrough[@]}" -gt 0 ]; then cargo_args+=("${cargo_passthrough[@]}"); fi

step "cargo ${cargo_args[*]}"
printf '%s    A cold build compiles lbug'"'"'s C++ engine from source. Expect several minutes.%s\n\n' \
    "$c_dim" "$c_off"

cd "$repo_root"
"$cargo_bin" "${cargo_args[@]}"

binary="$repo_root/target/$profile/neurostrata-mcp"
printf '\n'
if [ -x "$binary" ]; then
    size="$(du -h "$binary" | cut -f1)"
    printf '%sBuilt %s (%s)%s\n' "$c_green" "$binary" "$size" "$c_off"
else
    printf '%scargo reported success but the expected binary is not at%s\n' "$c_yellow" "$c_off"
    printf '%s  %s%s\n' "$c_yellow" "$binary" "$c_off"
fi
