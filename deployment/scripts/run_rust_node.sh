#!/bin/sh

set -e

export CFG_FILE_PATH="/node-data/${LB_HOST_IDX}/user_config.yaml" \
       CFG_HOST_IDENTIFIER="i-${LB_HOST_IDX}" \
       CFG_DEPLOYMENT_PATH="/node-data/deployment.yaml" \
       LOG_BACKEND="file" \
       LOG_DIR="/node-data/${LB_HOST_IDX}/" \
       LOGOSCTL_CONFIG_DIR="/opt/logos-session"

exec /usr/bin/logos-blockchain-node $CFG_FILE_PATH
