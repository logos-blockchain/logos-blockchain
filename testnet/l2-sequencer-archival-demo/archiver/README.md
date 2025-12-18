# Archiver Demo

A real-time block archiver that subscribes to a Logos Blockchain node's Last Immutable Block (LIB) stream, extracts L2 sequencer inscriptions from a specified channel, validates transactions, and exposes them via HTTP endpoints.

## What It Does

1. **Connects to a Logos Blockchain node** via HTTP to subscribe to the LIB stream
2. **Filters inscriptions** by channel ID to extract L2 sequencer block data
3. **Validates blocks** — a block is invalid if:
   - It references an invalid parent block, or
   - It contains a transaction where the sender has insufficient balance
4. **Persists valid blocks** and tracks invalid block IDs
5. **Broadcasts blocks** to connected clients via an SSE endpoint at `/block_stream`
6. **Serves historical blocks** via a REST endpoint at `/blocks`
7. **Pretty prints** transaction details to the console with colored output

## Building

```bash
cargo build --release -p logos-blockchain-archiver
```

## Running

### Command Line Arguments

| Flag | Env Variable | Description | Default |
|------|--------------|-------------|---------|
| `-e` | `TESTNET_ENDPOINT` | Logos Blockchain node HTTP endpoint URL | Required |
| `-u` | `TESTNET_USERNAME` | Basic auth username | Required |
| `-p` | `TESTNET_PASSWORD` | Basic auth password | Required |
| `-c` | `CHANNEL_ID` | Channel ID (64 hex chars / 32 bytes) | Required |
| `-t` | `TOKEN_NAME` | Token name to display in output | Required |
| `-b` | `INITIAL_BALANCE` | Initial balance for new accounts | `1000` |
| `-n` | `PORT_NUMBER` | HTTP server port | `8090` |

### Using CLI Flags

```bash
./target/release/logos-blockchain-archiver \
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
export TESTNET_USERNAME=admin
export TESTNET_PASSWORD=secret
export CHANNEL_ID=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
export TOKEN_NAME=DEMO
export INITIAL_BALANCE=1000
export PORT_NUMBER=8090

./target/release/logos-blockchain-archiver
```

### Using a `.env` File

Create a `.env` file:

```env
TESTNET_ENDPOINT=http://localhost:8080
TESTNET_USERNAME=admin
TESTNET_PASSWORD=secret
CHANNEL_ID=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
TOKEN_NAME=DEMO
INITIAL_BALANCE=1000
PORT_NUMBER=8090
```

Then run with a tool like `dotenv`:

```bash
dotenv ./target/release/logos-blockchain-archiver
```

## HTTP API

The archiver starts an HTTP server on the configured port (default `8090`).

### GET `/block_stream`

Server-Sent Events stream of validated L2 blocks in real-time.

**Example:**

```bash
curl -N http://localhost:8090/block_stream
```

**Response format:**

```
data: {"block_id":1,"transactions":[{"id":"...","from":"alice","to":"bob","amount":100}]}

data: {"block_id":2,"transactions":[{"id":"...","from":"bob","to":"charlie","amount":50}]}
```

Each `data:` line contains a JSON-serialized validated block object.

### GET `/blocks`

Returns all stored validated blocks as a JSON array.

**Example:**

```bash
curl http://localhost:8090/blocks
```

## Console Output

When running, the archiver displays:

- A startup banner with connection details
- Real-time block notifications with transaction details
- Colored output showing sender → receiver transfers

Example:

```
┌
│ 📦 Block #42
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