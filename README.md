<div align="center">

# Logos Blockchain

**A privacy-preserving, censorship-resistant blockchain for decentralized network states.**

[![MIT License](https://img.shields.io/badge/License-MIT-blue?style=for-the-badge)](https://github.com/logos-co/logos-blockchain/blob/master/LICENSE-MIT)
[![Apache License](https://img.shields.io/badge/License-Apache%202.0-blue?style=for-the-badge)](https://github.com/logos-co/logos-blockchain/blob/master/LICENSE-APACHE2.0)
[![Discord](https://img.shields.io/discord/1085215532189261874?style=for-the-badge&logo=discord&label=Discord)][logos-discord]

</div>

---

## What is Logos Blockchain?

Logos Blockchain is a core component of the [Logos][logos-website] technology stack.
It combines zero-knowledge proofs, a mix network for anonymity, and a modular service architecture to provide a foundation for sovereign digital communities.

This node represents the reference implementation of the Logos Blockchain specifications defined in the [Logos specifications space][specs].

## Quick Start

### Prerequisites

| Requirement      | Details                                 |
|------------------|-----------------------------------------|
| **LLVM / Clang** | Required for RocksDB and C bindings     |
| **ZK Circuits**  | Downloaded via setup script (see below) |

### 1. Clone the repository

```bash
git clone https://github.com/logos-blockchain/logos-blockchain.git
cd logos-blockchain
```

If you want to verify the circuits were installed successfully:

```bash
cargo test -p logos-blockchain-circuits-prover -p logos-blockchain-circuits-verifier
```

### 2. Build

**Note:** MacOS users may encounter linker warning messages due to a mismatch in C++ binaries (e.g. circuit or 
rapidsnark) target version (14.0 / 15.0) and what the Rust compiler on MacOS guarantees (11.0). These warnings may be 
safely ignored as the binaries should still run correctly, but the better fix would be to compile the node for the 
same minimum target version as the circuits (14.0). To do this, set the `MACOSX_DEPLOYMENT_TARGET` environment variable 
to `15.0` before building:

```bash
export MACOSX_DEPLOYMENT_TARGET=15.0
```

```bash
cargo build -p logos-blockchain-node --release
```

If you want to use [Jemalloc](https://crates.io/crates/tikv-jemallocator) as the global allocator, enable the `jemalloc` feature:
```bash
cargo build -p logos-blockchain-node --release --features jemalloc
```

### 3. Run a standalone node

To start a local standalone instance of a Logos Blockchain network, run:

```bash
target/release/logos-blockchain-node --deployment standalone-deployment-config.yaml nodes/node/standalone-node-config.yaml
```

The node stores state in the `state` directory. If you encounter issues on restart, try removing it before starting the node again.

### Docker

```bash
# Build
docker build -t logos-blockchain-node .

# Run (mount your config)
docker run -v "/path/to/node_config.yml:/node_config.yml" -v "/path/to/deployment_config.yml:/deployment_config.yml" logos-blockchain-node --deployment /deployment_config.yml /node_config.yml
```

---

## Architecture

Nodes are composed declaratively using the [Overwatch][overwatch-github] framework.
Each service has a front layer (Overwatch integration) and a back layer (business logic), making components easy to swap:

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
├── deployment/              Docker Compose testnets, faucet, L2 demo
└── tests/                Integration & Cucumber BDD tests
```

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
cd deployment
docker compose up
```

See [`deployment/README.md`](deployment/README.md) for details.

### Join Existing Devnet

Visit our [GitHub releases page][github-releases-page] to get instructions on how to join our existing devnet deployment!

You can visit the [Devnet dashboard][devnet-dashboard] to get more info about the current devnet deployment.

### L2 Demo

```bash
cd deployment/l2-sequencer-archival-demo
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

### Heap profiling

Heap profiling can be run on release builds by using the `release-profiling` Cargo profile:

```bash
    cargo build --profile release-profiling --features=dhat-heap
```
If the `dhat-heap` feature is enabled, it replaces the memory allocator with `dhat` even if `jemalloc` is enabled by default.

Run, then stop the node normally to capture the output, then read the generated `dhat-heap.json` file with 
https://nnethercote.github.io/dh_view/dh_view.html or other.

### Tokio task profiling

#### Build and node user config

Tokio task/resource profiling is available through `tokio-console`, which  is disabled in normal builds. 

Instrumented builds require both:
- the Cargo feature `tokio-console`
- `RUSTFLAGS="--cfg tokio_unstable"`

Manual build:

```bash
RUSTFLAGS="--cfg tokio_unstable" \
cargo build \
  --profile release-profiling \
  -p logos-blockchain-node \
  --features tokio-console
```

Enable the console endpoint at runtime by selecting the console tracing layer in your node config:

```yaml
tracing:
  console: !Console
    bind_address: 127.0.0.1
    port: 6669
    recording_path: /absolute/path/to/node-1-tokio-console.jsonl
```

`recording_path` enables subscriber-side raw Tokio Console recording. Omitting it
leaves raw recording disabled while the live Tokio Console endpoint remains
available. Absolute paths are recommended; the node process must be able to
create and write the file. Use a unique path for each node and profiling run.
Recordings can grow substantially during long runs, and recording adds
instrumentation and disk-I/O overhead. Stop the node gracefully so the recorder
can flush as much telemetry as possible. The file contains raw subscriber
telemetry for offline analysis and is different from Tokio Console client
diagnostic logs.

The verified `console-subscriber 0.5.0` format is newline-delimited JSON: the
first line is a version header (`{"v":1}`), followed by raw `Spawn`, `Enter`,
`Exit`, `Close`, and `Waker` event records. This recording format is currently
experimental and may change between subscriber versions. The Tokio Console
client connects to a live endpoint; it does not currently replay these raw
files directly, so offline analysis requires a compatible parser or tooling.

For multiple nodes, use unique ports and recording paths:

```yaml
# NODE_1
tracing:
  console: !Console
    bind_address: 127.0.0.1
    port: 6669
    recording_path: /profiles/run-001/node-1.jsonl
```

```yaml
# NODE_2
tracing:
  console: !Console
    bind_address: 127.0.0.1
    port: 6670
    recording_path: /profiles/run-001/node-2.jsonl
```

When using Cucumber, select recording independently for each profiled node:

```gherkin
And I will have tokio console profile nodes:
  | node_name | record_raw |
  | NODE_1    | true       |
  | NODE_2    | false      |
```

`true` enables the live endpoint and raw recording; `false` enables only the
live endpoint. Nodes omitted from the table are unaffected. For an enabled
node, Cucumber stores the recording at:

```text
<scenario-runtime-directory>/<node-runtime-directory>/tokio-console-raw.jsonl
```

The path is resolved after the node runtime directory is created and remains
with the scenario and node artifacts after shutdown.

**Note:** Port `6669` is the default port for the console, but you can change it in your config and use the 
corresponding value in the runtime if needed, for example, use a different port when multiple instrumented node 
processes are running on the same host.

Keep the console bound to loopback. When profiling a remote node, forward the port over SSH:

```bash
ssh -L 6669:127.0.0.1:6669 user@remote-host
```

#### Tokio console client

The `tokio-console` client version must be compatible with the `console-subscriber` version compiled into the node,
see https://github.com/tokio-rs/console/releases.

This repository currently uses `console-subscriber 0.5.x`, which is compatible with `tokio-console 0.1.14`. Keep the 
client version in sync with the node version to avoid connection issues.

Install the client (latest version):

```bash
cargo install --locked tokio-console
```

or  

Install a specific client version, e.g version 0.X.Y:

```bash 
cargo install --locked tokio-console --version 0.X.Y  
```

Run the node with that config, then connect from another terminal:

```bash
tokio-console http://127.0.0.1:6669
```

#### Manual connection checks

While the node is running, verify that the console endpoint is listening:

```bash
ss -ltnp | grep -E ':(6669)\b'
```

or:

```bash
nc -vz 127.0.0.1 6669
```

A successful TCP connection proves the node-side console server is listening. If the client UI is empty, check 
the `tokio-console` client version first.

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

[specs]: https://lip.logos.co/blockchain/index.html
[overwatch-github]: https://github.com/logos-co/Overwatch
[graphviz-online]: https://dreampuf.github.io/GraphvizOnline/
[github-releases-page]: https://github.com/logos-blockchain/logos-blockchain/releases
[logos-discord]: https://discord.gg/RxXjcHZE
[logos-x]: https://x.com/Logos_network
[logos-website]: https://logos.co/
[devnet-dashboard]: https://devnet.blockchain.logos.co/web/
