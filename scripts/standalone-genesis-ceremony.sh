./target/debug/logos-blockchain-tools-genesis ceremony \
  --inscription-params deployment/environments/standalone/ceremony/genesis/inscribe.yaml \
  --stake-holders deployment/environments/standalone/ceremony/genesis/template/stakeholders.yaml \
  --providers deployment/environments/standalone/ceremony/genesis/template/providers.yaml \
  --faucet deployment/environments/standalone/ceremony/genesis/template/faucet.yaml \
  --deployment deployment/environments/standalone/ceremony/genesis/template/deployment-template.yaml \
  --output nodes/node/standalone-deployment-config.yaml