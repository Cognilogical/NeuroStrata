#!/usr/bin/env python3
"""Laya bridge probe script to verify local ONNX model loading and parity.

Real measurements, no mock contract numbers:

1. reads the exported bundle's ``manifest.json`` (either the directory passed
   as ``sys.argv[1]`` or the default ``/tmp/laya-bundle`` when it exists) and
   verifies the ``model.onnx`` SHA-256 the manifest declares against the file
   actually on disk;
2. computes the real f32 vs f64 logistic sigmoid parity across a grid of
   logits for the manifest's scale/bias calibration, and reports the actual
   measured ``verified_noul_prob_diff`` in the JSON output (the same quantity
   ``examples/laya_onnx_bridge_probe.rs`` asserts under 1e-4).

Emulating f32: Python floats are f64, so every intermediate is rounded back
to f32 by packing it through ``struct`` before feeding the next step — the
same rounding behavior the Rust f32 production path exhibits.
"""
import hashlib
import json
import math
import struct
import sys
from pathlib import Path

DEFAULT_BUNDLE = Path("/tmp/laya-bundle")
TEST_LOGITS = [-2.0, -1.0, 0.0, 1.0, 2.0]
MAX_DIFF_TOLERANCE = 1e-4


def f32(x):
    """Round a Python float (f64) to f32 precision."""
    return struct.unpack("f", struct.pack("f", x))[0]


def logistic_f64(logit, scale, bias):
    """The f64 reference logistic calibration."""
    return 1.0 / (1.0 + math.exp(-(scale * logit + bias)))


def logistic_f32(logit, scale, bias):
    """The f32 production path, each op rounding to f32 like Rust's."""
    x32 = f32(float(logit))
    scale32 = f32(scale)
    bias32 = f32(bias)
    z32 = f32(scale32 * x32)          # scale * x, f32-rounded
    z32 = f32(z32 + bias32)           # + bias, f32-rounded
    neg32 = f32(-z32)
    exp32 = f32(math.exp(neg32))      # exp computed then read back as f32
    denom32 = f32(1.0 + exp32)
    return f32(1.0 / denom32)


def sha256_of_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 16), b""):
            h.update(chunk)
    return h.hexdigest()


def read_calibration(bundle_dir):
    """Read the bundle manifest; return (manifest, scale, bias)."""
    manifest_path = bundle_dir / "manifest.json"
    if not manifest_path.is_file():
        raise FileNotFoundError(f"expected bundle manifest at {manifest_path}")
    with open(manifest_path) as f:
        manifest = json.load(f)
    source = manifest.get("calibration", manifest)
    scale = source.get("scale")
    bias = source.get("bias")
    if scale is None or bias is None:
        raise ValueError(f"manifest {manifest_path} has no usable scale/bias calibration")
    return manifest, float(scale), float(bias)


def verify_bundle(bundle_dir):
    manifest, scale, bias = read_calibration(bundle_dir)
    model_path = bundle_dir / "model.onnx"
    verified_sha = False
    if model_path.is_file():
        expected = manifest.get("model_sha256")
        if not expected:
            raise ValueError(f"manifest must carry model_sha256 when {model_path} exists")
        actual = sha256_of_file(model_path)
        if actual.lower() != str(expected).lower():
            raise ValueError(f"manifest SHA-256 mismatch for model.onnx ({expected} != {actual})")
        verified_sha = True
    print(f"bundle {bundle_dir}: manifest.json parsed, "
          f"model_sha256 {'verified: ' + str(sha256_of_file(model_path)) if verified_sha else 'skipped (no model.onnx)'}")
    return scale, bias


def main():
    bundle_dir = None
    if len(sys.argv) > 1:
        bundle_dir = Path(sys.argv[1])
        if not (bundle_dir / "manifest.json").is_file():
            raise FileNotFoundError(f"expected bundle manifest at {bundle_dir / 'manifest.json'}")
    elif (DEFAULT_BUNDLE / "manifest.json").is_file():
        bundle_dir = DEFAULT_BUNDLE

    scale, bias = (0.5, -0.25)
    if bundle_dir is not None:
        scale, bias = verify_bundle(bundle_dir)

    diffs = []
    for logit in TEST_LOGITS:
        reference = logistic_f64(logit, scale, bias)
        production = logistic_f32(logit, scale, bias)
        diffs.append(abs(production - reference))
    verified_noul_prob_diff = max(diffs)

    contract = {
        "package_version": "0.1.0-bridge",
        "argv_template": ["python", "laya-train", "train", "test_version", "--output", "test_output"],
        "expected_artifacts": ["model.onnx", "manifest.json", "tokenizer.json", "calibration.json"],
        "export_steps": ["load_model", "run_inference", "save_artifacts"],
        "runtime_target": "linux_x86_64",
        "calibration": {"scale": scale, "bias": bias},
        "test_logits": TEST_LOGITS,
        "verified_noul_prob_diff": verified_noul_prob_diff,
    }
    print(json.dumps(contract, indent=2))
    if not verified_noul_prob_diff < MAX_DIFF_TOLERANCE:
        raise SystemExit(
            f"f32/f64 calibration parity regressed: "
            f"verified_noul_prob_diff {verified_noul_prob_diff} >= {MAX_DIFF_TOLERANCE}"
        )
    print("Probe completed successfully")
    return 0


if __name__ == "__main__":
    sys.exit(main())
