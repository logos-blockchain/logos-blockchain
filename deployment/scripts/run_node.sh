#!/bin/sh

set -e

export CFG_FILE_PATH="/node-data/${LB_HOST_IDX}/user_config.yaml" \
       CFG_SERVER_ADDR="http://cfgsync:4400" \
       CFG_HOST_IDENTIFIER="i-${LB_HOST_IDX}" \
       CFG_DEPLOYMENT_PATH="/node-data/deployment.yaml" \
       LOG_BACKEND="file" \
       LOG_DIR="/node-data/${LB_HOST_IDX}/"

(
    until /opt/logoscore/AppRun load-module blockchain_module > /dev/null 2>&1; do
        sleep 2
    done

    /opt/logoscore/AppRun call blockchain_module start "$CFG_FILE_PATH" "$CFG_DEPLOYMENT_PATH"

    echo "Logos Blockchain Module started."
) &

exec /opt/logoscore/AppRun -m /opt/modules -D
