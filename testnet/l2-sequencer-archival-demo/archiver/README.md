# Archiver Demo

A real-time block archiver that subscribes to a Logos Blockchain node's Last Immutable Block (LIB) stream, extracts L2 sequencer inscriptions from a specified channel, and exposes them via a Server-Sent Events (SSE) HTTP endpoint.

## What It Does

1. **Connects to a Nomos node** via HTTP to subscribe to the LIB stream
2. **Filters inscriptions** by channel ID to extract L2 sequencer block data
3. **Broadcasts blocks** to connected clients via an SSE endpoint at `/blocks`
4. **Pretty prints** transaction details to the console with colored output

## Building

```bash
cargo build --release -p logos-blockchain-archiver
```

## Running

### Command Line Arguments

| Flag | Env Variable | Description | Required |
|------|--------------|-------------|----------|
| `-e` | `ENDPOINT` | Nomos node HTTP endpoint URL | Yes |
| `-u` | `USERNAME` | Basic auth username | Yes |
| `-p` | `PASSWORD` | Basic auth password | Yes |
| `-c` | `CHANNEL_ID` | Channel ID (64 hex chars / 32 bytes) | Yes |
| `-t` | `TOKEN_NAME` | Token name to display in output | Yes |

### Using CLI Flags

```bash
./target/release/logos-blockchain-archiver \
  -e http://localhost:8080 \
  -u admin \
  -p secret \
  -c 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef \
  -t DEMO
```

### Using Environment Variables

```bash
export ENDPOINT=http://localhost:8080
export USERNAME=admin
export PASSWORD=secret
export CHANNEL_ID=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
export TOKEN_NAME=DEMO

./target/release/logos-blockchain-archiver
```

### Using a `.env` File

Create a `.env` file:

```env
ENDPOINT=http://localhost:8080
USERNAME=admin
PASSWORD=secret
CHANNEL_ID=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
TOKEN_NAME=DEMO
```

Then run with a tool like `dotenv`:

```bash
dotenv ./target/release/logos-blockchain-archiver
```

## HTTP API

### GET `/blocks`

Server-Sent Events stream of L2 blocks.

**Example:**

```bash
curl -N http://localhost:8080/blocks
```

**Response format:**

```
data: {"block_id":1,"transactions":[{"id":"...","from":"alice","to":"bob","amount":100}]}

data: {"block_id":2,"transactions":[{"id":"...","from":"bob","to":"charlie","amount":50}]}
```

Each `data:` line contains a JSON-serialized `BlockData` object.

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