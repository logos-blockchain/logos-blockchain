./target/debug/logos-blockchain-tools-genesis ceremony \
  --inscription-params deployment/ceremony/genesis/standalone/inscribe.yaml \
  --stake-holders deployment/ceremony/genesis/standalone/stakeholders.yaml \
  --providers deployment/ceremony/genesis/standalone/providers.yaml \
  --faucet deployment/ceremony/genesis/standalone/faucet.yaml \
  --deployment deployment/ceremony/genesis/standalone/deployment-template.yaml \
  --output nodes/node/standalone-deployment-config.yaml
