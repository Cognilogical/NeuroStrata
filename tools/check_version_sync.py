#!/usr/bin/env python3
"""Guard the single version fact.

The root ``Cargo.toml`` ``[package] version`` is the project's one version
fact. Three other manifests carry copies of it -- the Tauri crate, the
Tauri app config, and the web UI package -- and they drifted to ``0.1.1``
while the core moved on (panel row J33). This check reads the root version
and asserts equality in the three files, parsing TOML/JSON properly (never
regex), and exits non-zero naming each drifting file.

Usage: ``python3 tools/check_version_sync.py [ROOT]`` where ROOT defaults
to the repository root (the parent of this script's ``tools/`` directory).

Requires Python 3.11+ (stdlib ``tomllib``). No third-party dependencies.
"""

from __future__ import annotations

import json
import sys
import tomllib
from pathlib import Path

#: Manifests whose version must equal the root version, as repo-relative
#: paths. The root ``Cargo.toml`` itself is the source of truth and is not
#: in this list.
SYNCED_FILES = (
    "src-tauri/Cargo.toml",
    "src-tauri/tauri.conf.json",
    "web-ui/package.json",
)


class ManifestProblem(Exception):
    """A manifest is missing, unreadable, or carries no version field."""


def _manifest_version(path: Path) -> str:
    """Return the package version recorded in a TOML or JSON manifest.

    Raises:
        ManifestProblem: with a short problem phrase ("is missing",
            "has no version field", "is unreadable (...)"), so callers
            can name the file and fail in the checker's voice instead of
            surfacing a raw traceback.
    """
    try:
        if path.suffix == ".toml":
            with path.open("rb") as fh:
                return tomllib.load(fh)["package"]["version"]
        with path.open(encoding="utf-8") as fh:
            return json.load(fh)["version"]
    except FileNotFoundError:
        raise ManifestProblem("is missing") from None
    except KeyError:
        raise ManifestProblem("has no version field") from None
    except (OSError, ValueError) as exc:
        raise ManifestProblem(f"is unreadable ({exc})") from None


def root_version(root: Path) -> str:
    """The one version fact: root ``Cargo.toml``'s ``[package] version``."""
    try:
        return _manifest_version(root / "Cargo.toml")
    except ManifestProblem as exc:
        raise ManifestProblem(f"Cargo.toml {exc}") from None


def find_drift(root: Path) -> list[tuple[str, str, str]]:
    """Return ``(relative path, actual, expected)`` for each drifting file."""
    expected = root_version(root)
    drift = []
    for rel in SYNCED_FILES:
        try:
            actual = _manifest_version(root / rel)
        except ManifestProblem as exc:
            raise ManifestProblem(f"{rel} {exc}") from None
        if actual != expected:
            drift.append((rel, actual, expected))
    return drift


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    if args:
        root = Path(args[0]).resolve()
    else:
        root = Path(__file__).resolve().parent.parent
    try:
        drift = find_drift(root)
    except ManifestProblem as exc:
        print(f"version drift: {exc}")
        return 1
    if drift:
        for rel, actual, expected in drift:
            print(
                f"version drift: {rel} has {actual}, "
                f"expected {expected} (root Cargo.toml)"
            )
        return 1
    print(f"version sync ok: all manifests at {root_version(root)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
