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

## Devnet setup

If you wish to join the devnet at https://devnet.blockchain.logos.co, you can automatically generate and download your node configuration using the following command:

```bash
curl -X POST -L --location-trusted https://devnet.blockchain.logos.co/node/0/cfgsync/generate-config \
     -u "username:password" \
     -H "Content-Type: application/json" \
     -d '{
            "ip": "192.168.6.7",
            "identifier": "marcins-anonymous-node",
            "network_port": 3000,
            "blend_port": 4000,
            "api_port": 8080
         }' \
     -o my_logos_node_config.yaml
```

## Run the binary

After obtaining the deployment file for the network you want to join and tweaking the node config file to fit your needs, including specifying the list of bootnodes for the network you are joining, you can run the node binary.

For example: `logos-blockchain-node-macos-aarch64-0.0.1 --deployment deployment.yaml node-config.yaml`. See the repo's `README.md` for more info.

To verify that your node is running correctly and connected, visit http://localhost:{api_port_in_user_config}/cryptarchia/info. The slot and height should both be constently increasing.

## Checklist

Before publishing please ensure:
- [ ] Description is complete
- [ ] Changelog is correct, compared to last release
- [ ] Binaries for Mac and Linux platforms are present
- [ ] Circuits of the expected version for Mac and Linux platforms are present (need to be manually downloaded and included for now )
- [ ] Example user config YAML file is present
- [ ] Pre-release is checked if necessary
- [ ] Remove this checklist and address all TODOs before publishing the release.
