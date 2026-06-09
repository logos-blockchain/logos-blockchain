#!/bin/bash
set -e

VERSION="${1:-0.5.1}"
PLATFORM="${2:-linux-x86_64}"
OUT_PATH="${3:-/opt/circuits}"

REPO="logos-blockchain/logos-blockchain-circuits"
ARTIFACT_PREFIX="logos-blockchain-circuits"

ARTIFACT_NAME="${ARTIFACT_PREFIX}-${VERSION}-${PLATFORM}"
ARTIFACT_TAR_GZ="${ARTIFACT_NAME}.tar.gz"
DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${VERSION}/${ARTIFACT_TAR_GZ}"
TMP_FILE="/tmp/${ARTIFACT_TAR_GZ}"

echo "Installing Logos Circuits $VERSION ($PLATFORM) to $OUT_PATH."
echo ">> Downloading: $DOWNLOAD_URL"

if ! curl -Lfo "$TMP_FILE" "$DOWNLOAD_URL"; then
    echo "Download failed"
    exit 1
fi

mkdir -p "$OUT_PATH"
tar -xzf "$TMP_FILE" -C "$OUT_PATH"
rm "$TMP_FILE"
