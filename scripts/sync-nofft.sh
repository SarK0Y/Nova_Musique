#!/usr/bin/env bash
#
# Re-vendor the No_FFT codec sources that Nova links against.
#
# Copies the pristine upstream files into third_party/no_fft/ and rewrites
# UPSTREAM.md with the exact branch/commit that was fetched. Defaults to the
# `main` branch (trunk) of https://github.com/SarK0Y/No_FFT_v0.git.
#
# Usage:
#   scripts/sync-nofft.sh [--branch <name>] [--remote <git-url>]
#
set -euo pipefail

REPO_URL="${NOFFT_REPO_URL:-https://github.com/SarK0Y/No_FFT_v0.git}"
BRANCH="${NOFFT_BRANCH:-main}"

usage() {
    cat <<'EOF'
Re-vendor the No_FFT codecs from upstream.

  --branch <name>   upstream branch to install from (default: main/trunk)
  --remote <url>    override the repository URL
  -h, --help        show this help

Environment overrides: NOFFT_BRANCH, NOFFT_REPO_URL.
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --branch) BRANCH="${2:?--branch needs a value}"; shift 2 ;;
        --remote) REPO_URL="${2:?--remote needs a value}"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="$ROOT/third_party/no_fft"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "fetching $REPO_URL ($BRANCH) ..."
git clone --depth 1 --branch "$BRANCH" "$REPO_URL" "$TMP/nofft" >/dev/null 2>&1

COMMIT="$(git -C "$TMP/nofft" rev-parse HEAD)"
DATE="$(git -C "$TMP/nofft" log -1 --format=%cI)"

mkdir -p "$DEST/third_party"
for f in nofft.cpp nofft.h acpcm.cpp acpcm.h LICENSE; do
    if [ ! -f "$TMP/nofft/$f" ]; then
        echo "upstream is missing $f; aborting" >&2
        exit 1
    fi
    cp "$TMP/nofft/$f" "$DEST/$f"
done
cp "$TMP/nofft/third_party/dr_wav.h" "$DEST/third_party/dr_wav.h"

cat > "$DEST/UPSTREAM.md" <<EOF
# Vendored: No_FFT codecs

This directory is a **verbatim, unmodified** copy of the codec sources from

    $REPO_URL

pinned to the \`$BRANCH\` branch (trunk).

| Field   | Value |
| ------- | ----- |
| Repo    | \`$REPO_URL\` |
| Branch  | \`$BRANCH\` |
| Commit  | \`$COMMIT\` |
| Date    | \`$DATE\` |

## Vendored files

| File | Origin |
| ---- | ------ |
| \`nofft.cpp\`, \`nofft.h\` | polynomial-fit codec (\`.no_fft\`) |
| \`acpcm.cpp\`, \`acpcm.h\` | DPCM/basket codecs (\`.nadc\`, NFA1–NFA4) |
| \`third_party/dr_wav.h\` | upstream's WAV reader dependency |
| \`LICENSE\` | upstream licence |

Nothing here is edited by Nova: all glue lives in \`crates/nova-codec\`
(\`shim/nova_nofft_shim.cpp\` gives the C++ code a stable C ABI) and
\`crates/nova-codec/build.rs\`. Keeping the checkout pristine makes re-syncing
from \`main\` a clean copy with no merge.

## Refreshing from main/trunk

    scripts/sync-nofft.sh                 # fetch branch main
    scripts/sync-nofft.sh --branch main   # explicit

The script rewrites this file with the new commit, date and branch.
EOF

echo "vendored No_FFT $BRANCH @ $COMMIT"
