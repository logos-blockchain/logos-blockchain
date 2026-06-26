./target/debug/logos-blockchain-tools-genesis ceremony \
  --inscription-params deployment/ceremony/genesis/standalone/inscribe.yaml \
  --stake-holders deployment/ceremony/genesis/standalone/template/stakeholders.yaml \
  --providers deployment/ceremony/genesis/standalone/template/providers.yaml \
  --faucet deployment/ceremony/genesis/standalone/template/faucet.yaml \
  --deployment deployment/ceremony/genesis/standalone/template/deployment-template.yaml \
  --output nodes/node/standalone-deployment-config.yaml