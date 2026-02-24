#!/bin/sh

set -e

export CFG_SERVER_MODE="run" \
       CFG_SERVER_STORAGE_PATH="/node-data/cfgsync/deployment-settings.yaml" \
       ENTROPY_FILE="${ENTROPY_FILE:-/etc/logos-blockchain/entropy}"

exec /usr/bin/logos-blockchain-cfgsync-server /etc/logos-blockchain/cfgsync.yaml
