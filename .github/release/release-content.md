## 🚀 Quick Start

### 📦 Prerequisites

1. Download and unzip the **circuits** for your architecture from the release artifacts.
2. Rename the downloaded `logos-blockchain-circuits` to `.logos-blockchain-circuits` and move it to your home directory:

   ```bash
    mv logos-blockchain-circuits ~/.logos-blockchain-circuits
   ```

3. Download and unzip the **node binary** for your architecture:

   ```bash
    tar -xzf logos-blockchain-node-<arch>-<version>.tar.gz
   ```

### ⚙️ Initialize Your Node

Generate a default configuration by connecting to the testnet bootstrap peers:

```bash
./logos-blockchain-node init \
    -p /ip4/65.109.51.37/udp/3000/quic-v1/p2p/{TODO} \
    -p /ip4/65.109.51.37/udp/3001/quic-v1/p2p/{TODO} \
    -p /ip4/65.109.51.37/udp/3002/quic-v1/p2p/{TODO} \
    -p /ip4/65.109.51.37/udp/3003/quic-v1/p2p/{TODO}
```

If your node has a known public IP address and you want to disable NAT traversal, you can add `--external-address /ip4/<public-ip>/udp/<port>/quic-v1` to the previous command. Nodes behind NAT or CG-NAT require no extra flags — NAT traversal is enabled by default.

This takes a few seconds and produces a `user_config.yaml` file.

### ▶️ Run Your Node

Run the node:

```bash
./logos-blockchain-node user_config.yaml
```

The node writes rotating log files (one per hour).

### ✅ Verify It Works

Check your local consensus state by querying your node's API, by default listening on port `8080`:

```
curl -w "\n" http://localhost:8080/cryptarchia/info
```

Your node should be in `Bootstrapping` mode for a few minutes, with both `slot` and `height` steadily increasing.

After boostrapping is complete, your node will move to `Online` mode.
You can compare against the fleet nodes at the [Logos testnet dashboard][testnet-dashboard].

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

Visit the [testnet faucet][testnet-faucet] and enter the credentials provided by the Logos Blockchain team (you can reach out to them on [Discord][testnet-discord-public]), then paste your wallet key.

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

## 📝 Inscribing

Start publishing messages to the blockchain using the built in text sequencer:

```bash
./logos-blockchain-node inscribe
```

---

## 🛟 Troubleshooting

Having issues? Reach out to the Logos Blockchain team on [Discord][testnet-discord-public] or check the [testnet Notion page][release-notion] for FAQs and up-to-date instructions.

---

## [REMOVE BEFORE PUBLISHING] Release Checklist

> **Internal — remove this section before publishing.**

- [ ] Auto-generate the changelog (GitHub feature) using the tag of the previous release, then move the changelog section to the **top** of the release notes
- [ ] Verify binaries are present for **Mac** and **Linux**
- [ ] Verify circuits of the expected version are present for **Mac** and **Linux**
- [ ] Replace `{TODO}` peer IDs by visiting the [testnet dashboard][testnet-dashboard] and copying each node's address + peer ID from their network info
- [ ] Set the release type: check **pre-release** or **latest** as appropriate
- [ ] Delete this checklist and publish

[release-notion]: https://www.notion.so/nomos-tech/Internal-Devnet-Launch-February-2026-2fe261aa09df8025ad94e380933b4cf9#2ff261aa09df8058935ecb85aa587564
[testnet-faucet]: https://testnet.blockchain.logos.co/web/faucet/
[testnet-dashboard]: https://testnet.blockchain.logos.co/web/
[testnet-discord-public]: https://discord.com/channels/973324189794697286/1468535289604735038
