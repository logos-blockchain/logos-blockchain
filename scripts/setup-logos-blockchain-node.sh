#!/bin/bash
set -e

VERSION="${1:-0.1.1}"
PLATFORM="${2:-linux-x86_64}"
OUT_PATH="${3:-/usr/local/bin}"
TMP_FILE="/tmp/logos-blockchain-node.tar.gz"

echo "Installing Logos Node $VERSION ($PLATFORM) to $OUT_PATH."

curl -Lfo "$TMP_FILE" \
  "https://github.com/logos-blockchain/logos-blockchain/releases/download/$VERSION/logos-blockchain-node-$PLATFORM-$VERSION.tar.gz"

tar -xzf "$TMP_FILE" -C "$OUT_PATH"
rm "$TMP_FILE"

chmod +x "$OUT_PATH/logos-blockchain-node"

echo "Done."
