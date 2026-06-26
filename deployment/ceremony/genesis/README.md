# Genesis ceremony inputs

These files are the inputs to the genesis ceremony — the CLI step that builds a
network's embedded deployment settings. They are split by **how often each file
changes**, so it is clear which files are reusable blueprints and which must be
edited on every release.

## Layout

```
deployment/ceremony/genesis/
  <env>/                          # env ∈ { devnet, testnet, standalone }
    inscribe.yaml                 # PER-RELEASE  — edited on every release
    template/                     # PER-TYPE     — blueprint for this env
      deployment-template.yaml    #   consensus / network / blend params
      stakeholders.yaml           #   genesis stake distribution
      providers.yaml              #   bootstrap providers (id, locators)
      faucet.yaml                 #   faucet identity + funds
```

### `template/` — per-type blueprint (rarely changes)
The files under `template/` describe the *shape* of a given network type and only
change when the network itself is re-defined (new validator set, new bootstrap
providers, different consensus parameters). They are **not** touched on a routine
release. `deployment-template.yaml` carries `ENV_PLACEHOLDER` / `VERSION_PLACEHOLDER`
tokens that CI substitutes at ceremony time.

### `inscribe.yaml` — per-release (edited every release)
This is the one file in each `<env>/` directory that changes on every release:

- `chain_id` — embeds the version being released (e.g. `devnet-X.Y.Z-rc.N`,
  `testnet-X.Y.Z`); `standalone` stays `standalone-local`.
- `genesis_time` — set ~10 min into the future (ISO 8601) for the next genesis.
- `entropy_sources` — left as-is.

The version is **also** supplied out-of-band as the `version` input to the
genesis ceremony workflow (it fills `VERSION_PLACEHOLDER`), so keep the two in sync.

## How the ceremony consumes these (input → output)

The `logos-blockchain-tools-genesis ceremony` command
(`tools/blockchain-tools/src/bin/genesis.rs`) reads `inscribe.yaml` plus all of
`template/*` and writes a fully-resolved deployment settings file:

| Trigger | Inputs | Output (committed) |
| --- | --- | --- |
| `.github/workflows/genesis-ceremony.yml` (devnet / testnet) | `<env>/inscribe.yaml` + `<env>/template/*` | `nodes/node/binary/src/config/deployment/settings.yaml` |
| `scripts/standalone-genesis-ceremony.sh` (local) | `standalone/inscribe.yaml` + `standalone/template/*` | `nodes/node/standalone-deployment-config.yaml` |

The generated output (not these inputs) is what the node binary embeds and what
`code-check.yml` / config tests validate.

See [`../../README.md`](../../README.md) → "Release & deployment file taxonomy"
for how these files relate to the `.env.*` and `compose*.yml` deployment files.
