# MemChain Sequencer Demo

A demo L2 sequencer that processes token transfers and inscribes block data onto the Logos Blockchain.
It maintains account balances, batches transactions into blocks, and submits them as channel inscriptions.

## What It Does

1. **Accepts transfer requests** via a REST API
2. **Maintains account balances** in a persistent database (redb)
3. **Batches transactions** into blocks periodically
4. **Inscribes blocks** onto the Logos Blockchain via channel inscriptions
5. **Tracks confirmation status** by monitoring inscribed blocks on-chain

## Building

```bash
cargo build --release -p demo-sequencer
```

## Running

### Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `SEQUENCER_LISTEN_ADDR` | HTTP server listen address | `0.0.0.0:8080` |
| `SEQUENCER_NODE_ENDPOINT` | Nomos node HTTP endpoint | `http://localhost:18080` |
| `SEQUENCER_DB_PATH` | Path to redb database file | `sequencer.redb` |
| `SEQUENCER_SIGNING_KEY_PATH` | Path to signing key file (created if missing) | `sequencer.key` |
| `SEQUENCER_CHANNEL_ID` | Channel ID for inscriptions (64 hex chars) | **Required** |
| `SEQUENCER_INITIAL_BALANCE` | Initial token balance for new accounts | `1000` |
| `SEQUENCER_NODE_AUTH_USERNAME` | Basic auth username for Nomos node (optional) | - |
| `SEQUENCER_NODE_AUTH_PASSWORD` | Basic auth password for Nomos node (optional) | - |

### Example

```bash
export SEQUENCER_LISTEN_ADDR=0.0.0.0:3000
export SEQUENCER_NODE_ENDPOINT=http://localhost:18080
export SEQUENCER_DB_PATH=./data/sequencer.redb
export SEQUENCER_SIGNING_KEY_PATH=./data/sequencer.key
export SEQUENCER_INITIAL_BALANCE=1000
export SEQUENCER_NODE_AUTH_USERNAME=admin
export SEQUENCER_NODE_AUTH_PASSWORD=secret

./target/release/demo-sequencer
```

### Using a `.env` File

Create a `.env` file:

```env
SEQUENCER_LISTEN_ADDR=0.0.0.0:3000
SEQUENCER_NODE_ENDPOINT=http://localhost:18080
SEQUENCER_DB_PATH=./data/sequencer.redb
SEQUENCER_SIGNING_KEY_PATH=./data/sequencer.key
SEQUENCER_INITIAL_BALANCE=1000
SEQUENCER_NODE_AUTH_USERNAME=admin
SEQUENCER_NODE_AUTH_PASSWORD=secret
```

Then run with a tool like `dotenv`:

```bash
dotenv ./target/release/demo-sequencer
```

## HTTP API

### POST `/transfer`

Submit a token transfer between accounts.

**Request:**

```bash
curl -X POST http://localhost:8080/transfer \
  -H "Content-Type: application/json" \
  -d '{"from": "alice", "to": "bob", "amount": 100}'
```

**Response:**

```json
{
  "from_balance": 900,
  "to_balance": 1100,
  "tx_hash": "a1b2c3d4..."
}
```

### GET `/accounts/:account`

Get account balance and optionally transaction history.

**Request:**

```bash
# Balance only
curl http://localhost:8080/accounts/alice

# With transaction history
curl http://localhost:8080/accounts/alice?tx=true
```

**Response:**

```json
{
  "account": "alice",
  "balance": 900,
  "confirmed_balance": 900,
  "transactions": [
    {
      "id": "abc123...",
      "from": "alice",
      "to": "bob",
      "amount": 100
    }
  ]
}
```

### GET `/accounts`

List all accounts and their balances.

**Request:**

```bash
curl http://localhost:8080/accounts
```

**Response:**

```json
{
  "accounts": [
    { "account": "alice", "balance": 900 },
    { "account": "bob", "balance": 1100 }
  ]
}
```

### GET `/health`

Health check endpoint.

**Request:**

```bash
curl http://localhost:8080/health
```

**Response:**

```
OK
```

## Logging

The sequencer uses `tracing` for structured logging. Control log level via the `RUST_LOG` environment variable:

```bash
# Debug logging
RUST_LOG=debug ./target/release/demo-sequencer

# Only show warnings and errors
RUST_LOG=warn ./target/release/demo-sequencer
```

## Data Persistence

- **Database:** Account balances and transaction history are stored in a [redb](https://github.com/cberner/redb) database file
- **Signing Key:** An Ed25519 signing key is generated on first run and stored at the configured path

Both files are created automatically if they don't exist.