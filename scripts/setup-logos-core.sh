#!/bin/bash

set -e

VERSION="${1}"
PLATFORM="${2:-x86_64-linux}"

# 0.3.0 merged logoscore, lgpd and lgpm into a single `logosctl` binary.
curl -L -O "https://github.com/logos-co/logos-logoscore-cli/releases/download/${VERSION}/logosctl-${PLATFORM}.tar.gz"

tar -xvf logosctl-${PLATFORM}.tar.gz

rm logosctl-${PLATFORM}.tar.gz

# The tarball holds logosctl-<arch>.AppImage, without the platform suffix.
mv logosctl-*.AppImage logosctl

chmod +x logosctl

echo "Success! logosctl downloaded."
