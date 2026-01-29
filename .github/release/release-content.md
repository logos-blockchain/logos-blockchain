Logos Blockchain Node ${{ env.VERSION }}.

## What's Changed

TODO: Changelog.

## Setting up

If it's the first time configuring your environment, please do the following:

1. Download and unzip the circuits for your architecture.
2. Set the `LOGOS_BLOCKCHAIN_CIRCUITS` variable to the folder containing the circuits.

## Run the binary
Specify user and the deployment config for the network you want to join (see below).
For example: `logos-blockchain-node --deployment deployment.yaml config.yaml`. See the repo's `README.md` for more info.

## Available Networks

* Internal devnet: use deployment file `internal-devnet-config.yaml` and TODO: bootstrap nodes.

## Checklist

Before publishing please ensure:
- [ ] Description is complete
- [ ] Changelog is correct, compared to last release
- [ ] Bundles for Mac and Linux platforms are present
- [ ] Mac and Linux circuits are present
- [ ] Deployment configs for well-known networks are present
- [ ] List of bootstrap peers for well-known networks are present
- [ ] Pre-release is checked if necessary
- [ ] Remove this checklist and address all TODOs before publishing the release.