#!/bin/sh

set -e

export CFG_FILE_PATH="/node-data/${LB_HOST_IDX}/config.yaml" \
       CFG_SERVER_ADDR="http://cfgsync:4400" \
       CFG_HOST_IDENTIFIER="i-${LB_HOST_IDX}" \
       CFG_DEPLOYMENT_PATH="/node-data/cfgsync/deployment-settings.yaml" \
       LOG_LEVEL="DEBUG" \
       LOG_BACKEND="file" \
       LOG_DIR="/node-data/${LB_HOST_IDX}/" \
       STATE_PATH="/node-data/${LB_HOST_IDX}/state"

exec /usr/local/bin/logos-blockchain-node --deployment $CFG_DEPLOYMENT_PATH $CFG_FILE_PATH
