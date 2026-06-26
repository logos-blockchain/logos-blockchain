./target/debug/logos-blockchain-tools-genesis ceremony \
  --inscription-params deployment/standalone/ceremony/genesis/inscribe.yaml \
  --stake-holders deployment/standalone/ceremony/genesis/template/stakeholders.yaml \
  --providers deployment/standalone/ceremony/genesis/template/providers.yaml \
  --faucet deployment/standalone/ceremony/genesis/template/faucet.yaml \
  --deployment deployment/standalone/ceremony/genesis/template/deployment-template.yaml \
  --output nodes/node/standalone-deployment-config.yaml