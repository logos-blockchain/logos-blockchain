./target/debug/logos-blockchain-tools-genesis ceremony \
  --inscription-params deployment/ceremony/genesis/standalone/inscribe.yaml \
  --stake-holders deployment/ceremony/genesis/standalone/stakeholders.yaml \
  --providers deployment/ceremony/genesis/standalone/providers.yaml \
  --faucet deployment/ceremony/genesis/standalone/faucet.yaml \
  --deployment deployment/ceremony/genesis/standalone/deployment-template.yaml \
  --output nodes/node/standalone-deployment-config.yaml

# Keep the binary's embedded built-in deployment (the master default, used when
# no --deployment flag is passed) in sync with the standalone config above.
cp nodes/node/standalone-deployment-config.yaml \
  nodes/node/binary/src/config/deployment/builtin/deployment.yaml
