## 🚀 Quick Start

### 📦 Prerequisites

1. Download `logos-core` binaries:

- `lgpd` (Logos Package Downloader): https://github.com/logos-co/logos-package-downloader/releases/latest
- `lgpm` (Logos Package Manager): https://github.com/logos-co/logos-package-manager/releases/latest
- `logoscore` (Logos Core CLI): https://github.com/logos-co/logos-logoscore-cli/releases/latest

2. Download the Logos Blockchain module using the Logos Package Downloader:

```bash
lgpd download blockchain_module --version {{BLOCKCHAIN_MODULE_VERSION}} --output ./
# writes ./blockchain_module-{{BLOCKCHAIN_MODULE_VERSION}}.lgx
```

3. Install the Logos Blockchain module using the Logos Package Manager:

```bash
lgpm --modules-dir ./modules install --file blockchain_module-{{BLOCKCHAIN_MODULE_VERSION}}.lgx
```

4. Launch the Logos Core CLI in daemon mode and load the blockchain module:

```bash
logoscore -m ./modules -D &
logoscore load-module blockchain_module
```

### ⚙️ Initialize Your Node

Generate a default configuration by connecting to the bootstrap peers:

```bash
logoscore call blockchain_module generate_user_config '{
  "initial_peers": [
{{INITIAL_PEERS}}
  ]
}'
```

If your node has a known public IP address and you want to disable NAT traversal, you can add `--external-address /ip4/<public-ip>/udp/<port>/quic-v1` to the previous command. Nodes behind NAT or CG-NAT require no extra flags — NAT traversal is enabled by default.

This takes a few seconds and produces a `user_config.yaml` file.

### ▶️ Run Your Node

Run the node:

```bash
logoscore call blockchain_module start user_config.yaml ""
```

The node writes rotating log files (one per hour).

### ✅ Verify It Works

Check your local consensus state by querying your node's API:

```
logoscore call blockchain_module get_cryptarchia_info | jq -r .result.value | jq .
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
- [ ] Verify binaries are present for **Mac** and **Linux**
- [ ] Delete this checklist and publish

[release-notion]: https://www.notion.so/nomos-tech/Internal-Devnet-Launch-February-2026-2fe261aa09df8025ad94e380933b4cf9#2ff261aa09df8058935ecb85aa587564
[testnet-faucet]: https://testnet.blockchain.logos.co/web/faucet/
[devnet-faucet]: https://devnet.blockchain.logos.co/web/faucet/
[testnet-dashboard]: https://testnet.blockchain.logos.co/web/
[devnet-dashboard]: https://devnet.blockchain.logos.co/web/
[testnet-discord-public]: https://discord.com/channels/973324189794697286/1468535289604735038
