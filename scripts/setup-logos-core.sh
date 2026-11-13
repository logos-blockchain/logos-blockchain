#!/bin/bash

set -e

VERSION="${1}"
PLATFORM="${2:-x86_64-linux}"
ARCHIVE="logosctl-${VERSION}-${PLATFORM}.tar.gz"
CHECKSUMS="$(dirname "$0")/logosctl.sha256"

# Only an archive whose SHA-256 is pinned in logosctl.sha256 is accepted, so a replaced release asset is refused.
PINNED=$(awk -v archive="$ARCHIVE" '$2 == archive' "$CHECKSUMS")
if [ -z "$PINNED" ]; then
  echo "No SHA-256 pinned for ${ARCHIVE} in ${CHECKSUMS}. Add it before using logosctl ${VERSION}." >&2
  exit 1
fi

# 0.3.0 merged logoscore, lgpd and lgpm into a single `logosctl` binary.
curl -fL -o "$ARCHIVE" "https://github.com/logos-co/logos-logoscore-cli/releases/download/${VERSION}/logosctl-${PLATFORM}.tar.gz"

echo "$PINNED" | sha256sum -c -

tar -xvf "$ARCHIVE"

rm "$ARCHIVE"

# The tarball holds logosctl-<arch>.AppImage, without the platform suffix.
mv logosctl-*.AppImage logosctl

chmod +x logosctl

echo "Success! logosctl downloaded."
