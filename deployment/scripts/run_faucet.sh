#!/bin/sh

set -e

/usr/bin/logos-blockchain-faucet \
    --port $FAUCET_PORT \
    --node-base-url $NODE_API_ADDR \
    --faucet-pk $FAUCET_PK \
    --drip-amount 1000000000000

