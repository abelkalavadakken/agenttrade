#!/bin/sh
# Downloads the acceptance tape from the tagged release. CI and developers
# run this; the file is gitignored because it is 11.5 MB.
set -eu
TAG="${AGENTTRADE_TAPE_TAG:-v0.1.0-core}"
REPO="${AGENTTRADE_REPO:-abelkalavadakken/agenttrade}"
DIR="$(cd "$(dirname "$0")" && pwd)"
OUT="$DIR/acceptance.tape"
if [ -s "$OUT" ]; then
  echo "acceptance.tape present ($(wc -c < "$OUT") bytes)"
  exit 0
fi
URL="https://github.com/$REPO/releases/download/$TAG/acceptance.tape"
echo "fetching $URL"
curl -fsSL --retry 3 -o "$OUT.part" "$URL"
mv "$OUT.part" "$OUT"
echo "acceptance.tape fetched ($(wc -c < "$OUT") bytes)"
