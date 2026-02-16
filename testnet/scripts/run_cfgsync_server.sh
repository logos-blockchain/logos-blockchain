#!/bin/sh

set -e

export CFG_SERVER_MODE="run" \
       CFG_SERVER_STORAGE_PATH="/node-data/cfgsync/deploymen-settings.yaml"

exec /usr/bin/logos-blockchain-cfgsync-server /etc/logos-blockchain/cfgsync.yaml
