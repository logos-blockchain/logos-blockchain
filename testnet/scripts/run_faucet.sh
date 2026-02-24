export CFG_DEPLOYMENT_PATH="/node-data/cfgsync/deployment-settings.yaml" 

/usr/bin/logos-blockchain-faucet \
    --port $FAUCET_PORT \
    --node-base-url "http://localhost:$NODE_API_PORT" \
    --deployment-file $CFG_DEPLOYMENT_PATH \
    --drip-amount 1000
