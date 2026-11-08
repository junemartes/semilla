#!/usr/bin/env bash
# Spike S3: the compile cost of semilla on a real crate.
#
# Usage: spikes/s3/run.sh <crate source folder> <work folder>
#
# The script copies the crate into a new git repository in the work folder, and measures:
#   1. `semilla index` of the base tree (the N0 build only),
#   2. the first `semilla check` of a step (N0, N2, N3 for both trees),
#   3. the same check again (all results come from the cache),
#   4. the check of a second step (only the new tree is built).
# Build semilla first: cargo build --release -p semilla -p semilla-mir
# Set SEMILLA_PROFILE=debug to measure the debug binaries.
set -euo pipefail

src=$(cd "$1" && pwd)
work=$2
root=$(cd "$(dirname "$0")/../.." && pwd)
profile=${SEMILLA_PROFILE:-release}
semilla="$root/target/$profile/semilla"
export SEMILLA_MIR_DRIVER="$root/target/$profile/semilla-mir-driver"
export SEMILLA_TIMING=1
export CARGO_NET_OFFLINE=true
[ -x "$root/.semilla/tools/z3" ] && export SEMILLA_Z3="$root/.semilla/tools/z3"

rm -rf "$work"
mkdir -p "$work"
cp -r "$src" "$work/repo"
cd "$work/repo"
rm -f Cargo.toml.orig .cargo_vcs_info.json
# The dev-dependencies and optional dependencies may not be in the offline cache. Remove them,
# the targets that need them, and the features that name them.
python3 -I - Cargo.toml <<'PY'
import re, sys, tomllib
path = sys.argv[1]
text = open(path).read()
manifest = tomllib.loads(text)
# Optional dependencies can be missing from the offline cache too.
optional = {name for name, dep in manifest.get('dependencies', {}).items() if isinstance(dep, dict) and dep.get('optional')}
features = {}
for name, items in manifest.get('features', {}).items():
    if name in optional:
        continue
    features[name] = [i for i in items if i.split('/')[0].removeprefix('dep:').rstrip('?') not in optional]
out, skip = [], False
for line in text.splitlines():
    if line.startswith('['):
        header = line.strip('[]')
        skip = ('dev-dependencies' in line
                or re.match(r'^\[\[(test|bench|example)\]\]', line) is not None
                or header == 'features'
                or any(header.endswith('dependencies.' + name) for name in optional))
    if not skip:
        out.append(line)
out.append('[features]')
out.extend(f'{name} = [{", ".join(chr(34) + i + chr(34) for i in items)}]' for name, items in features.items())
open(path, 'w').write('\n'.join(out) + '\n')
PY
name=$(python3 -I -c "import tomllib; print(tomllib.load(open('Cargo.toml','rb'))['package']['name'].replace('-','_'))")
lines=$(find src -name '*.rs' -exec cat {} + | wc -l)
# The stripped manifest needs a new lock file; cargo would rewrite it on each build otherwise.
cargo generate-lockfile --offline -q 2>/dev/null || true
printf '/target\n/.semilla/\n' > .gitignore
git init -q -b main
git -c user.name=s3 -c user.email=s3@s3 add -A
git -c user.name=s3 -c user.email=s3@s3 commit -q -m base

measure() {
  local label=$1; shift
  local start end
  start=$(date +%s.%N)
  "$@" > "$work/$label.out" 2> "$work/$label.err" || true
  end=$(date +%s.%N)
  printf '%-34s %7.2f s\n' "$label" "$(echo "$end - $start" | bc)"
  grep '^\[timing\]' "$work/$label.err" | sed 's/^/    /' || true
}

echo "crate: $name ($lines lines of Rust)"
measure "1 index (base, N0)" "$semilla" index

"$semilla" step begin "add a probe function" > /dev/null
printf '\n/// A probe for spike S3.\npub fn semilla_s3_probe(x: u32) -> u32 {\n    x + 1\n}\n' >> src/lib.rs
"$semilla" claim add-item --at "$name::semilla_s3_probe" > /dev/null
"$semilla" step end > /dev/null
measure "2 first check of a step" "$semilla" check
measure "3 same check, cached" "$semilla" check

"$semilla" step begin "change the probe" > /dev/null
sed -i 's/    x + 1$/    x + 2/' src/lib.rs
"$semilla" claim change-literal --at "$name::semilla_s3_probe/tail/rhs" > /dev/null
"$semilla" step end > /dev/null
measure "4 check of a second step" "$semilla" check

echo
echo "result of the last check:"
grep -E '^(result|  [✓✗?])' "$work/4 check of a second step.out" || cat "$work/4 check of a second step.out"
du -sh .semilla/cache 2>/dev/null | sed 's/^/cache size: /'
