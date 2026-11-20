#!/bin/sh

set -e

CFG_FILE_PATH="/node-data/${LB_HOST_IDX}/user_config.yaml"
CFG_DEPLOYMENT_PATH="/node-data/deployment.yaml"

export LOG_BACKEND="file" \
       LOG_DIR="/node-data/${LB_HOST_IDX}/"

# Upgrades a user config of an earlier release in place, keeping the original
# next to it, and stops here when the config is not valid.
logos-blockchain-node "$CFG_FILE_PATH" --deployment "$CFG_DEPLOYMENT_PATH" --check-config

exec logos-blockchain-node "$CFG_FILE_PATH" --deployment "$CFG_DEPLOYMENT_PATH"
