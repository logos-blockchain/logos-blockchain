# Manual TUI Zone Deposit/Withdrawal Demo

This demo uses the existing cucumber `Transactions manual control` scenario as the chain and wallet bootstrapper, then
uses `tui-sequencer` subcommands for file-based Zone deposit and withdrawal.

The wallet export format is demo-only. `include_secret true` writes private wallet material to disk so the TUI command
can spend exported cucumber wallet UTXOs into a Zone deposit.

## Start from a clean slate

Remove prior demo files and create the working directory:

```sh
rm -rf /tmp/tui-zone
mkdir -p /tmp/tui-zone
```

## [Cucumber] Start cucumber manual control

```sh
export CUCUMBER_MANUAL_COMMAND_FILE=/tmp/cucumber-manual-commands.txt
export CUCUMBER_LOG_LEVEL=trace
export CUCUMBER_VERBOSE_CONSOLE=true
cargo test -p logos-blockchain-tests --features cucumber --test cucumber -- --name "Transactions manual control"
```

Wait until the scenario reaches:

```text
When I perform manual control of transactions for all wallets no time-out
```

## Export cucumber wallet funds

Append commands to `/tmp/cucumber-manual-commands.txt`:

```text
BALANCE, wallet 'WALLET_1A'
EXPORT_FUNDS, wallet 'WALLET_1A', value 1000, output '/tmp/tui-zone/funds-wallet-1a.json', include_secret true
```

**Note:** Processed commands are marked with `---->`. Invalid commands are marked with `== ERROR == >`.

The output file contains the exported funds information, including the private key material needed for the TUI commands.
**Note:** The funds export will not be reflected on-chain until after the deposit transaction has been mined.

**Note:** `--node-url` to be used in the next steps should be extracted from the cucumber export funds output \
`node_url` field in '/tmp/tui-zone/funds-wallet-1a.json'.

## [Sequencer] Create a new channel

```sh
cargo run -p logos-blockchain-tui-zone -- run --node-url http://localhost:<PORT> --key-path /tmp/tui-zone/seq-a.key
```

CTRL-C to stop after the channel is created and the public key is printed, then proceed to the next steps.

Create additional local sequencer signing keys. This does not contact the node and does not create channels; it only
creates or
loads each key file and prints its public key.

```sh
cargo run -p logos-blockchain-tui-zone -- keygen --key-path /tmp/tui-zone/seq-b.key

cargo run -p logos-blockchain-tui-zone -- keygen --key-path /tmp/tui-zone/seq-c.key
```

`seq-a.key` is used as the channel admin key in the commands below. `seq-b.key` and `seq-c.key` are only accredited
later by the channel config command.

## [Sequencer] Deposit

```sh
cargo run -p logos-blockchain-tui-zone -- deposit \
  --node-url http://localhost:<PORT> \
  --key-path /tmp/tui-zone/seq-a.key \
  --funds /tmp/tui-zone/funds-wallet-1a.json \
  --amount 1000 \
  --metadata "demo deposit" \
  --message "deposit wallet 1a" \
  --submit
```

Repeat with another exported funds file for a second deposit if needed.

## [Cucumber] Refresh cucumber wallet

Append:

```text
BALANCE, wallet 'WALLET_1A'
```

If the new balance does not reflect the deposit, wait for the deposit transaction to be mined and observed by the cucumber wallet before proceeding.

## [Sequencer] Single-Signer Withdrawal

```sh
cargo run -p logos-blockchain-tui-zone -- withdraw prepare \
  --node-url http://localhost:<PORT> \
  --key-path /tmp/tui-zone/seq-a.key \
  --amount 500 \
  --recipient-funds /tmp/tui-zone/funds-wallet-1a.json \
  --message "withdraw wallet 1a" \
  --out /tmp/tui-zone/withdraw.intent.json

cargo run -p logos-blockchain-tui-zone -- withdraw sign \
  --key-path /tmp/tui-zone/seq-a.key \
  --in /tmp/tui-zone/withdraw.intent.json \
  --out /tmp/tui-zone/sig-a.json

cargo run -p logos-blockchain-tui-zone -- withdraw combine \
  --in /tmp/tui-zone/withdraw.intent.json \
  --sig /tmp/tui-zone/sig-a.json \
  --out /tmp/tui-zone/withdraw.signed.json

cargo run -p logos-blockchain-tui-zone -- withdraw submit \
  --node-url http://localhost:<PORT> \
  --in /tmp/tui-zone/withdraw.signed.json
```

## [Sequencer] Multi-Signer Withdrawal

First configure the Zone channel created by `seq-a.key` so the accredited
withdrawal keys contain all three local sequencer keys and the withdrawal
threshold is `2`:

```sh
cargo run -p logos-blockchain-tui-zone -- config \
  --node-url http://localhost:<PORT> \
  --key-path /tmp/tui-zone/seq-a.key \
  --authorized-key-path /tmp/tui-zone/seq-a.key \
  --authorized-key-path /tmp/tui-zone/seq-b.key \
  --authorized-key-path /tmp/tui-zone/seq-c.key \
  --configuration-threshold 1 \
  --withdraw-threshold 2 \
  --posting-timeframe 30 \
  --posting-timeout 30
```

The `--key-path` key is the channel admin key and is kept at authorized key
index `0`; duplicate `--authorized-key-path` entries are ignored.

The file flow is the same as the single-signer case, except two different
signer key files sign the same intent before combining:

```sh
cargo run -p logos-blockchain-tui-zone -- withdraw prepare \
  --node-url http://localhost:<PORT> \
  --key-path /tmp/tui-zone/seq-a.key \
  --amount 500 \
  --recipient-funds /tmp/tui-zone/funds-wallet-1a.json \
  --message "withdraw wallet 1a multisig" \
  --out /tmp/tui-zone/withdraw-2of3.intent.json

cargo run -p logos-blockchain-tui-zone -- withdraw sign \
  --key-path /tmp/tui-zone/seq-a.key \
  --in /tmp/tui-zone/withdraw-2of3.intent.json \
  --out /tmp/tui-zone/sig-a.json

cargo run -p logos-blockchain-tui-zone -- withdraw sign \
  --key-path /tmp/tui-zone/seq-b.key \
  --in /tmp/tui-zone/withdraw-2of3.intent.json \
  --out /tmp/tui-zone/sig-b.json

cargo run -p logos-blockchain-tui-zone -- withdraw combine \
  --in /tmp/tui-zone/withdraw-2of3.intent.json \
  --sig /tmp/tui-zone/sig-a.json \
  --sig /tmp/tui-zone/sig-b.json \
  --out /tmp/tui-zone/withdraw-2of3.signed.json

cargo run -p logos-blockchain-tui-zone -- withdraw submit \
  --node-url http://localhost:<PORT> \
  --in /tmp/tui-zone/withdraw-2of3.signed.json
```

Each signer keeps its private key local. The only exchanged files are the intent
JSON and signature JSON files.

## [Cucumber] Refresh cucumber wallet

Append:

```text
BALANCE, wallet 'WALLET_1A'
```

The withdrawn funds are normal chain notes addressed to the exported cucumber wallet public key, so they are observed
through the existing wallet scan path.

## [Cucumber] Stop cucumber scenario

Append:

```text
STOP
```
