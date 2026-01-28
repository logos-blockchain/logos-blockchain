# Tests

## Tests Debugging Setup

This document provides instructions for setting up and using the testing environment, including how to start the Docker 
setup, run tests with a feature flag, and access the Grafana dashboard.

## Prerequisites

### Using Docker

Ensure that the following are installed on your system:
- [Docker](https://docs.docker.com/get-docker/)
- [Docker Compose](https://docs.docker.com/compose/install/)

### Using Rust `cargo test`

Integration tests involving nodes run the binaries directly by spawning. Ensure the binaries are built and available in 
your `target/debug` or `target/release` directory. You can build the project using:

`cargo build --features pol-dev-mode` or `cargo build --release --features pol-dev-mode`

**Notes:**
- The `pol-dev-mode` feature flag enables proofs to be generated in dev mode.

## Setup and Usage (using Docker)

### 1. Start `compose.debug.yml`

To start the services defined in `compose.debug.yml` using Docker Compose, run the following command:

```bash
docker-compose -f compose.debug.yml up -d
```

This command will:
    Use the configuration specified in compose.debug.yml.
    Start all services in detached mode (-d), allowing the terminal to be used for other commands.

To stop the services, you can run:
```
docker compose -f compose.debug.yml down   # compose filename needs to be the same
```

### 2. Access the Grafana Dashboard
> It's important that the test is performed after the docker compose is started

Once the Docker setup is running, you can access the Grafana dashboard to view metrics and logs:
    Open a browser and navigate to http://localhost:9091.

Use "Explore" tab to select data source: "Loki", "Tempo", "Prometheus". Prometheus source is unusable at the moment in 
local setup.

- Loki - to kickstart your query, select "host" as label filter, and "nomo-0" or other nodes as value, this will show 
- all logs for selected host.
- Tempo - to kickstart your query, enter "{}" as TraceQL query to see all traces.


## Setup and Usage (using `cargo test`)

Where tests involve spawning node binaries, preference will be given to binaries corresponding to `USE_DEBUG_BINARIES` 
and `USE_RELEASE_BINARIES` environment variables in the in `target/debug` and in `target/debug` paths respectively. If 
neither are defined, preference will be given to debug binaries.

### 1. Run a specific test

_**MacOS or Linux**_

```bash
POL_PROOF_DEV_MODE=1 cargo test --test test_cryptarchia_happy_path two_nodes_happy -- --no-capture
```
or 
```bash
POL_PROOF_DEV_MODE=1 USE_RELEASE_BINARIES=1 cargo test --test test_cryptarchia_happy_path two_nodes_happy --release -- --no-capture
```

_**Windows (PowerShell)**_

```pwsh
$env:POL_PROOF_DEV_MODE="1"; cargo test --test test_cryptarchia_happy_path two_nodes_happy -- --no-capture
```
or
```pwsh
$env:POL_PROOF_DEV_MODE="1"; $env:USE_RELEASE_BINARIES="1"; cargo test --test test_cryptarchia_happy_path two_nodes_happy --release -- --no-capture

```

**Notes:**
- The presence of the `POL_PROOF_DEV_MODE` environment variable enables proofs to be generated in dev mode.


### 2. Run Tests with Debug Feature Flag

To execute the test suite with the debug feature flag, use the following command:

_**MacOS or Linux**_

```bash
POL_PROOF_DEV_MODE=1 cargo test -p logos-blockchain-tests -F debug disseminate_and_retrieve
```

_**Windows (PowerShell)**_

```pwsh
$env:POL_PROOF_DEV_MODE="1"; cargo test -p logos-blockchain-tests -F debug disseminate_and_retrieve
```

`-F debug`: Enables the debug feature flag for the integration tests, allowing for extra debug output or specific
debug-only code paths to be enabled during the tests.
To modify the tracing configuration when using `-F debug` flag go to `tests/src/topology/configs/tracing.rs`. If debug
flag is not used, logs will be written into each nodes temporary directory.

## Running Cucumber tests

To run the Cucumber tests, ensure the binaries are built (debug or release) and the environment variables below point to 
the corresponding binaries:

```text
POL_PROOF_DEV_MODE=1
LOGOS_BLOCKCHAIN_KZGRS_PARAMS_PATH=/path-to/tests/logos-blockchain-kzgrs
LOGOS_BLOCKCHAIN__NODE_BIN=/path-to/target/release/logos-blockchain-node
```

Filtering based on tags can be done using the `--tags` option. For example, to run all tests tagged with `@normal_ci`, 
use the following command:
```bash
cargo test --release --features cucumber --test cucumber -- --tags "@normal_ci"
```

Filtering based on test names can be done using the `--name` option. For example, to run a specific test named
"Idle smoke", use the following command:

```bash
cargo test --release --features cucumber --test cucumber -- --name "Idle smoke"
```

For more information on running Cucumber tests, refer to https://github.com/cucumber-rs/cucumber or 
https://cucumber.io/docs.
