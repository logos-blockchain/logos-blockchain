#!/bin/sh

set -e

export CFG_FILE_PATH="/node-data/${LB_HOST_IDX}/config.yaml" \
       CFG_SERVER_ADDR="http://cfgsync:4400" \
       CFG_HOST_IDENTIFIER="i-${LB_HOST_IDX}" \
       CFG_DEPLOYMENT_PATH="/node-data/cfgsync/deployment-settings.yaml" \
       LOG_LEVEL="DEBUG" \
       LOG_BACKEND="file" \
       LOG_DIR="/node-data/${LB_HOST_IDX}/"

echo "Starting Faucet..."
/usr/bin/logos-blockchain-faucet \
    --port $FAUCET_PORT \
    --node-base-url "http://localhost:$CFG_API_PORT"\
    --host-identifier $CFG_HOST_IDENTIFIER \
    --drip-amount 1000 &

echo "Starting Node..."
exec /usr/bin/logos-blockchain-node --deployment $CFG_DEPLOYMENT_PATH $CFG_FILE_PATH
