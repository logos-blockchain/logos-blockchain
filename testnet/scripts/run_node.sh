#!/bin/sh

set -e

export CFG_FILE_PATH="/config.yaml" \
       CFG_SERVER_ADDR="http://cfgsync:4400" \
       CFG_HOST_IDENTIFIER="validator-$CFG_API_PORT" \
       LOG_LEVEL="INFO" \
       POL_PROOF_DEV_MODE=true

/usr/bin/logos-blockchain-cfgsync-client

echo "Starting Faucet..."
/usr/bin/logos-blockchain-faucet \
    --port $FAUCET_PORT \
    --node-base-url "http://localhost:$CFG_API_PORT"\
    --host-identifier $CFG_HOST_IDENTIFIER
    --drip-amount 1000 &

echo "Starting Node..."
exec /usr/bin/logos-blockchain-node /config.yaml
