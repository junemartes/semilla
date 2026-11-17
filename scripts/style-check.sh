#!/usr/bin/env bash
# Mechanical checks for docs/STYLE.md. Exit 0 when every check passes.
# The judgment rules (R1, R3, R4, R5, R6, R7, R9, R10, R14, R15, R16) are
# not checked here. Read the diff against docs/STYLE.md for those.
set -uo pipefail
cd "$(dirname "$0")/.."

failed=0
step() {
    echo "==> $1"
    if ! "${@:2}"; then
        echo "FAILED: $1" >&2
        failed=1
    fi
}

# R11, R2 (too_many_lines at the threshold in clippy.toml), R8 (unreachable_pub).
step "clippy pedantic" cargo clippy --workspace --all-targets --locked -- \
    -D warnings -W clippy::pedantic -D unreachable_pub

step "rustfmt" cargo fmt --all -- --check

# Test files, test data, and comment lines are exempt.
EXEMPT='/tests?/|_tests?\.rs:|/tests\.rs:|^spikes/s1/cases/|^[^:]+[:-][0-9]+[:-]\s*//'

# R9, R13, R6, R11: patterns that a lint cannot express. Semilla reads Rust
# code, so a test fixture holds Rust code in strings. A match inside a string
# literal is not a use: an odd count of quote marks before the match on its
# line marks a string.
PATTERN='debug_assert!|\.max\(1\)|(^|[^A-Za-z0-9_])dyn[[:space:]]+[A-Za-z_(]|allow\(clippy::too_many_arguments\)'
outside_strings() {
    PATTERN="$PATTERN" awk '{
        head = $0
        sub(/^[^:]+:[0-9]+:/, "", head)
        if (!match(head, ENVIRON["PATTERN"])) next
        before = substr(head, 1, RSTART - 1)
        gsub(/\\./, "", before)
        if (gsub(/"/, "", before) % 2 == 0) print
    }'
}
forbidden() {
    local hits
    hits="$(grep -rnE --include='*.rs' -e "$PATTERN" crates spikes 2>/dev/null \
        | grep -vE "$EXEMPT" | outside_strings || true)"
    # `Box<` and `dyn` split over two lines by rustfmt: report the file and
    # the line of the `Box<`.
    local wrapped
    wrapped="$(grep -rnE --include='*.rs' -A1 -e 'Box<$' crates spikes 2>/dev/null \
        | grep -E -B1 '^[^:]+-[0-9]+-\s*dyn ' \
        | grep -E ':[0-9]+:' \
        | grep -vE "$EXEMPT" || true)"
    hits="${hits}${wrapped:+$'\n'$wrapped}"
    if [ -n "$hits" ]; then
        echo "$hits"
        return 1
    fi
}
step "forbidden patterns (debug_assert!, .max(1), dyn, allow(too_many_arguments))" forbidden

if [ "$failed" -ne 0 ]; then
    echo "style check failed. See docs/STYLE.md." >&2
    exit 1
fi
echo "style check passed."
