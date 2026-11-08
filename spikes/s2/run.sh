#!/usr/bin/env bash
# Spike S2: build a pinned Charon and run it on one crate.
#
# Usage: spikes/s2/run.sh <cache-dir> <crate-dir> [<out-dir>]
#
# - <cache-dir>: Charon is cloned and built here (once). The full-MIR sysroot also goes here.
# - <crate-dir>: the crate to translate. Use a copy: cargo and build scripts run in it.
# - <out-dir>:   the output folder. The default is <cache-dir>/out/<crate-folder-name>.
#
# The script writes these files to <out-dir>:
# - llbc.json, ullbc.json: the default LLBC and ULLBC output (deduplicated JSON).
# - review.json: LLBC with the "review" flags, plain JSON (input for tools/analyze.py).
# - nodup.json: the same as review.json, with --no-code-duplication.
# - mir.txt: the rustc MIR of the crate (`-Zunpretty=mir`, input for tools/count_mir.py).
# - *.log, *.time: the logs and the run times.
# - *.analysis.json, mir.counts.json: the measurements.
#
# Charon is downloaded code. Use a cache folder outside your own repositories.
set -euo pipefail

if [ $# -lt 2 ]; then
    echo "usage: $0 <cache-dir> <crate-dir> [<out-dir>]" >&2
    exit 2
fi

# The pinned Charon: tag nightly-2026.10.15, version 0.1.289. It pins rustc nightly-2026-09-17.
CHARON_REPO=https://github.com/AeneasVerif/charon
CHARON_COMMIT=1c4aedb1dfaeb1436deffb03513fcccf2b0abecd

SPIKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
mkdir -p "$1"
CACHE=$(cd "$1" && pwd)
CRATE=$(cd "$2" && pwd)
OUT=${3:-$CACHE/out/$(basename "$CRATE")}
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)

SRC=$CACHE/charon
CHARON=$SRC/charon/target/release/charon

# 1. Clone and build Charon once.
if [ ! -x "$CHARON" ] || [ "$(git -C "$SRC" rev-parse HEAD 2>/dev/null)" != "$CHARON_COMMIT" ]; then
    if [ ! -d "$SRC/.git" ]; then
        git clone --quiet "$CHARON_REPO" "$SRC"
    fi
    git -C "$SRC" fetch --quiet origin
    git -C "$SRC" checkout --quiet "$CHARON_COMMIT"
    # The build needs the minirust submodule.
    git -C "$SRC" submodule update --init --recursive --quiet
    # The README says `make build-charon-rust`. That target runs `cargo fmt` first, and the pinned
    # toolchain has no rustfmt. So we run the build step of the Makefile directly. Rustup reads
    # charon/rust-toolchain and installs the pinned nightly.
    echo "building Charon $CHARON_COMMIT ..." >&2
    (cd "$SRC/charon" && /usr/bin/time -f "WALL=%e USER=%U MAXRSS_KB=%M" -o "$CACHE/build.time" cargo build --release)
    cat "$CACHE/build.time" >&2
fi
"$CHARON" version

export CHARON_CACHE_DIR=$CACHE/charon-cache
# Keep the build output of the crate out of the crate folder.
export CARGO_TARGET_DIR=$OUT/target
TOOLCHAIN=$("$CHARON" toolchain-version)

# 2. Build the full-MIR sysroot once (Charon does it on first use; here we time it).
/usr/bin/time -f "WALL=%e" -o "$OUT/sysroot.time" "$CHARON" print-sysroot > /dev/null

# 3. Count the MIR bodies with rustc (the same nightly as Charon).
(cd "$CRATE" && cargo "+$TOOLCHAIN" rustc --lib -q -- -Zunpretty=mir) > "$OUT/mir.txt" 2> "$OUT/mir.log"
python3 -I "$SPIKE_DIR/tools/count_mir.py" "$OUT/mir.txt" > "$OUT/mir.counts.json"

# 4. Run Charon. Each run starts from a clean target folder, so the time includes the compile.
REVIEW=(--reconstruct-matches --reconstruct-asserts --reconstruct-panic-calls
    --reconstruct-fallible-operations --hide-marker-traits)
run() {
    local name=$1
    shift
    rm -rf "$CARGO_TARGET_DIR"
    local status=0
    (cd "$CRATE" && /usr/bin/time -f "WALL=%e USER=%U MAXRSS_KB=%M" -o "$OUT/$name.time" \
        "$CHARON" cargo "$@" --dest-file "$OUT/$name.json") > "$OUT/$name.log" 2>&1 || status=$?
    printf '%-6s exit=%s %s size=%s errors=%s warnings=%s\n' "$name" "$status" \
        "$(tail -1 "$OUT/$name.time")" "$(stat -c %s "$OUT/$name.json" 2>/dev/null || echo -)" \
        "$(grep -c '^error' "$OUT/$name.log" || true)" "$(grep -c '^warning' "$OUT/$name.log" || true)"
}
run llbc
run ullbc --ullbc
run review --no-dedup-serialized-ast "${REVIEW[@]}"
run nodup --no-dedup-serialized-ast "${REVIEW[@]}" --no-code-duplication

# 5. Measure.
for name in review nodup; do
    if [ -s "$OUT/$name.json" ]; then
        python3 -I "$SPIKE_DIR/tools/analyze.py" "$OUT/$name.json" > "$OUT/$name.analysis.json"
    fi
done
echo "rustc bodies: $(cat "$OUT/mir.counts.json")"
echo "output: $OUT"
