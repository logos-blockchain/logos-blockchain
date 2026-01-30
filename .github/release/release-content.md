## What's Changed

TODO: Changelog.

## Setup

If it's the first time configuring your environment, please do the following:

1. From the artifacts, download and unzip the circuits for your architecture.
2. Set the `LOGOS_BLOCKCHAIN_CIRCUITS` variable to the folder containing the circuits.

To run the binary, you will need two configuration files: a deployment config and a node config.

For the former, please reach out to the Logos Blockchain team on [Discord](https://discord.gg/CXnvqEG7) to get a copy of it and a list of bootnode addresses for the network you intend to join.

For the latter, you can download the example config from this release and tweak it to your needs.
Please check the docs for info on what each field means.

## Run the binary

After obtaining the deployment file for the network you want to join and tweaking the node config file to fit your needs, including specifying the list of bootnodes for the network you are joining, you can run the node binary.

For example: `logos-blockchain-node-macos-aarch64-0.0.1 --deployment deployment.yaml node-config.yaml`. See the repo's `README.md` for more info.

## Checklist

Before publishing please ensure:
- [ ] Description is complete
- [ ] Changelog is correct, compared to last release
- [ ] Binaries for Mac and Linux platforms are present
- [ ] Circuits of the expected version for Mac and Linux platforms are present (need to be manually downloaded and included for now )
- [ ] Example user config YAML file is present
- [ ] Pre-release is checked if necessary
- [ ] Remove this checklist and address all TODOs before publishing the release.