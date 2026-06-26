# Docker Compose Deployment for Logos Blockchain

The Logos blockchain Docker Compose deployment contains four distinct service types:

- **Logos Blockchain Node Services**: Multiple dynamically spawned Logos blockchain nodes that synchronizes their configuration via cfgsync utility.

## Building

Upon making modifications to the codebase or the Dockerfile, the Logos blockchain images must be rebuilt:

```bash
docker compose build
```

## Configuring

Configuration of the Docker deployment is accomplished using an `.env` file. A documented example can be found in `.env.example` at the repo root. Each deployment environment keeps its own env file at `deployment/<env>/.env` (e.g. `deployment/devnet/.env`); select one by pointing Compose at it, e.g. `docker compose --env-file deployment/devnet/.env up`, or by copying it to the default `.env`.

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

### Per-environment layout

Everything specific to a deployment environment lives under `deployment/<env>/`
(`env` ∈ `devnet`, `testnet`, `standalone`):

```
deployment/<env>/
  .env                            # PER-TYPE infra + PER-RELEASE NODE_IMAGE_LABEL
                                  #   (devnet / testnet only; standalone has none)
  ceremony/genesis/
    inscribe.yaml                 # TEMPLATE entropy + PER-RELEASE chain_id/genesis_time
    template/                     # PER-TYPE genesis blueprint
      deployment-template.yaml    #   consensus / network / blend params
      stakeholders.yaml           #   genesis stake distribution
      providers.yaml              #   bootstrap providers (id, locators)
      faucet.yaml                 #   faucet identity + funds
```

The genesis ceremony (`logos-blockchain-tools-genesis ceremony`,
`tools/blockchain-tools/src/bin/genesis.rs`) reads `inscribe.yaml` plus all of
`template/*` and writes a fully-resolved settings file. The generated output
(not these inputs) is what the node embeds and what `code-check.yml` / config
tests validate:

| Trigger | Inputs | Output (committed) |
| --- | --- | --- |
| `.github/workflows/genesis-ceremony.yml` (devnet / testnet) | `deployment/<env>/ceremony/genesis/{inscribe.yaml,template/*}` | `nodes/node/binary/src/config/deployment/settings.yaml` |
| `scripts/standalone-genesis-ceremony.sh` (local) | `deployment/standalone/ceremony/genesis/{inscribe.yaml,template/*}` | `nodes/node/standalone-deployment-config.yaml` |

### 1. Template for all deployments (any type)
Shared blueprints reused by every deployment type; not edited per release.

- `compose.yml`, `compose.run.yml`, `compose.setup.yml`, `deployment/compose.tracing.yml`
- `Dockerfile`, `deployment/Dockerfile`
- `deployment/cfgsync.yaml`, `deployment/cfgsync/deployment-settings.yaml`
- `deployment/nginx/*`, `deployment/scripts/*`, `deployment/systemd/*`
- the `# TEMPLATE` section (`entropy_sources`) of each `deployment/<env>/ceremony/genesis/inscribe.yaml`

### 2. Template for a certain deployment type
Per-type blueprints; change only when a network type is re-defined.

- `deployment/<env>/ceremony/genesis/template/*`
- The `# DEPLOYMENT TYPE` section of `deployment/<env>/.env`
  (`TOOLS_IMAGE_LABEL`, `EXPLORER_IMAGE_LABEL`, `ENV_TITLE_STRING`,
  `PUBLIC_IP_ADDR`, `DOCKER_COMPOSE_LIBP2P_REPLICAS`, node ports)

### 3. Per-release info (edited on every release)

- The `# PER-RELEASE` section (`chain_id`, `genesis_time`) of `deployment/<env>/ceremony/genesis/inscribe.yaml`
- `NODE_IMAGE_LABEL` in `deployment/<env>/.env` (its `# PER-RELEASE` section)
- The `version` input of the genesis ceremony workflow (fills `VERSION_PLACEHOLDER`)

Generated each release (by the ceremony, not hand-edited):
`nodes/node/binary/src/config/deployment/settings.yaml`,
`nodes/node/standalone-deployment-config.yaml`.