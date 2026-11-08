#!/usr/bin/env bash
# Download a pinned Z3 release for x86_64 Linux and install it to .semilla/tools/z3.
#
# Usage: scripts/fetch-z3.sh
#
# The script uses the official release from github.com/Z3Prover/z3/releases.
# It checks the SHA-256 of the archive before it uses the archive.
# The build needs glibc 2.39 or later.

set -euo pipefail

Z3_VERSION="4.15.4"
Z3_ASSET="z3-${Z3_VERSION}-x64-glibc-2.39"
Z3_SHA256="a41b690e89c343931471506cdc6d957b6044a200fd2d240cc017432afdff7d3e"
Z3_URL="https://github.com/Z3Prover/z3/releases/download/z3-${Z3_VERSION}/${Z3_ASSET}.zip"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST_DIR="${ROOT}/.semilla/tools"
DEST="${DEST_DIR}/z3"

if [[ "$(uname -s)" != "Linux" || "$(uname -m)" != "x86_64" ]]; then
    echo "error: this script supports x86_64 Linux only" >&2
    exit 1
fi

# Do nothing if the correct version is already installed.
if [[ -x "${DEST}" ]] && "${DEST}" --version 2>/dev/null | grep -q "Z3 version ${Z3_VERSION}"; then
    echo "Z3 ${Z3_VERSION} is already installed at ${DEST}"
    exit 0
fi

WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

echo "Downloading ${Z3_URL}"
curl --fail --location --silent --show-error --retry 3 --output "${WORK}/z3.zip" "${Z3_URL}"

echo "${Z3_SHA256}  ${WORK}/z3.zip" | sha256sum --check --quiet - || {
    echo "error: the SHA-256 of the archive is not correct" >&2
    exit 1
}

# Extract only the z3 binary. Use the first tool that is available.
MEMBER="${Z3_ASSET}/bin/z3"
mkdir -p "${WORK}/out"
if command -v unzip >/dev/null 2>&1; then
    unzip -q -j "${WORK}/z3.zip" "${MEMBER}" -d "${WORK}/out"
elif command -v python3 >/dev/null 2>&1; then
    python3 -I - "${WORK}/z3.zip" "${MEMBER}" "${WORK}/out/z3" <<'EOF'
import shutil, sys, zipfile
archive, member, target = sys.argv[1:4]
with zipfile.ZipFile(archive) as zf, zf.open(member) as src, open(target, "wb") as dst:
    shutil.copyfileobj(src, dst)
EOF
elif command -v busybox >/dev/null 2>&1; then
    (cd "${WORK}/out" && busybox unzip -q -j "${WORK}/z3.zip" "${MEMBER}")
else
    echo "error: unzip, python3, or busybox is necessary to extract the archive" >&2
    exit 1
fi
chmod +x "${WORK}/out/z3"

# Make sure that the binary runs on this machine before you install it.
if ! VERSION_LINE="$("${WORK}/out/z3" --version 2>&1)"; then
    echo "error: the Z3 binary does not run on this machine:" >&2
    echo "${VERSION_LINE}" >&2
    exit 1
fi
if ! echo '(check-sat)' | "${WORK}/out/z3" -in -smt2 | grep -q '^sat$'; then
    echo "error: the Z3 binary does not answer a simple query" >&2
    exit 1
fi

mkdir -p "${DEST_DIR}"
mv "${WORK}/out/z3" "${DEST}"
echo "Installed ${VERSION_LINE} at ${DEST}"
