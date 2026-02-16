## What's Changed

TODO: Changelog.

## Setup

If it's the first time configuring your environment, please do the following:

1. From the artifacts, download and unzip the circuits for your architecture.
2. Set the `LOGOS_BLOCKCHAIN_CIRCUITS` variable to the folder containing the circuits.

To run the binary, you will need to create a node config.

### Config generation

Check the [Notion page][release-notion] for info about how to connect your node to the devnet!

## Run the binary

After generating the node config file to fit your needs, you can untar and run the node binary.

To untar the binary, run:

`tar -xzf logos-blockchain-node-{your_architecture}-{binary_version}.tar.gz`, for instance `tar -xzf logos-blockchain-node-macos-aarch64-0.0.1.tar.gz`.

The operation will give you the `logos-blockchain-node` binary, which you can now run. See the repo's `README.md` for more info.

To verify that your node is running correctly and connected, visit http://localhost:{api_port_in_user_config}/cryptarchia/info. The slot and height should both be constantly increasing.

## Checklist

Before publishing please ensure:
- [ ] Description is complete
- [ ] Changelog is correct, compared to last release
- [ ] Binaries for Mac and Linux platforms are present
- [ ] Circuits of the expected version for Mac and Linux platforms are present (need to be manually downloaded and included for now)
- [ ] Pre-release is checked if necessary
- [ ] Remove this checklist and address all TODOs before publishing the release.

[release-notion]: https://www.notion.so/nomos-tech/Internal-Devnet-Launch-February-2026-2fe261aa09df8025ad94e380933b4cf9#2ff261aa09df8058935ecb85aa587564