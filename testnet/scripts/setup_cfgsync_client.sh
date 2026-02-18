#!/bin/sh

rm -rf /node-data/${LB_HOST_IDX}

set -e

export CFG_FILE_PATH="/node-data/${LB_HOST_IDX}/config.yaml" \
       CFG_SERVER_ADDR="http://cfgsync:4400" \
       CFG_HOST_IDENTIFIER="i-${LB_HOST_IDX}"

mkdir /node-data/${LB_HOST_IDX}

exec /usr/bin/logos-blockchain-cfgsync-client 
