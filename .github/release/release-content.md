## 🚀 Quick Start

### 📦 Prerequisites

1. Install `logosctl`, the Logos Core CLI, as described in its [latest release](https://github.com/logos-co/logos-logoscore-cli/releases/latest).

2. Start the `logosctl` daemon, and add the catalog that publishes the Logos Blockchain module:

```bash
logosctl daemon start --detach
logosctl catalog add https://raw.githubusercontent.com/logos-blockchain/blockchain-modules-release/refs/heads/main/logos-repo.json
logosctl catalog refresh
```

3. Download the Logos Blockchain module, and check its SHA-256 against the one under **Verify Your Downloads** below:

```bash
logosctl package download blockchain_module --version {{BLOCKCHAIN_MODULE_VERSION}} --output ./
# writes ./blockchain_module-{{BLOCKCHAIN_MODULE_VERSION}}.lgx
sha256sum blockchain_module-{{BLOCKCHAIN_MODULE_VERSION}}.lgx  # on macOS: shasum -a 256 blockchain_module-{{BLOCKCHAIN_MODULE_VERSION}}.lgx
```

4. Install the module and load it:

```bash
logosctl install ./blockchain_module-{{BLOCKCHAIN_MODULE_VERSION}}.lgx -y
logosctl module load blockchain_module
```

### 🔐 Verify Your Downloads

- **Blockchain module**: `blockchain_module-{{BLOCKCHAIN_MODULE_VERSION}}.lgx` must have this SHA-256. Don't install it if it differs:

  ```
  MODULE_SHA256_PENDING
  ```

- **Node binaries** attached to this release: check an archive against `SHA256SUMS`, then check that this repository's release workflow built it, with the [GitHub CLI](https://cli.github.com/):

  ```bash
  sha256sum --ignore-missing -c SHA256SUMS  # on macOS: shasum -a 256 --ignore-missing -c SHA256SUMS
  gh attestation verify logos-blockchain-node-<os>-<arch>-{{BLOCKCHAIN_MODULE_VERSION}}.tar.gz --repo logos-blockchain/logos-blockchain
  ```

- **Docker image**: check that this repository's image workflow built it:

  ```bash
  gh attestation verify oci://ghcr.io/logos-blockchain/logos-blockchain-node:{{BLOCKCHAIN_MODULE_VERSION}} --repo logos-blockchain/logos-blockchain
  ```

### ⚙️ Initialize Your Node

Generate a default configuration that connects to the bootstrap peers. The arguments go in a file: `@user_config_args.json` passes its contents to the module, and `output` sets where the configuration is written:

```bash
cat > user_config_args.json <<EOF
{
  "output": "$PWD/user_config.yaml",
  "initial_peers": [
{{INITIAL_PEERS}}
  ]
}
EOF
logosctl call blockchain_module generate_user_config @user_config_args.json
```

If your node has a known public IP address and you want to disable NAT traversal, add `"external_address": "/ip4/<public-ip>/udp/<port>/quic-v1",` on a line after `"output"` before running the command. Nodes behind NAT or CG-NAT require nothing extra — NAT traversal is enabled by default.

This takes a few seconds and writes `user_config.yaml`, and its `keystore.yaml`, to the current directory.

### ▶️ Run Your Node

Run the node:

```bash
logosctl call blockchain_module start "$PWD/user_config.yaml" ""
```

The node writes rotating log files (one per hour).

### ✅ Verify It Works

Check your local consensus state by querying your node's API:

```bash
logosctl call blockchain_module get_cryptarchia_info --json | jq -r .result.value | jq .
```

Your node should be in `Bootstrapping` mode for a few minutes, with both `slot` and `height` steadily increasing.

After bootstrapping is complete, your node will move to `Online` mode.
If you have joined the devnet, you can compare against the fleet nodes at the [Logos devnet dashboard][devnet-dashboard].
For testnet, you can check the [Logos testnet dashboard][testnet-dashboard].

---

## 💰 Getting Funds

**1. 🔑 Find your wallet key**

```bash
grep -A3 known_keys user_config.yaml
```

Copy any of the listed key IDs. For example:

```yaml
known_keys:
    af391a0d7v29e5f7ca28281eca974146689f8f1c9b712380c07089dabcb60a8c: ...
    de3233cec107e6589f83d4f3094caa65c633b5b33601211353779dc01972ca14: ...
```

Either key can be used.

**2. 🚰 Request funds from the faucet**

Visit the [devnet faucet][devnet-faucet] or [testnet faucet][testnet-faucet], paste your wallet key into the **Destination Public Key (Hex)** field, and click **Request Funds**. (Questions? Reach the Logos Blockchain team on [Discord][testnet-discord-public].)

A word of caution - do not _powerclick_ your way through as only one request can be made per block! So if you want to receive funds more than once, wait until your balance increases before requesting new funds.

**3. 💸 Confirm your balance**

Wait 1-2 minutes for the transaction to land in a block, then:

```bash
curl -w "\n" http://localhost:8080/wallet/<my_key>/balance
```

Replace `<my_key>` with the key ID you funded.

---

## 🧱 Proposing Blocks

Approximately 3.5h (two epochs) after you receive funds from the faucet, your node will automatically start producing blocks. 🎉

---

## 🛟 Troubleshooting

Having issues? Reach out to the Logos Blockchain team on [Discord][testnet-discord-public] or check the [testnet Notion page][release-notion] for FAQs and up-to-date instructions.

---

## [REMOVE BEFORE PUBLISHING] Release Checklist

> **Internal — remove this section before publishing.**

- [ ] Generate the changelog (GitHub feature) using the tag of the previous release.
    * If this is the first release candidate, then the previous tag is the version of the latest release, e.g. `0.1.3-rc.1` compares against `0.1.2`
    * If this is a release candidate but not the first one that has a GH release, then the previous tag is the version of previous release candidate for this release, e.g., `0.1.3-rc.2` compares against `0.1.3-rc.1`
    * If this is an actual release, then the previous tag is the latest release, e.g., `0.1.3` compares against `0.1.2`
- [ ] Verify binaries are present for **Mac** and **Linux**, with `SHA256SUMS`, and that the commands under **Verify Your Downloads** accept one of them
- [ ] Replace `MODULE_SHA256_PENDING` with the SHA-256 of `blockchain_module` {{BLOCKCHAIN_MODULE_VERSION}} that the release issue records once the module is published
- [ ] Delete this checklist and publish

[release-notion]: https://www.notion.so/nomos-tech/Internal-Devnet-Launch-February-2026-2fe261aa09df8025ad94e380933b4cf9#2ff261aa09df8058935ecb85aa587564
[testnet-faucet]: https://testnet.blockchain.logos.co/web/faucet/
[devnet-faucet]: https://devnet.blockchain.logos.co/web/faucet/
[testnet-dashboard]: https://testnet.blockchain.logos.co/web/
[devnet-dashboard]: https://devnet.blockchain.logos.co/web/
[testnet-discord-public]: https://discord.com/channels/973324189794697286/1468535289604735038
