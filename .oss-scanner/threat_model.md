# Threat model

Logos Blockchain is the reference node of the Logos blockchain.
Its specifications (https://lip.logos.co/blockchain/) are not available offline: here, the code and its doc comments are the reference.
This is a first, generic version, to be refined as scans come in.

## What this project does and where untrusted input enters

The node runs Cryptarchia, a proof-of-stake consensus with private leader election: a slot leader proves its eligibility with a zero-knowledge proof of leadership (PoL) and publishes its block proposal through Blend, a mix network that hides which node proposed it.
The ledger (Mantle) is UTXO-based: notes, Ed25519 and zero-knowledge signatures, channels that
zones (L2s) use for inscriptions, deposits and withdrawals, service declarations (SDP) through which nodes join services such as Blend, and leader and proof-of-work reward claims.
Proofs are Groth16 over BN254: proving runs native witness generators and rapidsnark, verification lives in `zk/`.

Untrusted input, roughly from most to least exposed:

- **The libp2p swarm** (QUIC; `libp2p/`, `services/network/`). Any peer can connect.
  Gossipsub carries transactions (the `…/mempool/…` topic) and block proposals (`…/cryptarchia/…`), chain sync (`consensus/cryptarchia-sync/`) serves and requests blocks, and Kademlia, identify and AutoNAT run alongside.
- **Blend** (QUIC; `blend/`, `services/blend/`): layered, encrypted messages from other Blend nodes and from edge clients, carrying signatures and proofs of quota.
- **The contents of transactions and blocks**: every Mantle operation, signature, proof, header and leader proof is attacker-controlled until validated (`core/`, `ledger/`, `services/chain/`, `zk/`).
- **NTP replies** (`services/time/`): unauthenticated UDP from the configured server.
- **The HTTP API** (`nodes/node/binary/src/api/`, every route listed in `routes.rs`). It has
  no authentication and it moves the node's funds, signs transactions and controls mining, so whoever reaches it controls the node: it is an operator interface. In scope is anything that lets someone else reach or drive it, such as a web page in the operator's browser (cross-origin requests, DNS rebinding).
- **The C bindings** (`c-bindings/`): applications start and drive a node in-process through this library (`start_lb_node` and the functions in `logos_blockchain.h`). The host application is trusted, but the bindings must stay memory-safe for any input their C API accepts.

Trusted: the operator, the config files and CLI arguments, the local disk (state, database, keys) and whoever can reach the HTTP API.

## Components that matter most / least

Most:

- Consensus and chain handling: `consensus/cryptarchia-engine/` (fork choice) and `services/chain/` (block validation and application, sync, leadership).
- Ledger rules: `ledger/`, `core/` (transactions, operations, fees and gas, SDP, rewards), `utxotree/`, `mmr/`.
- Verification: `zk/groth16/`, `zk/proofs/`, `zk/circuits/verifier/`, and the signatures and keys in `kms/`.
- Decoding of network input: `binary-codec/` and the wire types in `core/`, `blend/message/` and
  `consensus/cryptarchia-sync/`.
- Blend (`blend/`, `services/blend/`): the anonymity of block proposers rests on it.
- Resource limits on network input: `libp2p/`, `services/network/`, `services/tx-service/` (the mempool).
- The unsafe FFI in `c-bindings/`.

Least:

- Client-side libraries: `zone-sdk/`, `logos_sql/`, `wallet-http-client/`. In scope, at a lower priority.
- Operator and testnet tooling: `tools/`, `deployment/` (the testnet faucet among it), `scripts/`.
- Tests: `tests/`, benches and `#[cfg(test)]` code.

## How to exercise it

The image has the checkout at `/src`, the toolchain from `rust-toolchain.toml` with clippy, and no network.
Every crate, the circuit artifacts (`~/.cache/logos/blockchain`) and rapidsnark (`$RAPIDSNARK_LIB_DIR`) are already in it, and `CARGO_NET_OFFLINE=true`.

Only the node is prebuilt: `target/release/logos-blockchain-node`, in the release profile with the `testing` feature, also at `$LOGOS_BLOCKCHAIN_NODE_BIN`. `testing` only adds serde derives to `RunConfig`: the node runs production code. Release builds are stripped, so its backtraces carry no symbols. Tests compile on first use, on 2 CPUs, so keep runs narrow, with `RUST_BACKTRACE=1`:

    cargo test -p <crate> <test name filter>

A one-node network, with its state and database in the current directory:

    mkdir -p /tmp/node && cd /tmp/node
    "$LOGOS_BLOCKCHAIN_NODE_BIN" /src/nodes/node/standalone-node-config.yaml \
      --deployment /src/nodes/node/standalone-deployment-config.yaml

It listens on UDP 3000 (libp2p) and UDP 3400 (Blend) and serves the HTTP API on `127.0.0.1:8080`, with the OpenAPI document at `/api-docs/openapi.json`.
NTP sync fails offline and only logs a warning: slots follow the system clock.

Several nodes: the end-to-end tests in `tests/` (crate `logos-blockchain-tests`, targets listed in `tests/Cargo.toml`) start nodes from `$LOGOS_BLOCKCHAIN_NODE_BIN`, as in `cargo nextest run -p logos-blockchain-tests --test test_mantle_chain_start`.
Compiling them takes a while on 2 CPUs.

The FFI safety tests are `#[ignore]`d; CI runs them under valgrind:

    FFI_AUDIT_WAIT_SECS=600 \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER="valgrind --leak-check=full --errors-for-leak-kinds=definite \
      --error-exitcode=1 --suppressions=/src/c-bindings/valgrind.supp" \
      cargo test -p logos-blockchain-c -- --ignored --test-threads=1 ffi_safety_tests

A proof of concept is most useful as a test in the affected crate, or as a script against a local node for network and API issues.
Keep patches to the smallest change that fixes the issue, in the style of the surrounding code.

## How you rate severity

The attackers are any network peer, any transaction sender, Blend nodes and slot leaders.
Rate as if the network held real value, by what an attacker with ordinary resources can do.
If an attack needs a large share of the stake or of the Blend nodes, say so and rate it one level lower.

- **Critical**: the ledger or consensus accepts what it must not, or honest nodes diverge: inflation, double spends, spending without the key, forged proofs or signatures accepted, a chain split between honest nodes. Also remote code execution or memory corruption reachable from the network or from transaction contents, and theft or disclosure of secret keys.
- **High**: a single peer, or a single crafted transaction, block or message, crashes or hangs a node, exhausts its memory or halts block production; a block proposer or Blend sender can be identified; state is corrupted so that a node cannot restart without manual repair.
- **Medium**: denial of service that needs sustained traffic or resources comparable to its effect; a rate or size limit bypassed without a crash; privacy leaks short of identifying a proposer or sender.
- **Low**: anything that needs the operator's cooperation, configuration, disk or HTTP API access; hardening without a demonstrated exploit.

## Anything to leave alone

- Code compiled only for tests or instrumentation: `#[cfg(test)]`, `tests/`, benches, and the features `testing`, `testing-disable-proposal-publish`, `test-utils`, `unsafe-test-functions`, `dhat-heap`, `profiling`, `tokio-console` and `tokio-task-names`. The `unsafe` feature of the KMS crates is the exception: node builds enable it, so the code behind it is in scope.
- Keys and secrets in committed configs (`nodes/node/*.yaml`, `deployment/`, `tests/`): they are test and devnet data.
- The circuits and rapidsnark, which come from other repositories (how this repository calls them and checks their results is in scope), and bugs in third-party crates, unless this repository uses them wrongly.
- Panics or errors on invalid local configuration or CLI arguments.