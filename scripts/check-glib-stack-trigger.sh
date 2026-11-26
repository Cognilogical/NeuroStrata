#!/usr/bin/env bash
# Report-only trigger radar for #427 (glib RUSTSEC-2024-0429 / gtk-rs stack).
#
# Checks whether tauri and tauri-runtime-wry have moved to gtk >= 0.19 on Linux
# targets. Prints TRIGGER MET or BLOCKED and exits 0 in both cases (report-only).
# Exits non-zero only on usage/fetch errors.
#
# Usage:
#   scripts/check-glib-stack-trigger.sh [--crate tauri[:version]] [--crate tauri-runtime-wry[:version]]
#
# Defaults to latest stable versions of tauri and tauri-runtime-wry on crates.io.

set -euo pipefail

UA="neurostrata-glib-radar (https://github.com/Cognilogical/NeuroStrata/issues/427)"
CRATES=()
while [ $# -gt 0 ]; do
  case "$1" in
    --crate)
      [ $# -ge 2 ] || { echo "error: --crate needs value" >&2; exit 2; }
      CRATES+=("$2")
      shift 2
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

if [ ${#CRATES[@]} -eq 0 ]; then
  CRATES=("tauri" "tauri-runtime-wry")
fi

for cmd in curl python3; do
  command -v "$cmd" >/dev/null 2>&1 || { echo "error: missing $cmd" >&2; exit 2; }
done

fetch_json() {
  curl -fsSL -H "User-Agent: $UA" "$1"
}

latest_stable() {
  local crate="$1"
  fetch_json "https://crates.io/api/v1/crates/${crate}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["crate"]["max_stable_version"])'
}

gtk_req_for_linux() {
  local crate="$1" version="$2"
  fetch_json "https://crates.io/api/v1/crates/${crate}/${version}/dependencies" | python3 -c '
import json,sys
d=json.load(sys.stdin)
reqs=[]
for dep in d.get("dependencies",[]):
    if dep.get("crate_id")!="gtk":
        continue
    if dep.get("kind")!="normal":
        continue
    target=(dep.get("target") or "")
    if "linux" in target or target=="":
        reqs.append(dep.get("req",""))
# dedupe preserve order
seen=set(); out=[]
for r in reqs:
    if r not in seen:
        seen.add(r); out.append(r)
print("\n".join(out))
'
}

allows_gtk_0_19() {
  local req="$1"
  python3 - "$req" <<'PY'
import sys,re
req=sys.argv[1].strip()
if not req:
    print("no")
    raise SystemExit
# Split on commas, evaluate simple Cargo semver comparators.
parts=[p.strip() for p in req.split(',') if p.strip()]
# Candidate versions that represent "accepts gtk >= 0.19 line"
candidates=["0.19.0","0.20.0","0.22.0","1.0.0"]
def sat(ver, part):
    m=re.match(r'^(\^|~|>=|<=|>|<|=)?\s*([0-9]+(?:\.[0-9]+){0,2})', part)
    if not m:
        return False
    op=m.group(1) or '='
    v=[int(x) for x in m.group(2).split('.')]
    while len(v)<3: v.append(0)
    V=tuple(v)
    X=tuple(int(x) for x in ver.split('.'))
    if op=='=':
        return X==V
    if op=='>=':
        return X>=V
    if op=='>':
        return X>V
    if op=='<=':
        return X<=V
    if op=='<':
        return X<V
    if op=='^':
        if V[0]!=0:
            upper=(V[0]+1,0,0)
        elif V[1]!=0:
            upper=(0,V[1]+1,0)
        else:
            upper=(0,0,V[2]+1)
        return X>=V and X<upper
    if op=='~':
        if len([int(x) for x in m.group(2).split('.')])>=2:
            upper=(V[0],V[1]+1,0) if V[0]==0 else (V[0]+1,0,0)
        else:
            upper=(V[0]+1,0,0)
        return X>=V and X<upper
    return False

ok=any(all(sat(c,p) for p in parts) for c in candidates)
print("yes" if ok else "no")
PY
}

blocked=0
echo "# glib/gtk-rs trigger radar for #427"
echo "# trigger: tauri and tauri-runtime-wry accept gtk >= 0.19 on Linux targets"
echo

for entry in "${CRATES[@]}"; do
  crate="${entry%%:*}"
  version="${entry#*:}"
  if [ "$crate" = "$version" ]; then
    version="$(latest_stable "$crate")"
  fi
  echo "crate: $crate@$version"
  reqs="$(gtk_req_for_linux "$crate" "$version")"
  if [ -z "$reqs" ]; then
    echo "  gtk dep: (none found)"
    echo "  status: UNKNOWN (no gtk dependency found; manual review required)"
    blocked=1
    echo
    continue
  fi
  crate_ok=1
  while IFS= read -r req; do
    [ -n "$req" ] || continue
    allows="$(allows_gtk_0_19 "$req")"
    echo "  gtk req: $req"
    echo "  allows >=0.19: $allows"
    if [ "$allows" != "yes" ]; then
      crate_ok=0
    fi
  done <<<"$reqs"
  if [ "$crate_ok" -eq 1 ]; then
    echo "  status: READY"
  else
    echo "  status: BLOCKED"
    blocked=1
  fi
  echo
done

if [ "$blocked" -eq 0 ]; then
  echo "TRIGGER MET"
else
  echo "BLOCKED"
fi
