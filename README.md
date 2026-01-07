# Nomos

Nomos is the blockchain layer of the Logos technology stack, providing a privacy-preserving and censorship-resistant
framework for decentralized network states.

This monorepo serves as a unified codebase for the Nomos ecosystem, housing all core components, services, and tools
necessary for running and interacting with the Nomos blockchain. Key features include:

- Consensus mechanisms for secure and scalable network agreement
- Ledger management for state persistence and validation
- Networking layers leveraging libp2p for peer-to-peer communication
- CLI tools and clients for seamless interaction with the blockchain
- Testnet configurations for development and experimentation

## Table of Contents

- [Nomos](#nomos)
  - [Table of Contents](#table-of-contents)
  - [Requirements](#requirements)
  - [Setting Up Zero-Knowledge Circuits](#setting-up-zero-knowledge-circuits)
    - [Quick Setup (Recommended)](#quick-setup-recommended)
    - [Custom Installation](#custom-installation)
    - [What Gets Installed](#what-gets-installed)
    - [macOS Users](#macos-users)
    - [Verifying Installation](#verifying-installation)
  - [Design Goals](#design-goals)
    - [Service Architecture](#service-architecture)
    - [Static Dispatching](#static-dispatching)
  - [Project Structure](#project-structure)
  - [Development Workflow](#development-workflow)
    - [Docker](#docker)
      - [Building the Image](#building-the-image)
      - [Running a Nomos Node](#running-a-nomos-node)
  - [Running Tests](#running-tests)
  - [Generating Documentation](#generating-documentation)
  - [Contributing](#contributing)
  - [License](#license)
  - [Community](#community)

## Requirements

- **Rust**
    - We aim to maintain compatibility with the latest stable version of Rust.
    - [Installation Guide](https://www.rust-lang.org/tools/install)

## Setting Up Zero-Knowledge Circuits

Nomos uses zero-knowledge circuits for various cryptographic operations. To set up the required circuit binaries and keys:

### Quick Setup (Recommended)

Run the setup script to download and install the latest nomos-circuits release:

```bash
./scripts/setup-nomos-circuits.sh
```

This will install circuits to `~/.nomos-circuits/` by default.

### Custom Installation

You can specify a custom version or installation directory:

```bash
# Install a specific version
./scripts/setup-nomos-circuits.sh v0.3.0

# Install to a custom directory
./scripts/setup-nomos-circuits.sh v0.2.0 /opt/circuits
```

If you use a custom directory, you'll need to set the `NOMOS_CIRCUITS` environment variable:

```bash
export NOMOS_CIRCUITS=/opt/circuits
```

### macOS Users

Since we don't yet have code-signing implemented on macOS, the setup script automatically removes quarantine attributes from downloaded binaries. This allows the binaries to run without manual authorization through System Settings.

### Verifying Installation

After installation, verify the circuits are properly set up:

```bash
# Run tests that use the circuits
cargo test -p circuits-prover -p circuits-verifier --lib
```

## Design Goals

### Service Architecture

Nomos services follow a consistent design pattern: a front layer handles the `Overwatch` service, while a back layer
implements the actual service logic.

This modular approach allows for easy replacement of components in a declarative manner.

For example:

```rust ignore
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

Nomos favours static dispatching over dynamic, influenced by Overwatch.
This means you'll encounter Generics sprinkled throughout the codebase.
While it might occasionally feel a bit over the top, it brings some solid advantages, such as:

- Compile-time type checking
- Highly modular and adaptable applications

## Project Structure

```
nomos/
├── book/               # Documentation in Markdown format
├── ci/                 # Non-GitHub scripts, such as Jenkins' nightly integration and fuzzy testing
├── clients/            # General-purpose clients
├── consensus/          # Engine and protocols for agreement and validation
├── ledger/             # Ledger management and state transition logic
├── nodes/              # Node implementations
├── nomos-blend/        # Blend Network, our privacy routing protocol
├── nomos-bundler/      # Crate packaging and bundling
├── nomos-cli/          # Command-line interface for interacting with the Nomos blockchain
├── nomos-core/         # Collection of essential structures
├── nomos-da/           # Data availability layer
├── nomos-libp2p/       # Libp2p integration
├── nomos-services/     # Building blocks for the Node
├── nomos-tracing/      # Tracing, logging, and metrics
├── nomos-utils/        # Shared utility functions and helpers
├── scripts/            # Utility scripts including circuit setup
├── testnet/            # Testnet configurations, monitoring, and deployment scripts
├── tests/              # Integration and E2E test suites
└── zk/                 # Zero-knowledge proof infrastructure
    ├── circuits/       # ZK circuit utilities (prover, verifier, witness generators)
    └── proofs/         # Proof implementations (pol, poq, poc, zksign)
```

## Development Workflow

### Docker

#### Building the Image

To build the Nomos Docker image, run:

```bash
docker build -t nomos .
```

#### Setting `chain_start_time` timestamp

When running a node locally with a custom config or `config-one-node.yaml`, you may encounter the following error if the configuration's start time is too far in the past:
```
ERROR chain_leader: trying to propose a block for slot XXXX but epoch state is not available
```

To resolve this, you must manually update the chain_start_time in the config file to a recent timestamp (ideally within a few minutes of your current system time) before launching the node, **or use a command-line flag to start the node** with the chain start time set to the current time:

```bash
CONSENSUS_SLOT_TIME=5 POL_PROOF_DEV_MODE=true nomos-node nodes/nomos-node/config-one-node.yaml --dev-mode-reset-chain-clock
```

##### Manually set chain start time in config

You can generate a timestamp in the required format using the following command in your terminal:
Bash
```bash
# For macOS/Linux
date -u +"%Y-%m-%d %H:%M:%S.000000 +00:00:00"
```

Open nodes/nomos-node/config-one-node.yaml and locate the time section. Replace the chain_start_time value with the output from the command above:
YAML

```bash
time:
  backend:
    ntp_server: pool.ntp.org
    # ... other settings ...
  chain_start_time: 2026-01-07 10:45:00.000000 +00:00:00 # <--- Update this line
```

Once updated, restart the node.

#### Running Logos Blockchain Node with Docker

To run a docker container with the Nomos node you need to mount both `config.yml` and `global_params_path` specified in
the configuration.

```bash
docker run -v "/path/to/config.yml" -v "/path/to/global_params:global/params/path" nomos /etc/nomos/config.yml
```

To use an example configuration located at `nodes/nomos-node/config.yaml`, first run the test that generates the random
kzgrs file and then run the docker container with the appropriate config and global params:

```bash
cargo test --package kzgrs-backend write_random_kzgrs_params_to_file -- --ignored

docker run -v "$(pwd)/nodes/nomos-node/config.yaml:/etc/nomos/config.yml" -v "$(pwd)/nomos-da/kzgrs-backend/kzgrs_test_params:/app/tests/kzgrs/kzgrs_test_params" nomos /etc/nomos/config.yml

```

#### Running Logos Blockchain Node locally

When the node is built locally, it can be run with example config for one node network:
```bash
# Build logos blockchain binaries.
cargo build --all-features --all-targets

# Run node without connecting to any other node.
CONSENSUS_SLOT_TIME=5 POL_PROOF_DEV_MODE=true target/debug/nomos-node nodes/nomos-node/config-one-node.yaml
```

Node stores its state inside the `db` directory. If there are any issues when restarting the node, please try removing `db` directory.

#### Running Logos Blockchain Node with integration test

To run the node programatically, one can use `local_testnet_one_node` integration test.
```bash
# Build logos blockchain binaries.
cargo build --all-features --all-targets

# Integration test uses binaries built in a previous step.
CONSENSUS_SLOT_TIME=5 POL_PROOF_DEV_MODE=true cargo test --all-features local_testnet_one_node -- --ignored --nocapture
```

## Running Tests

To run the test suite, use:

```bash
cargo test
```

## Generating Documentation

To generate the project documentation locally, run:

```bash
cargo doc
```

## Dependency Graph Visualization

To visualize the project's dependency structure, you can generate a dependency graph using `cargo-depgraph`.

### Installation

First, install the `cargo-depgraph` tool:

```bash
cargo install cargo-depgraph
```

### Generating the Graph

Generate a DOT file containing the dependency graph:

```bash
# Full dependency graph with all transitive dependencies
cargo depgraph --all-deps --dedup-transitive-deps --workspace-only --all-features > dependencies_graph.dot

# Simplified graph showing only direct dependencies
cargo depgraph --workspace-only --all-features > dependencies_graph_simple.dot
```

### Rendering the Graph

Convert the DOT file to a viewable format using Graphviz:

```bash
# Install Graphviz (macOS)
brew install graphviz

# Install Graphviz (Ubuntu/Debian)
sudo apt-get install graphviz

# Render to PNG
dot -Tpng dependencies_graph.dot -o dependencies_graph.png

# Render to SVG (better for large graphs)
dot -Tsvg dependencies_graph.dot -o dependencies_graph.svg
```

### Alternative: Online Visualization

You can also visualize the DOT file online using tools like:
- [Graphviz Online](https://dreampuf.github.io/GraphvizOnline/)
- [WebGraphviz](http://www.webgraphviz.com/)

Simply copy the contents of the DOT file and paste it into the online tool.

## Contributing

We welcome contributions! Please read our [Contributing Guidelines](CONTRIBUTING.md) for details on how to get started.

## License

This project is primarily distributed under the terms defined by either the MIT license or the
Apache License (Version 2.0), at your option.

See [LICENSE-APACHE2.0](LICENSE-APACHE2.0) and [LICENSE-MIT](LICENSE-MIT) for details.

## Community

Join the Nomos community on [Discord](https://discord.gg/8Q7Q7vz) and follow us
on [Twitter](https://twitter.com/nomos_tech).

For more information, visit [nomos.tech](https://nomos.tech/?utm_source=chatgpt.com).
