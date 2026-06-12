# Archiver Demo

A real-time block archiver that subscribes to a Logos Blockchain node's Last Immutable Block (LIB) stream, extracts L2 sequencer inscriptions from a specified channel, validates transactions, and exposes them via HTTP endpoints.

## What It Does

1. **Connects to a Logos Blockchain node** via HTTP to subscribe to the LIB stream
2. **Filters inscriptions** by channel ID to extract L2 sequencer block data
3. **Validates blocks** — a block is invalid if:
   - Its parent block (except genesis block 0) was previously marked as invalid, or
   - It contains a transaction where the sender has insufficient balance
4. **Persists valid blocks** in a redb database and tracks invalid block IDs
5. **Re-validates blocks** — previously invalid blocks are automatically marked as valid when they appear again with valid ancestry
6. **Broadcasts blocks** to connected clients via an SSE endpoint at `/block_stream`
7. **Serves historical blocks** via a REST endpoint at `/blocks`
8. **Pretty prints** transaction details to the console with colored output

## Building

```bash
cargo build --release -p logos-blockchain-demo-archiver
```

## Running

### Command Line Arguments

| Flag | Env Variable | Description | Default |
|------|--------------|-------------|---------|
| `-e` | `TESTNET_ENDPOINT` | Logos Blockchain node HTTP endpoint URL | Required |
| `-u` | `TESTNET_USERNAME` | Basic auth username | Optional |
| `-p` | `TESTNET_PASSWORD` | Basic auth password | Optional |
| `-c` | `CHANNEL_ID` | Channel ID (64 hex chars / 32 bytes) | Required |
| `-t` | `TOKEN_NAME` | Token name to display in output | Required |
| `-b` | `INITIAL_BALANCE` | Initial balance for new accounts | `1000` |
| `-n` | `PORT_NUMBER` | HTTP server port | `8090` |

### Using CLI Flags

```bash
./target/release/logos-blockchain-demo-archiver \
  -e http://localhost:8080 \
  -c 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef \
  -t DEMO
```

With optional authentication:

```bash
./target/release/logos-blockchain-demo-archiver \
  -e http://localhost:8080 \
  -u admin \
  -p secret \
  -c 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef \
  -t DEMO \
  -b 1000 \
  -n 8090
```

### Using Environment Variables

```bash
export TESTNET_ENDPOINT=http://localhost:8080
export CHANNEL_ID=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
export TOKEN_NAME=DEMO

# Optional
export TESTNET_USERNAME=admin
export TESTNET_PASSWORD=secret
export INITIAL_BALANCE=1000
export PORT_NUMBER=8090

./target/release/logos-blockchain-demo-archiver
```

### Using a `.env` File

Create a `.env` file:

```env
TESTNET_ENDPOINT=http://localhost:8080
CHANNEL_ID=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
TOKEN_NAME=DEMO

# Optional
TESTNET_USERNAME=admin
TESTNET_PASSWORD=secret
INITIAL_BALANCE=1000
PORT_NUMBER=8090
```

Then run with a tool like `dotenv`:

```bash
dotenv ./target/release/logos-blockchain-demo-archiver
```

## HTTP API

The archiver starts an HTTP server on the configured port (default `8090`). CORS is enabled for all origins.

### GET `/block_stream`

Server-Sent Events stream of validated L2 blocks in real-time.

**Example:**

```bash
curl -N http://localhost:8090/block_stream
```

**Response format:**

```
data: {"data":{"block_id":1,"parent_block_id":0,"transactions":[{"id":"...","from":"alice","to":"bob","amount":100,"confirmed":false,"index":0}]},"l1_block_id":"..."}

data: {"data":{"block_id":2,"parent_block_id":1,"transactions":[{"id":"...","from":"bob","to":"charlie","amount":50,"confirmed":false,"index":0}]},"l1_block_id":"..."}
```

Each `data:` line contains a JSON-serialized validated block object with the L1 block ID where it was inscribed.

### GET `/blocks`

Returns all stored validated blocks as a JSON array.

**Example:**

```bash
curl http://localhost:8090/blocks
```

**Response format:**

```json
[
  {
    "data": {
      "block_id": 1,
      "parent_block_id": 0,
      "transactions": [
        {
          "id": "tx-uuid",
          "from": "alice",
          "to": "bob",
          "amount": 100,
          "confirmed": false,
          "index": 0
        }
      ]
    },
    "l1_block_id": "0123456789abcdef..."
  }
]
```

## Data Storage

The archiver uses [redb](https://github.com/cberner/redb) for persistent storage:

- **`blocks.database`** — Stores validated L2 blocks
- **`accounts.database`** — Tracks account balances for transaction validation

## Console Output

When running, the archiver displays:

- A startup banner with connection details
- Real-time L1 block notifications with height and header ID
- L2 block details with transaction information
- Colored output showing sender → receiver transfers

Example:

```
    _             _     _                 ____
   / \   _ __ ___| |__ (_)_   _____ _ __ |  _ \  ___ _ __ ___   ___
  / _ \ | '__/ __| '_ \| \ \ / / _ \ '__|| | | |/ _ \ '_ ` _ \ / _ \
 / ___ \| | | (__| | | | |\ V /  __/ |   | |_| |  __/ | | | | | (_) |
/_/   \_\_|  \___|_| |_|_| \_/ \___|_|   |____/ \___|_| |_| |_|\___/

══════════════════════════════════════════════════════════════════════
  📡 Logos Blockchain Node: http://localhost:8080
  📺 Channel ID: 0123456789abcdef...
  🌐 HTTP Server: http://0.0.0.0:8090/blocks
══════════════════════════════════════════════════════════════════════
  ⏳ Waiting for blocks...

  🔗 Block at height 42 (abc123...)
┌
│ 📦 Block #1
│ 💳 2 transaction(s)
│   ↳ alice → bob (100 DEMO)
│   ↳ bob → charlie (50 DEMO)
└
```

## Graceful Shutdown

Press `Ctrl+C` to initiate a graceful shutdown. The archiver will:

1. Stop accepting new SSE connections
2. Complete any in-flight block processing
3. Close all connections cleanly