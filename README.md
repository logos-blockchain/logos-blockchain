# Logos Blockchain

**A privacy-preserving, censorship-resistant blockchain for decentralized network states.**

Logos Blockchain is a core component of the [Logos][logos-website] technology stack.
It combines zero-knowledge proofs, a mix network for anonymity, and a modular service architecture to provide a foundation for sovereign digital communities.

---

## Highlights

- **Privacy by design** — UTXO-based ledger with Groth16 zero-knowledge proofs (BN254 curve) for transaction privacy
- **Censorship resistance** — Built-in [Blend mix network](blend/) anonymizes block proposal messages before propagation
- **Cryptarchia consensus** — Ouroboros-family (Private) Proof-of-Stake with ZK-proven leader election
- **Modular architecture** — Compile-time composable services via the [Overwatch][overwatch-github] framework
- **L2-ready** — Channel inscriptions enable rollup sequencers to anchor data on-chain
- **Cross-language support** — C bindings ([`c-bindings/`](c-bindings/)) for embedding the node in non-Rust applications

---

## Quick Start

### Prerequisites

| Requirement | Details |
|---|---|
| **LLVM / Clang** | Required for RocksDB and C bindings |
| **ZK Circuits** | Downloaded via setup script (see below) |

### 1. Clone and install ZK circuits

```bash
git clone https://github.com/logos-co/logos-blockchain.git
cd logos-blockchain
```

<details>
<summary><b>Linux / macOS</b></summary>

```bash
./scripts/setup-logos-blockchain-circuits.sh
```

Circuits are installed to `~/.logos-blockchain-circuits/` by default.

> **macOS note:** The setup script automatically removes quarantine attributes from downloaded binaries since code-signing is not yet implemented.

</details>

<details>
<summary><b>Custom version or directory</b></summary>

```bash
# Specific version
./scripts/setup-logos-blockchain-circuits.sh v0.4.1

# Custom directory
./scripts/setup-logos-blockchain-circuits.sh v0.4.1 /opt/circuits
export LOGOS_BLOCKCHAIN_CIRCUITS=/opt/circuits
```

</details>

Verify the installation:

```bash
cargo test -p logos-blockchain-circuits-prover -p logos-blockchain-circuits-verifier
```

### 2. Build

```bash
cargo build -p logos-blockchain-node --release
```

### 3. Run a standalone node

Before launching, update `chain_start_time` to a recent timestamp:

```bash
# Generate a timestamp
date -u +"%Y-%m-%d %H:%M:%S.000000 +00:00:00"
```

Edit [`nodes/node/standalone-deployment-config.yaml`](nodes/node/standalone-deployment-config.yaml) and paste the timestamp into the `time.chain_start_time` field, then run:

```bash
target/release/logos-blockchain-node --deployment standalone-deployment-config.yaml nodes/node/standalone-node-config.yaml
```

The node stores state in the `state` directory. If you encounter issues on restart, try removing it.

### Docker

```bash
# Build
docker build -t logos-blockchain-node .

# Run (mount your config)
docker run -v "/path/to/node_config.yml:/node_config.yml" -v "/path/to/deployment_config.yml:/deployment_config.yml" logos-blockchain-node --deployment /deployment_config.yml /node_config.yml
```

---

## Project Structure

```
logos-blockchain/
├── core/                 Core types — blocks, transactions, UTXO notes, proofs
├── consensus/
│   ├── cryptarchia-engine/   Cryptarchia PoS consensus logic
│   └── cryptarchia-sync/     Chain synchronization over libp2p
├── blend/                Blend mix network
│   ├── crypto/               Cryptographic primitives
│   ├── message/              Message types
│   ├── network/              Network layer
│   ├── proofs/               ZK proofs (PoL, PoQ)
│   └── scheduling/           Cover traffic & delay scheduling
├── zk/                   Zero-knowledge proof infrastructure
│   ├── groth16/              Groth16 over BN254 (arkworks)
│   ├── poseidon2/            Poseidon2 hash function
│   ├── circuits/             Circuit prover, verifier, witness generator
│   └── proofs/               PoC, PoL, PoQ, ZK signatures
├── ledger/               UTXO-based ledger & state transitions
├── utxotree/             Persistent UTXO commitment tree
├── mmr/                  Merkle Mountain Range (header commitments)
├── kms/                  Key Management System (Ed25519, X25519, ZK keys)
├── libp2p/               Networking — QUIC, GossipSub, Kademlia, AutoNAT
├── services/             Overwatch services (chain, blend, wallet, API, …)
├── nodes/node/           Node binary — wires everything together
├── wallet/               Wallet logic (UTXO selection, key management)
├── zone-sdk/             SDK for building zone sequencers & indexers
├── c-bindings/           C-compatible dynamic library + header
├── testnet/              Docker Compose testnets, faucet, L2 demo
└── tests/                Integration & Cucumber BDD tests
```

---

## Architecture

### Service Composition

Logos Blockchain uses the [Overwatch][overwatch-github] framework to compose nodes from independent services at compile time. Each service has a front layer (Overwatch integration) and a back layer (business logic), making components easy to swap:

```rust
#[derive_services]
struct MockPoolNode {
    logging: Logger,
    network: NetworkService<Waku>,
    mockpool: MempoolService<WakuAdapter<Tx>, MockPool<TxId, Tx>>,
    http: HttpService<AxumBackend>,
    bridges: HttpBridgeService,
}
```

### Static Dispatching

The codebase favors generics and static dispatch over dynamic dispatch. This means you'll see generics throughout — the trade-off is compile-time type safety and highly modular, adaptable applications.

---

## Development

### Running Tests

```bash
# Unit tests
cargo test --workspace --exclude logos-blockchain-tests

# Integration tests
cargo build -p logos-blockchain-node --all-targets --features testing
cargo test -p logos-blockchain-tests
```

### Multi-Node Local Testnet

```bash
cd testnet
docker compose up
```

See [`testnet/README.md`](testnet/README.md) for details.

### Join Existing Devnet

Visit our [GitHub releases page][github-releases-page] to get instructions on how to join our existing devnet deployment!

You can visit the [Devnet dashboard][devnet-dashboard] to get more info about the current devnet deployment.

### L2 Demo

```bash
cd testnet/l2-sequencer-archival-demo
docker compose up
# Web UI → http://localhost:8200
```

### Generating Documentation

```bash
cargo doc --open
```

### Dependency Graph

```bash
cargo install cargo-depgraph
cargo depgraph --workspace-only --all-features > deps.dot

# Render with Graphviz
dot -Tsvg deps.dot -o deps.svg
```

Or paste the `.dot` file into [Graphviz Online][graphviz-online].

---

## Contributing

We welcome contributions! Please read our [Contributing Guidelines](CONTRIBUTING.md) to get started.

---

## License

Dual-licensed under your choice of:

- [MIT](LICENSE-MIT)
- [Apache 2.0](LICENSE-APACHE2.0)

---

## Community

- [Discord][logos-discord]
- [Twitter / X][logos-x]
- [logos.co][logos-website]

[overwatch-github]: https://github.com/logos-co/Overwatch
[graphviz-online]: https://dreampuf.github.io/GraphvizOnline/
[logos-website]: https://logos.co/
[github-releases-page]: https://github.com/logos-blockchain/logos-blockchain/releases
[logos-discord]: https://discord.gg/ezJefwJY
[logos-x]: https://x.com/Logos_network
[logos-website]: https://logos.co/
[devnet-dashboard]: https://devnet.blockchain.logos.co/web/