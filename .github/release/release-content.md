## What's Changed

TODO: Changelog.

## Setup

If it's the first time configuring your environment, please do the following:

1. From the artifacts, download and unzip the circuits for your architecture.
2. Set the `LOGOS_BLOCKCHAIN_CIRCUITS` variable to the folder containing the circuits.

To run the binary, you will need to create a node config.

### Config generation

TODO.

## Run the binary

After generating the node config file to fit your needs, you can run the node binary.

For example: `logos-blockchain-node-macos-aarch64-0.0.1 node-config.yaml`. See the repo's `README.md` for more info.

To verify that your node is running correctly and connected, visit http://localhost:{api_port_in_user_config}/cryptarchia/info. The slot and height should both be constantly increasing.

## Checklist

Before publishing please ensure:
- [ ] Description is complete
- [ ] Changelog is correct, compared to last release
- [ ] Binaries for Mac and Linux platforms are present
- [ ] Circuits of the expected version for Mac and Linux platforms are present (need to be manually downloaded and included for now)
- [ ] Pre-release is checked if necessary
- [ ] Remove this checklist and address all TODOs before publishing the release.
