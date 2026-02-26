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

To verify that your node is running correctly and connected, visit `http://localhost:{api_port_in_user_config}/cryptarchia/info`. The slot and height should both be constantly increasing.

You can compare your consensus state with any nodes of the Logos Blockchain fleet reachable at [https://devnet.blockchain.logos.co/web/](https://devnet.blockchain.logos.co/web/) by checking their cryptarchia info.

## Checklist

Before publishing please ensure:
- [ ] Description is complete
- [ ] Auto-generate the changelog (GH feature) by selecting the tag corresponding to the previous release. GH will add the changelog to the end of the release notes. Move the whole section at the top of the release notes instead
- [ ] Verify binaries for Mac and Linux platforms are present
- [ ] Verify circuits of the expected version for Mac and Linux platforms are present
- [ ] Check either the pre-release or "latest" checkbox, depending on the type of release
- [ ] Remove this checklist once fully addressed and publish the release

[release-notion]: https://www.notion.so/nomos-tech/Internal-Devnet-Launch-February-2026-2fe261aa09df8025ad94e380933b4cf9#2ff261aa09df8058935ecb85aa587564