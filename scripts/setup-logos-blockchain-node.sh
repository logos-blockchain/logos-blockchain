#!/bin/bash
set -e

VERSION="${1:-0.1.1}"
PLATFORM="${2:-linux-x86_64}"
OUT_PATH="${3:-/usr/local/bin}/logos-blockchain-node"

echo "Installing Logos Node $VERSION ($PLATFORM) to $OUT_PATH."

curl -Lfo "$OUT_PATH" --create-dirs \
  "https://github.com/logos-blockchain/logos-blockchain/releases/download/$VERSION/logos-blockchain-node-$PLATFORM-$VERSION"

chmod +x "$OUT_PATH"

echo "Done."
