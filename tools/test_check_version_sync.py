"""Tests for tools/check_version_sync.py.

Run locally: ``python -m pytest tools/test_check_version_sync.py``

The fixture tests build copies of the four manifests in a temp directory;
the real-tree test runs the checker against this repository and is the
standing local guard (it is also the RED record for the J33 drift: before
the stale copies were synced it failed, naming all three ``0.1.1`` files).
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import check_version_sync as cvs

ROOT_VERSION = "1.2.3"
STALE_VERSION = "0.0.9"


def _write_fixture_tree(base: Path, copy_version: str) -> Path:
    """Write the four manifests under *base*: root at ROOT_VERSION, the
    three synced copies at *copy_version*."""
    (base / "src-tauri").mkdir(parents=True)
    (base / "web-ui").mkdir(parents=True)
    (base / "Cargo.toml").write_text(
        f'[package]\nname = "core"\nversion = "{ROOT_VERSION}"\n'
    )
    (base / "src-tauri" / "Cargo.toml").write_text(
        f'[package]\nname = "gui"\nversion = "{copy_version}"\n'
    )
    (base / "src-tauri" / "tauri.conf.json").write_text(
        json.dumps({"version": copy_version})
    )
    (base / "web-ui" / "package.json").write_text(
        json.dumps({"name": "web-ui", "version": copy_version})
    )
    return base


def test_check_passes_when_versions_match(tmp_path, capsys):
    root = _write_fixture_tree(tmp_path, ROOT_VERSION)
    assert cvs.main([str(root)]) == 0
    assert cvs.find_drift(root) == []
    assert ROOT_VERSION in capsys.readouterr().out


def test_check_names_each_drifting_file(tmp_path, capsys):
    root = _write_fixture_tree(tmp_path, STALE_VERSION)
    assert cvs.main([str(root)]) == 1
    out = capsys.readouterr().out
    for rel in cvs.SYNCED_FILES:
        assert rel in out
    drift = cvs.find_drift(root)
    assert [rel for rel, _, _ in drift] == list(cvs.SYNCED_FILES)
    assert all(
        actual == STALE_VERSION and expected == ROOT_VERSION
        for _, actual, expected in drift
    )


def test_missing_manifest_fails_with_clean_message(tmp_path, capsys):
    """A deleted copy fails closed: exit 1 and the checker's own message
    naming the file -- not an uncaught FileNotFoundError traceback."""
    root = _write_fixture_tree(tmp_path, ROOT_VERSION)
    (root / "web-ui" / "package.json").unlink()
    assert cvs.main([str(root)]) == 1
    out = capsys.readouterr().out
    assert "version drift: web-ui/package.json is missing" in out
    assert "Traceback" not in out


def test_real_tree_is_in_sync(capsys):
    """The repository this test lives in must satisfy its own check."""
    assert cvs.main([]) == 0
    capsys.readouterr()  # drain; failure output is asserted in the RED run
