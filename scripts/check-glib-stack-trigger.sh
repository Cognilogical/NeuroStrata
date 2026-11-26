#!/usr/bin/env bash
# Report-only trigger radar for #427 (glib RUSTSEC-2024-0429 / gtk-rs stack).
#
# Checks whether tauri, tauri-runtime-wry, and wry have moved to gtk >= 0.19 on
# Linux targets. Prints TRIGGER MET or BLOCKED and exits 0 in both cases
# (report-only). Exits non-zero only on usage or fetch errors.
#
# Usage:
#   scripts/check-glib-stack-trigger.sh [--crate <required-crate>[:<version>]]...
#
# The required crates are always tauri, tauri-runtime-wry, and wry. --crate may
# pin a version for one of them but cannot remove a required crate. Defaults to
# the latest stable version of each required crate on crates.io.

set -euo pipefail

UA="neurostrata-glib-radar (https://github.com/Cognilogical/NeuroStrata/issues/427)"
ARGS=()
SELF_TEST=0
while [ $# -gt 0 ]; do
  case "$1" in
    --crate)
      [ $# -ge 2 ] || { echo "error: --crate needs value" >&2; exit 2; }
      ARGS+=("--crate=$2")
      shift 2
      ;;
    --self-test)
      SELF_TEST=1
      shift
      ;;
    -h|--help)
      sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "error: unknown arg: $1" >&2
      exit 2
      ;;
  esac
done

command -v python3 >/dev/null 2>&1 || { echo "error: missing python3" >&2; exit 2; }

python3 - "$UA" "$SELF_TEST" ${ARGS[@]+"${ARGS[@]}"} <<'PY'
import json
import re
import sys
import urllib.error
import urllib.request

UA = sys.argv[1]
SELF_TEST = sys.argv[2] == "1"
REQUIRED = ("tauri", "tauri-runtime-wry", "wry")
overrides = {}
for arg in sys.argv[3:]:
    if not arg.startswith("--crate="):
        continue
    entry = arg.split("=", 1)[1]
    name, sep, version = entry.partition(":")
    if name not in REQUIRED:
        raise SystemExit(f"error: --crate must name one of {', '.join(REQUIRED)}")
    overrides[name] = version if sep and version else ""


def fetch_json(url):
    request = urllib.request.Request(url, headers={"User-Agent": UA})
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.load(response)
    except urllib.error.HTTPError as error:
        raise SystemExit(f"error: crates.io returned {error.code} for {url}")
    except urllib.error.URLError as error:
        raise SystemExit(f"error: could not reach crates.io: {error.reason}")


def latest_stable(crate):
    return fetch_json(f"https://crates.io/api/v1/crates/{crate}")["crate"][
        "max_stable_version"
    ]


def version_tuple(text):
    parts = text.split(".")
    while len(parts) < 3:
        parts.append("0")
    return tuple(int(part) for part in parts[:3])


def next_patch(version):
    return (version[0], version[1], version[2] + 1)


def next_minor(version):
    return (version[0], version[1] + 1, 0)


def next_major(version):
    return (version[0] + 1, 0, 0)


def comparator_interval(part):
    part = part.strip()
    if not part or part == "*":
        return ((0, 0, 0), None)

    wildcard = re.fullmatch(r"(\d+)\.(\d+)\.\*", part)
    if wildcard:
        base = (int(wildcard.group(1)), int(wildcard.group(2)), 0)
        return (base, next_minor(base))
    wildcard = re.fullmatch(r"(\d+)\.\*", part)
    if wildcard:
        base = (int(wildcard.group(1)), 0, 0)
        return (base, next_major(base))

    match = re.fullmatch(
        r"(\^|~|>=|<=|>|<|=)?\s*(\d+(?:\.\d+){0,2})", part
    )
    if not match:
        # An unparseable comparator is not evidence that the trigger fired.
        return ((1, 0, 0), (1, 0, 0))
    op = match.group(1) or "^"
    raw = match.group(2)
    version = version_tuple(raw)
    components = len(raw.split("."))

    if op == "^":
        if version[0] != 0:
            upper = next_major(version)
        elif version[1] != 0:
            upper = (0, version[1] + 1, 0)
        else:
            upper = next_patch(version)
        return (version, upper)
    if op == "~":
        upper = (
            (version[0], version[1] + 1, 0)
            if components >= 2
            else next_major(version)
        )
        return (version, upper)
    if op == ">=":
        return (version, None)
    if op == ">":
        return (next_patch(version), None)
    if op == "<":
        return ((0, 0, 0), version)
    if op == "<=":
        return ((0, 0, 0), next_patch(version))
    return (version, next_patch(version))  # "=" exact


def requirement_admits_gtk_019(requirement):
    """True when the Cargo requirement intersects [0.19.0, infinity)."""
    lower = (0, 0, 0)
    upper = None
    for part in requirement.split(","):
        part = part.strip()
        if not part:
            continue
        part_lower, part_upper = comparator_interval(part)
        lower = max(lower, part_lower)
        if part_upper is not None:
            upper = part_upper if upper is None else min(upper, part_upper)
    floor = max(lower, (0, 19, 0))
    return upper is None or floor < upper


def target_applies_to_linux(target):
    """Evaluate the cfg() forms crates.io uses for platform dependencies."""
    target = (target or "").strip()
    if not target:
        return True
    match = re.fullmatch(r"cfg\((.*)\)", target)
    expression = match.group(1) if match else target

    def evaluate(text):
        text = text.strip()
        if text.startswith("not(") and text.endswith(")"):
            return not evaluate(text[4:-1])
        if text.startswith("any(") and text.endswith(")"):
            return any(evaluate(part) for part in split_args(text[4:-1]))
        if text.startswith("all(") and text.endswith(")"):
            return all(evaluate(part) for part in split_args(text[4:-1]))
        if text == "unix":
            return True
        if text == "windows":
            return False
        key_value = re.fullmatch(r'(\w+)\s*=\s*"([^"]*)"', text)
        if key_value:
            key, value = key_value.groups()
            if key == "target_os":
                return value == "linux"
            if key == "target_family":
                return value == "unix"
            return False
        return False

    def split_args(text):
        parts = []
        depth = 0
        current = []
        for char in text:
            if char == "(":
                depth += 1
            elif char == ")":
                depth -= 1
            if char == "," and depth == 0:
                parts.append("".join(current))
                current = []
            else:
                current.append(char)
        if current:
            parts.append("".join(current))
        return parts

    return evaluate(expression)


def gtk_requirements(crate, version):
    url = f"https://crates.io/api/v1/crates/{crate}/{version}/dependencies"
    requirements = []
    page = 0
    while True:
        payload = fetch_json(url if page == 0 else f"{url}?page={page + 1}")
        for dependency in payload.get("dependencies", []):
            if dependency.get("crate_id") != "gtk":
                continue
            if dependency.get("kind") not in (None, "normal"):
                continue
            if not target_applies_to_linux(dependency.get("target")):
                continue
            requirement = dependency.get("req", "")
            if requirement not in requirements:
                requirements.append(requirement)
        next_page = (payload.get("meta") or {}).get("next_page")
        if not next_page:
            break
        page = int(next_page)
    return requirements


if SELF_TEST:
    requirements = {
        "^0.18": False,
        "^0.19.1": True,
        "^0.21": True,
        ">=0.19.1, <0.20": True,
        ">=0.18, <0.19": False,
        "0.19.1": True,
        "~0.19": True,
        "~0.18": False,
        ">0.18": True,
        "<=0.18.9": False,
        "*": True,
    }
    for requirement, expected in requirements.items():
        actual = requirement_admits_gtk_019(requirement)
        assert actual is expected, f"{requirement}: expected {expected}, got {actual}"

    targets = {
        "": True,
        "cfg(unix)": True,
        "cfg(not(windows))": True,
        'cfg(any(target_os = "linux", target_os = "macos"))': True,
        'cfg(target_os = "macos")': False,
        'cfg(not(target_os = "linux"))': False,
        'cfg(all(unix, not(target_os = "macos")))': True,
        "cfg(windows)": False,
    }
    for target, expected in targets.items():
        actual = target_applies_to_linux(target)
        assert actual is expected, f"{target}: expected {expected}, got {actual}"

    print("self-test: PASS")
    raise SystemExit(0)

blocked = False
print("# glib/gtk-rs trigger radar for #427")
print("# trigger: tauri, tauri-runtime-wry, and wry accept gtk >= 0.19 on Linux targets")
print()
for crate in REQUIRED:
    version = overrides.get(crate) or latest_stable(crate)
    print(f"crate: {crate}@{version}")
    requirements = gtk_requirements(crate, version)
    if not requirements:
        print("  gtk dep: (none found)")
        print("  status: UNKNOWN (no Linux gtk dependency found; manual review required)")
        blocked = True
        print()
        continue
    crate_ok = True
    for requirement in requirements:
        admits = requirement_admits_gtk_019(requirement)
        print(f"  gtk req: {requirement}")
        print(f"  allows >=0.19: {'yes' if admits else 'no'}")
        if not admits:
            crate_ok = False
    print(f"  status: {'READY' if crate_ok else 'BLOCKED'}")
    if not crate_ok:
        blocked = True
    print()

print("BLOCKED" if blocked else "TRIGGER MET")
PY
