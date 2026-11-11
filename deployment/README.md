# Docker Compose Deployment for Logos Blockchain

The Compose files in this directory:

- `compose.yml`: one node built from this checkout, running the standalone one-node network
  (`nodes/node/standalone-node-config.yaml` and `nodes/node/standalone-deployment-config.yaml`).
- `compose.setup.yml` and `compose.run.yml`: the devnet and testnet stack, from published images, configured by
  the `.env.<env>` files.
- `compose.tracing.yml`: Grafana, Loki, Prometheus and an OpenTelemetry collector.

## Building

Run all the `docker compose` commands below from this `deployment/` directory, where the Compose files (`compose.yml`, `compose.run.yml`, `compose.setup.yml`) live.

Upon making modifications to the codebase or the Dockerfile, the Logos blockchain images must be rebuilt:

```bash
docker compose build
```

## Configuring

`compose.yml` takes no configuration. The devnet and testnet stack is configured with an `.env` file in this directory. A documented example is in `.env.example`, and each environment has its own file (`.env.devnet`, `.env.testnet`); select one by pointing Compose at it, e.g. `docker compose --env-file .env.devnet -f compose.setup.yml up`, or by copying it to the default `.env`.

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

### Container user

The node and tools images run as the unprivileged user `logos`, with UID and GID `10001`. A new `node-data`
volume takes that ownership from the tools image. A volume created before, when the containers ran as root,
has to be handed over once, with the stack stopped (`docker volume ls` shows its full name):

```bash
docker run --rm -v <project>_node-data:/node-data debian:trixie-slim chown -R 10001:10001 /node-data
```

Files put into the volume from outside, such as a node's `user_config.yaml` and `keystore.yaml`, must belong
to `10001` too. A node refuses a keystore that users other than its owner can access: keep `keystore.yaml` at
mode `600`.

## Using deployment

The node of `compose.yml` serves its API inside the container only:

```bash
docker compose exec logos-blockchain-node curl -s localhost:8080/cryptarchia/info
```

### Reaching the node API by name

A node answers to `localhost` and to IP addresses. A client that reaches its API by any other host name gets
`403 Forbidden` until that name is listed in the node's user config, which is what stops a web page from reaching the
API through DNS rebinding. In `compose.run.yml` the explorer and the faucet both call node 0 by name, so that node needs:

```yaml
api:
  backend:
    allowed_hosts:
      - logos-blockchain-node-0
      - logos_blockchain_node_0
```

Web pages are refused the same way, unless the node serves them itself: list a page's origin in
`api.backend.cors_origins`, such as `https://app.example`, for it to use the API. There is no wildcard for either
setting.

## Release & deployment file taxonomy

Deployment files fall into three categories by **how often they change**. This
makes it clear which files are reusable blueprints and which must be touched on
every release.

### Genesis ceremony layout

Genesis ceremony inputs are grouped by environment under `deployment/ceremony/genesis/`
(`env` ∈ `devnet`, `testnet`, `standalone`). Per-environment dirs sit next to an
optional `shared/` dir, so files common to every environment have a natural home:

```
deployment/ceremony/genesis/
  shared/                         # (optional) inputs common to ALL environments
  <env>/
    inscribe.yaml                 # TEMPLATE entropy/PoW reward pool + PER-RELEASE chain_id/genesis_time
    deployment-template.yaml      # PER-TYPE eras, keyed by first epoch (consensus / blend / time)
    stakeholders.yaml             # PER-TYPE genesis stake distribution
    providers.yaml                # PER-TYPE bootstrap providers (id, locators)
    consensus_excluded_keys.yaml  # PER-TYPE consensus-excluded identity + funds
```

The per-environment `.env` files (`.env.devnet`, `.env.testnet`) live in
`deployment/` next to `compose.yml`, since they are consumed by Docker Compose.

The genesis ceremony (`logos-blockchain-tools-genesis ceremony`,
`tools/blockchain-tools/src/bin/genesis.rs`) reads all input files in the
`<env>/` dir and writes a fully-resolved settings file. The generated output
(not these inputs) is what the node embeds and what `code-check.yml` / config
tests validate:

| Trigger | Inputs | Output (committed) |
| --- | --- | --- |
| `.github/workflows/genesis-ceremony.yml` (devnet / testnet) | `deployment/ceremony/genesis/<env>/*` | `nodes/node/binary/src/config/deployment/settings.yaml` and the era files it includes, `era/schedule/<era number>.yaml` next to it |
| `scripts/standalone-genesis-ceremony.sh` (local) | `deployment/ceremony/genesis/standalone/*` | `nodes/node/standalone-deployment-config.yaml` and the era files it includes, `era/schedule/<era number>.yaml` next to it |

### Eras

A deployment schedules its eras under `eras`, each keyed by the epoch it
starts at; the first starts at epoch 0. The deployments the ceremony writes,
the binary's default and the standalone one, keep each era in its own file,
named after its era number (its place in the schedule, from 0), which the main
file includes (`settings.yaml` for the default):

```yaml
eras:
  0: !include era/schedule/0.yaml
  40: !include era/schedule/1.yaml
genesis_block:
  ...
```

To schedule an era in the default deployment, add its file to
`nodes/node/binary/src/config/deployment/era/schedule/`, named after its era
number, and include it from `settings.yaml` under the epoch it starts at. The
node's build script embeds every `.yaml` file in `era/schedule/` in the binary,
and a config test fails when a file there is not included, an include names no
file there, or an era's file is not named after its era number.

Any deployment file passed with `--deployment` can include era files the same
way, relative to itself, so they must be shipped next to it:
`compose.setup.yml` mounts the `era/schedule/` directory and
`cleanup_node_data.sh` copies it to `/node-data/era/schedule/`, next to
`/node-data/deployment.yaml`.

The ceremony writes the eras of its environment's `deployment-template.yaml`,
which keys them by the epoch each starts at, as `eras` does above, each
written out in full, every section under its version tag: an era takes no
value from the era before it. YAML anchors can share a section two eras have
in common, whole: a section that changes is written out in full.

```yaml
0: !V1
  blend:
    ...
  cryptarchia:
    ...
  time: &time
    slot_duration_in_seconds: 1
40: !V1
  blend:
    ...
  cryptarchia:
    ...
  time: *time
```

The ceremony checks the whole schedule as the node does, and writes each era
to its own file. Re-running it generates the genesis block again and removes
the era files of the schedule it replaces, so to append an era to a chain that
is already running, add its file and include it by hand, as above.

The node checks every era when it loads a deployment, and refuses to start on
one it cannot run: values the specifications rule out (a slot activation
coefficient `f` of 1 or more, an uncle reference window `W` wider than `⌊0.6k⌋`,
a Blend peering degree outside 3 to 5, ...), values the node cannot compute with
(`f` below 1/1000, `⌊k/4f⌋` of zero, ...), an era whose `W·f⁻¹` exceeds a fifth of
the previous era's finalization window `3⌊k/f⌋`, and an era that ends within the
transition period it opens. `--check-config` runs the same checks.

A release may append eras, or change eras that have not begun, but must not
change an era a node has already executed. A node refuses to start with a
deployment that gives the slot of its tip another fork digest than the one the
tip was applied under: run the release the node ran with, or remove the node's
state and sync again.

Protocol and gossipsub topic names are not configured: every node derives them
from its deployment. Blend, chain sync and the cryptarchia and mempool topics are
named `/logos-blockchain/<fork digest>/<protocol>`, so nodes on different forks
never talk to each other. Kademlia and identify are named
`/logos-blockchain/<genesis block ID>/<protocol>`, so peers of a chain still find each other across its forks.

### 1. Template for all deployments (any type)
Shared blueprints reused by every deployment type; not edited per release.

- `deployment/compose.yml`, `deployment/compose.run.yml`, `deployment/compose.setup.yml`, `deployment/compose.tracing.yml`
- `Dockerfile`, `deployment/Dockerfile`
- `deployment/nginx/*`, `deployment/scripts/*`, `deployment/systemd/*`
- the `# TEMPLATE` section (`entropy_sources`, `pow_reward_pool`) of each `deployment/ceremony/genesis/<env>/inscribe.yaml`

### 2. Template for a certain deployment type
Per-type blueprints; change only when a network type is re-defined.

- The blueprint files in `deployment/ceremony/genesis/<env>/` (`deployment-template.yaml`, `stakeholders.yaml`, `providers.yaml`,
  `consensus_excluded_keys.yaml`)
- The `# DEPLOYMENT TYPE` section of `.env.<env>`
  (`TOOLS_IMAGE_LABEL`, `EXPLORER_IMAGE_LABEL`, `ENV_TITLE_STRING`,
  `PUBLIC_IP_ADDR`, node ports)

### 3. Per-release info (edited on every release)

- The `# PER-RELEASE` section (`chain_id`, `genesis_time`) of `deployment/ceremony/genesis/<env>/inscribe.yaml`
- `NODE_IMAGE_LABEL` in `.env.<env>` (its `# PER-RELEASE` section)
- The `version` input of the genesis ceremony workflow (named in the ceremony's commit message)

Generated each release (by the ceremony, not hand-edited):
`nodes/node/binary/src/config/deployment/settings.yaml` and its era files, in
`nodes/node/binary/src/config/deployment/era/schedule/`, and
`nodes/node/standalone-deployment-config.yaml` and its era files, in
`nodes/node/era/schedule/`.