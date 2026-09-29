#!/bin/sh

set -e

export CFG_FILE_PATH="/node-data/${LB_HOST_IDX}/user_config.yaml" \
       CFG_HOST_IDENTIFIER="i-${LB_HOST_IDX}" \
       CFG_DEPLOYMENT_PATH="/node-data/deployment.yaml" \
       LOG_BACKEND="file" \
       LOG_DIR="/node-data/${LB_HOST_IDX}/" \
       LOGOSCTL_CONFIG_DIR="/opt/logos-session"

(
    until logosctl daemon status > /dev/null 2>&1; do
        sleep 2
    done

    logosctl module load blockchain_module
    logosctl call blockchain_module start "$CFG_FILE_PATH" "$CFG_DEPLOYMENT_PATH"

    echo "Logos Blockchain Module started."
) &

exec logosctl daemon start
