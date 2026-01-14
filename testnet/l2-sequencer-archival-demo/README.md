# Logos L2 Sequencer & Archival Demo

This directory contains a reference implementation of a l2 solution using the Logos Blockchain as a Settlement layer. It consists of three primary components working in tandem to provide fast L2 transactions with L1 security.

## System Architecture

The demo follows a classic rollup-style architecture where the Sequencer handles execution, and the Archiver handles state derivation from L1 data.

1. **L2 Sequencer**: The entry point for users. It accepts transactions, maintains a local mempool, batches them into L2 blocks, and "inscribes" them to a specific **Channel ID** on the Logos L1.
2. **Logos L1**: Acts as the immutable ledger. It doesn't know the "rules" of the L2; it simply stores the L2 data in a verifiable sequence.
3. **Archiver**: Watches the Logos L1 stream. It pulls L2 data from the designated channel, validates the transactions against L2 state rules (e.g., balance checks), and provides a verified API for the frontend.
4. **Frontend**: A simple dashboard to visualize transfers, account balances, and real-time block production.

---

## Project Structure

Each component is a standalone service that can be run independently or via Docker.

| Component | Directory | Responsibility |
| --- | --- | --- |
| **Sequencer** | `sequencer/` | Transaction ingestion, batching, and L1 inscription. |
| **Archiver** | `archiver/` | L1 monitoring, L2 block validation, and data serving. |
| **Frontend** | `webapp/` | UI for monitoring L2 state and sending transactions. |

---

## Getting Started (Local Run)

### Prerequisites

* **Docker & Docker Compose**
* **Logos Testnet Credentials**: If connecting to the public testnet, you need basic auth credentials (Username/Password). Contact the team via Discord to obtain these.

### 1. Configuration

Copy the example environment file and fill in your credentials.

```bash
cp testnet/l2-sequencer-archival-demo/.env.example testnet/l2-sequencer-archival-demo/.env

```

### 2. Launch with Docker Compose

The simplest way to run the entire stack is using our prebuilt images.

```bash
# Navigate to the demo directory
cd testnet/l2-sequencer-archival-demo

# Start all services
docker compose up
```

Once running, the web application will be available at `http://localhost:8200`.

---

## Manual Development Setup

For developers on macOS (including ARM/M1/M2) or Linux who wish to run components outside of Docker, we provide a unified helper script: `run-local.sh`.

### Prerequisites

* **Rust**: For building the Sequencer and Archiver binaries.
* **Bun**: For running the frontend development server.
* **OpenSSL**: For generating unique Channel IDs.

### Using the Local Runner

The script automates building binaries, managing data directories, and linking environment variables between services.

```bash
# Usage
./run-local.sh <service> --env-file <path-to-env> [--clean]

# 1. Run the entire stack (Sequencer + Archiver + Frontend)
./run-local.sh all --env-file .env-local

# 2. Run only a specific component
./run-local.sh sequencer --env-file .env-local

# 3. Start fresh (deletes local databases/keys)
./run-local.sh all --env-file .env-local --clean

```

### Environment Setup

You will need a running **Nomos Node** (L1). If you are running one locally, ensure the `TESTNET_ENDPOINT` in your `.env` points to your local node.

---

## Component READMEs

For detailed configuration flags, API documentation, and internal logic for each component, please refer to their individual documentation:

* **[Sequencer In-Depth](./sequencer/README.md)** - Transaction batching and L1 submission logic.
* **[Archiver In-Depth](./archiver/README.md)** - Validation engine and the SSE block stream.
* **[Webapp Setup](./webapp/README.md)** - UI development and customization instructions.

