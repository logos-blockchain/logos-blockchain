# Docker Compose Deployment for Logos Blockchain

The Logos blockchain Docker Compose deployment contains four distinct service types:

- **Logos Blockchain Node Services**: Multiple dynamically spawned Logos blockchain nodes that synchronizes their configuration via cfgsync utility.

## Building

Upon making modifications to the codebase or the Dockerfile, the Logos blockchain images must be rebuilt:

```bash
docker compose build
```

## Configuring

Configuration of the Docker deployment is accomplished using the `.env` file. An example configuration can be found in `.env.example`.

To adjust the count of Logos blockchain nodes, modify the variable:

```bash
DOCKER_COMPOSE_LIBP2P_REPLICAS=100
```

## Running

Initiate the deployment by executing the following command:

```bash
docker compose up
```

This command will merge all output logs and display them in Stdout. For a more refined output, it's recommended to first run:

```bash
docker compose up -d
```

Followed by:

```bash
docker compose logs -f logos-blockchain-node
```

## Using deployment

Bootstrap node is accessible from the host via `3000` and `18080` ports. To expose other Logos blockchain nodes, please update `logos-blockchain-node` service in the `compose.yml` file with this configuration:

```bash
  logos-blockchain-node-0:
    ports:
    - "3001-3010:3000" # Use range depending on the number of Logos blockchain node replicas.
    - "18081-18190:18080"
```

After running `docker compose up`, the randomly assigned ports can be viewed with `ps` command:

```bash
docker compose ps 
```

## Release & deployment file taxonomy

Deployment files fall into three categories by **how often they change**. This
makes it clear which files are reusable blueprints and which must be touched on
every release.

### 1. Template for all deployments (any type)
Shared blueprints reused by every deployment type; not edited per release.

- `compose.yml`, `compose.run.yml`, `compose.setup.yml`, `deployment/compose.tracing.yml`
- `Dockerfile`, `deployment/Dockerfile`
- `deployment/cfgsync.yaml`, `deployment/cfgsync/deployment-settings.yaml`
- `deployment/nginx/*`, `deployment/scripts/*`, `deployment/systemd/*`

### 2. Template for a certain deployment type
Per-type blueprints; change only when a network type is re-defined.

- `deployment/ceremony/genesis/<env>/template/*` — see [`ceremony/genesis/README.md`](ceremony/genesis/README.md)
- The `# DEPLOYMENT TYPE` section of `.env.devnet` / `.env.testnet`
  (`TOOLS_IMAGE_LABEL`, `EXPLORER_IMAGE_LABEL`, `ENV_TITLE_STRING`,
  `PUBLIC_IP_ADDR`, `DOCKER_COMPOSE_LIBP2P_REPLICAS`, node ports)

### 3. Per-release info (edited on every release)

- `deployment/ceremony/genesis/<env>/inscribe.yaml` — `chain_id`, `genesis_time`
- `NODE_IMAGE_LABEL` in `.env.devnet` / `.env.testnet` (the `# PER-RELEASE` section)
- The `version` input of the genesis ceremony workflow (fills `VERSION_PLACEHOLDER`)

Generated each release (by the ceremony, not hand-edited):
`nodes/node/binary/src/config/deployment/settings.yaml`,
`nodes/node/standalone-deployment-config.yaml`.
