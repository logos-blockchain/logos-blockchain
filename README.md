# Logos Blockchain

Logos blockchain is a component of the Logos technology stack, providing a privacy-preserving and censorship-resistant framework for decentralized network states.

This monorepo serves as a unified codebase for the Logos blockchain ecosystem, housing all core components, services, and tools
necessary for running and interacting with the Logos blockchain. Key features include:

- Consensus mechanisms for secure and scalable network agreement
- Ledger management for state persistence and validation
- Networking layers leveraging libp2p for peer-to-peer communication
- CLI tools and clients for seamless interaction with the blockchain
- Testnet configurations for development and experimentation

## Table of Contents

## Table of Contents

- [Logos blockchain](#logos-blockchain)
  - [Table of Contents](#table-of-contents)
  - [Requirements](#requirements)
  - [Setting Up Zero-Knowledge Circuits](#setting-up-zero-knowledge-circuits)
    - [Quick Setup (Recommended)](#quick-setup-recommended)
      - [Linux](#linux)
      - [Windows](#windows)
    - [Custom Installation](#custom-installation)
      - [Linux](#linux-1)
      - [Windows](#windows-1)
    - [macOS Users](#macos-users)
    - [Verifying Installation](#verifying-installation)
  - [Design Goals](#design-goals)
    - [Service Architecture](#service-architecture)
    - [Static Dispatching](#static-dispatching)
  - [Project Structure](#project-structure)
  - [Development Workflow](#development-workflow)
    - [Feature exclusions](#feature-exclusions)
    - [Building the Image](#building-the-image)
      - [Docker](#docker)
      - [Command line](#command-line)
    - [Setting `chain_start_time` timestamp](#setting-chain_start_time-timestamp)
      - [Manually set chain start time in config](#manually-set-chain-start-time-in-config)
    - [Running a Logos blockchain Node](#running-a-logos-blockchain-node)
      - [Docker](#docker-1)
      - [Running Logos Blockchain Node locally](#running-logos-blockchain-node-locally)
      - [Running Logos Blockchain Node with integration test](#running-logos-blockchain-node-with-integration-test)
  - [Running Tests](#running-tests)
  - [Generating Documentation](#generating-documentation)
  - [Dependency Graph Visualization](#dependency-graph-visualization)
    - [Installation](#installation)
    - [Generating the Graph](#generating-the-graph)
    - [Rendering the Graph](#rendering-the-graph)
    - [Alternative: Online Visualization](#alternative-online-visualization)
  - [Contributing](#contributing)
  - [License](#license)
  - [Community](#community)

## Requirements

- **Rust**
    - We aim to maintain compatibility with the latest stable version of Rust.
    - [Installation Guide](https://www.rust-lang.org/tools/install)

## Setting Up Zero-Knowledge Circuits

Logos blockchain uses zero-knowledge circuits for various cryptographic operations. To set up the required circuit binaries and keys:

### Quick Setup (Recommended)

Run the setup script to download and install the latest logos-blockchain-circuits release, which will install circuits to 
`~/.logos-blockchain-circuits/` (`Linux`) or `$env:USERPROFILE\.logos-blockchain-circuits` (`Windows`) by default.:

#### Linux

```bash
./scripts/setup-logos-blockchain-circuits.sh
```

#### Windows

```powershell
.\scripts\setup-logos-blockchain-circuits.ps1
```

Also make sure that Visual Studio build tools with LLVM (or other LLVM with clang) are installed with the 
`LIBCLANG_PATH` environment variable specified and pointing to the 64-bit `libclang.dll` folder, for example
`setx LIBCLANG_PATH "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\Llvm\x64\bin"`

### Custom Installation

#### Linux

You can specify a custom version or installation directory:

```bash
# Install a specific version
./scripts/setup-logos-blockchain-circuits.sh v0.3.0

# Install to a custom directory
./scripts/setup-logos-blockchain-circuits.sh v0.2.0 /opt/circuits
```

If you use a custom directory, you'll need to set the `LOGOS_BLOCKCHAIN_CIRCUITS` environment variable:

```bash
export LOGOS_BLOCKCHAIN_CIRCUITS=/opt/circuits
```

#### Windows

```powershell
# Install a specific version
.\scripts\setup-logos-blockchain-circuits.ps1 v0.3.0

# Install to a custom directory
.\scripts\setup-logos-blockchain-circuits.ps1 v0.2.0 $env:USERPROFILE\circuits
```

If you use a custom directory, you'll need to set the `LOGOS_BLOCKCHAIN_CIRCUITS` environment variable:

```powershell
$env:LOGOS_BLOCKCHAIN_CIRCUITS="$env:USERPROFILE\circuits"
```

### macOS Users

Since we don't yet have code-signing implemented on macOS, the setup script automatically removes quarantine attributes 
from downloaded binaries. This allows the binaries to run without manual authorization through System Settings.

### Verifying Installation

After installation, verify the circuits are properly set up:

```bash
# Run tests that use the circuits
cargo test -p logos-blockchain-circuits-prover -p logos-blockchain-circuits-verifier --lib
```

## Design Goals

### Service Architecture

Logos blockchain services follow a consistent design pattern: a front layer handles the `Overwatch` service, while a back layer
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

Logos blockchain favours static dispatching over dynamic, influenced by Overwatch.
This means you'll encounter Generics sprinkled throughout the codebase.
While it might occasionally feel a bit over the top, it brings some solid advantages, such as:

- Compile-time type checking
- Highly modular and adaptable applications

## Development Workflow

### Feature exclusions

Currently the `"profiling"` feature is not supported in Windows builds.

### Building the Image

#### Docker

To build the Logos blockchain Docker image, run:

```bash
docker build -t logos-blockchain-node .
```

#### Command line

To build the Logos blockchain command line executable, run:

```bash
cargo build --release
```

### Setting `chain_start_time` timestamp

When running a node locally with a custom config or `config-one-node.yaml`, you may encounter the following error if the 
configuration's start time is too far in the past:
```
ERROR chain_leader: trying to propose a block for slot XXXX but epoch state is not available
```

To resolve this, you must manually update the chain_start_time in the config file to a recent timestamp (ideally within 
a few minutes of your current system time) before launching the node, **or use a command-line flag to start the node** 
with the chain start time set to the current time:

```bash
logos-blockchain-node nodes/node/config-one-node.yaml --dev-mode-reset-chain-clock
```

#### Manually set chain start time in config

You can generate a timestamp in the required format using the following command in your terminal:
Bash
```bash
# For macOS/Linux
date -u +"%Y-%m-%d %H:%M:%S.000000 +00:00:00"
```

Open nodes/node/config-one-node.yaml and locate the time section. Replace the `chain_start_time` value with the 
output from the command above:
YAML

```bash
time:
  backend:
    ntp_server: pool.ntp.org:123
    # ... other settings ...
  chain_start_time: 2026-01-07 10:45:00.000000 +00:00:00 # <--- Update this line
```

Once updated, restart the node.

### Running a Logos Blockchain Node

#### Docker

To run a docker container with the Logos blockchain node you need to mount both `config.yml` and `global_params_path` specified in
the configuration.

```bash
docker run -v "/path/to/config.yml:/config.yml" logos-blockchain-node /config.yml
```

#### Running Logos Blockchain Node locally

When the node is built locally, it can be run with example config for one node network:
```bash
# Build logos blockchain binaries.
cargo build --all-features --all-targets

# Run node without connecting to any other node.
target/debug/logos-blockchain-node nodes/node/config-one-node.yaml
```

Node stores its state inside the `db` directory. If there are any issues when restarting the node, please try removing 
`db` directory.

**Notes**

- To use an example configuration located at `nodes/node/config.yaml`, first run the test that generates the 
random kzgrs file (`kzgrs_test_params`), leave it in `./tests/kzgrs/kzgrs_test_params` or place it in a convenient 
location:

```bash
cargo test --package logos-blockchain-kzgrs-backend write_random_kzgrs_params_to_file -- --ignored
```

- To run the Logos blockchain node directly from the command line, edit the `global_params_path:` key in `/path/to/config.yaml` to 
point to the kzgrs file (`kzgrs_test_params`) and run with:

```bash
cargo run --package logos-blockchain-node -- /path/to/config.yaml
```

or copy the executable and run the binary directly:

```bash
./logos-blockchain-node /path/to/config.yaml
```


#### Running Logos Blockchain Node with integration test

To run the node programatically, one can use `local_testnet_one_node` integration test.
```bash
# Build logos blockchain binaries.
cargo build --all-features --all-targets

# Integration test uses binaries built in a previous step.
cargo test --all-features local_testnet_one_node -- --ignored --nocapture
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

Join the Logos community on [Discord](https://discord.gg/dUnm7CcB) and follow us
on [Twitter](https://twitter.com/nomos_tech).

For more information, visit [logos.co](https://logos.co/?utm_source=chatgpt.com).
