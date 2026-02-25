#!/bin/sh

rm -rf /node-data/cfgsync

set -e

export CFG_SERVER_MODE="setup" \
       CFG_SERVER_STORAGE_PATH="/node-data/cfgsync/deployment-settings.yaml" \
       CHAIN_START_TIME=$(date -u +"%Y-%m-%dT%H:%M:%SZ") \
       ENTROPY_FILE="${ENTROPY_FILE:-/etc/logos-blockchain/entropy}"

mkdir /node-data/cfgsync

exec /usr/bin/logos-blockchain-cfgsync-server /etc/logos-blockchain/cfgsync.yaml
