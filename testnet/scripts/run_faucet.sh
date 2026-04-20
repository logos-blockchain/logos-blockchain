#!/bin/sh

set -e

export CFG_DEPLOYMENT_PATH="/node-data/cfgsync/deployment-settings.yaml" 

/usr/bin/logos-blockchain-faucet \
    --port $FAUCET_PORT \
    --node-base-url $NODE_API_ADDR \
    --deployment-file $CFG_DEPLOYMENT_PATH \
    --drip-amount 1000

