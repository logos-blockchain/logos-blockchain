#!/bin/bash

set -e

VERSION="${1:-0.2.0}"
PLATFORM="${2:-x86_64-linux}"

curl -L -O "https://github.com/logos-co/logos-logoscore-cli/releases/download/${VERSION}/logoscore-${PLATFORM}.tar.gz"
curl -L -O "https://github.com/logos-co/logos-package-manager/releases/download/${VERSION}/lgpm-${PLATFORM}.tar.gz"
curl -L -O "https://github.com/logos-co/logos-package-downloader/releases/download/${VERSION}/lgpd-${PLATFORM}.tar.gz"

tar -xvf logoscore-${PLATFORM}.tar.gz
tar -xvf lgpm-${PLATFORM}.tar.gz
tar -xvf lgpd-${PLATFORM}.tar.gz

rm logoscore-${PLATFORM}.tar.gz
rm lgpm-${PLATFORM}.tar.gz
rm lgpd-${PLATFORM}.tar.gz

mv logoscore-${ARCH}* logoscore
mv lgpm-${ARCH}* lgpm
mv lgpd-${ARCH}* lgpd

chmod +x logoscore lgpm lgpd

echo "Success! logoscore, lgpm and lgpd downloaded."
