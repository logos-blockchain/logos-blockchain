# Manual TUI Zone Deposit/Withdrawal Demo

This demo uses the existing cucumber `Transactions manual control` scenario as
the chain and wallet bootstrapper, then uses `tui-sequencer` subcommands for
file-based Zone deposit and withdrawal.

The wallet export format is demo-only. `include_secret true` writes private
wallet material to disk so the TUI command can spend exported cucumber wallet
UTXOs into a Zone deposit.

## Start from a clean slate

Remove prior demo files and create the working directories:

```sh
rm -rf /tmp/tui-zone
mkdir -p /tmp/tui-zone/keys /tmp/tui-zone/artifacts
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

## [Cucumber] Export wallet funds

Append commands to `/tmp/cucumber-manual-commands.txt`:

```text
EXPORT_FUNDS, wallet 'WALLET_1A', value 1000, output '/tmp/tui-zone/artifacts/funds-wallet-1a.json', include_secret true
EXPORT_FUNDS, wallet 'WALLET_2A', value 1000, output '/tmp/tui-zone/artifacts/funds-wallet-2a.json', include_secret true
```

Processed commands are marked with `---->`. Invalid commands are marked with
`== ERROR == >`.

The exported funds are not committed on-chain yet, so cucumber wallet balances
will not change until after the corresponding deposit transaction is mined.

Use the `node_url` field from the exported funds JSON files if your node is not
available at `http://localhost:<PORT>`.

## [Sequencer] Create and validate a new channel

Start the sequencer with the channel admin key:

```sh
cargo run -p logos-blockchain-tui-zone -- run \
  --node-url http://localhost:<PORT> \
  --key-path /tmp/tui-zone/keys/seq-a.key
```

Post one inscription and wait until it is shown as adopted/published:

```text
a1
```

Then stop the sequencer with `CTRL-C`.

Create the second, third, and fourth signer keys:

```sh
cargo run -p logos-blockchain-tui-zone -- keygen \
  --key-path /tmp/tui-zone/keys/seq-b.key

cargo run -p logos-blockchain-tui-zone -- keygen \
  --key-path /tmp/tui-zone/keys/seq-c.key

cargo run -p logos-blockchain-tui-zone -- keygen \
  --key-path /tmp/tui-zone/keys/seq-d.key
```

`seq-a.key` is the channel admin key. `seq-b.key`, `seq-c.key`, and
`seq-d.key` are only accredited later by channel config commands.

Print the channel balance at any point:

```sh
cargo run -p logos-blockchain-tui-zone -- state balance \
  --node-url http://localhost:<PORT> \
  --key-path /tmp/tui-zone/keys/seq-a.key
```

## [Sequencer] Deposit

Deposit the first exported wallet funds:

```sh
cargo run -p logos-blockchain-tui-zone -- deposit \
  --node-url http://localhost:<PORT> \
  --key-path /tmp/tui-zone/keys/seq-a.key \
  --funds /tmp/tui-zone/artifacts/funds-wallet-1a.json \
  --amount 1000 \
  --metadata "demo deposit" \
  --message "deposit wallet 1a"
```

Deposit the second exported wallet funds:

```sh
cargo run -p logos-blockchain-tui-zone -- deposit \
  --node-url http://localhost:<PORT> \
  --key-path /tmp/tui-zone/keys/seq-a.key \
  --funds /tmp/tui-zone/artifacts/funds-wallet-2a.json \
  --amount 1000 \
  --metadata "demo deposit" \
  --message "deposit wallet 2a"
```

## [Cucumber] Check deposit balances

Append:

```text
BALANCE, wallet 'WALLET_1A'
BALANCE, wallet 'WALLET_2A'
```

If a balance does not reflect the deposit yet, wait for the deposit transaction
to be mined and observed by the cucumber wallet before proceeding.

## [Sequencer] Single-signer withdrawal

Prepare the withdrawal intent:

```sh
cargo run -p logos-blockchain-tui-zone -- withdraw prepare \
  --node-url http://localhost:<PORT> \
  --key-path /tmp/tui-zone/keys/seq-a.key \
  --amount 500 \
  --recipient-funds /tmp/tui-zone/artifacts/funds-wallet-1a.json \
  --message "withdraw wallet 1a" \
  --out /tmp/tui-zone/artifacts/withdraw.intent.json
```

Sign it with the channel admin key:

```sh
cargo run -p logos-blockchain-tui-zone -- withdraw sign \
  --key-path /tmp/tui-zone/keys/seq-a.key \
  --in /tmp/tui-zone/artifacts/withdraw.intent.json \
  --out /tmp/tui-zone/artifacts/sig-a.json
```

Combine the intent and signature:

```sh
cargo run -p logos-blockchain-tui-zone -- withdraw combine \
  --in /tmp/tui-zone/artifacts/withdraw.intent.json \
  --sig /tmp/tui-zone/artifacts/sig-a.json \
  --out /tmp/tui-zone/artifacts/withdraw.signed.json
```

Submit the signed withdrawal:

```sh
cargo run -p logos-blockchain-tui-zone -- withdraw submit \
  --node-url http://localhost:<PORT> \
  --key-path /tmp/tui-zone/keys/seq-a.key \
  --in /tmp/tui-zone/artifacts/withdraw.signed.json
```

## [Cucumber] Check single-signer withdrawal balance

Append:

```text
BALANCE, wallet 'WALLET_1A'
```

## [Sequencer] Configure multi-signers

Configure the Zone channel created by `seq-a.key` so the accredited withdrawal
keys contain the first three local sequencer keys, the withdrawal threshold is
`2`, and future configuration changes require `2` signatures:

```sh
cargo run -p logos-blockchain-tui-zone -- config apply \
  --node-url http://localhost:<PORT> \
  --key-path /tmp/tui-zone/keys/seq-a.key \
  --authorized-key-path /tmp/tui-zone/keys/seq-a.key \
  --authorized-key-path /tmp/tui-zone/keys/seq-b.key \
  --authorized-key-path /tmp/tui-zone/keys/seq-c.key \
  --configuration-threshold 2 \
  --withdraw-threshold 2 \
  --posting-timeframe 30 \
  --posting-timeout 30
```

The `--key-path` key signs this threshold-1 update and is kept at authorized key
index `0`; duplicate `--authorized-key-path` entries are ignored.

## [Sequencer] Multi-signer withdrawal

Prepare the 2-of-3 withdrawal intent:

```sh
cargo run -p logos-blockchain-tui-zone -- withdraw prepare \
  --node-url http://localhost:<PORT> \
  --key-path /tmp/tui-zone/keys/seq-a.key \
  --amount 500 \
  --recipient-funds /tmp/tui-zone/artifacts/funds-wallet-1a.json \
  --message "withdraw wallet 1a multisig" \
  --out /tmp/tui-zone/artifacts/withdraw-2of3.intent.json
```

Sign the same intent with two different authorized signer keys:

```sh
cargo run -p logos-blockchain-tui-zone -- withdraw sign \
  --key-path /tmp/tui-zone/keys/seq-a.key \
  --in /tmp/tui-zone/artifacts/withdraw-2of3.intent.json \
  --out /tmp/tui-zone/artifacts/sig-a.json

cargo run -p logos-blockchain-tui-zone -- withdraw sign \
  --key-path /tmp/tui-zone/keys/seq-b.key \
  --in /tmp/tui-zone/artifacts/withdraw-2of3.intent.json \
  --out /tmp/tui-zone/artifacts/sig-b.json
```

Combine both signatures:

```sh
cargo run -p logos-blockchain-tui-zone -- withdraw combine \
  --in /tmp/tui-zone/artifacts/withdraw-2of3.intent.json \
  --sig /tmp/tui-zone/artifacts/sig-a.json \
  --sig /tmp/tui-zone/artifacts/sig-b.json \
  --out /tmp/tui-zone/artifacts/withdraw-2of3.signed.json
```

Submit the signed withdrawal:

```sh
cargo run -p logos-blockchain-tui-zone -- withdraw submit \
  --node-url http://localhost:<PORT> \
  --key-path /tmp/tui-zone/keys/seq-a.key \
  --in /tmp/tui-zone/artifacts/withdraw-2of3.signed.json
```

Each signer keeps its private key local. The only exchanged files are the intent
JSON and signature JSON files.

## [Cucumber] Check multi-signer withdrawal balance

Append:

```text
BALANCE, wallet 'WALLET_1A'
```

The withdrawn funds are normal chain notes addressed to the exported cucumber
wallet public key, so they are observed through the existing wallet scan path.

## [Sequencer] Multi-signer config update

Prepare a 2-of-3 configuration update that adds the fourth key and raises both
thresholds to `3`:

```sh
cargo run -p logos-blockchain-tui-zone -- config prepare \
  --node-url http://localhost:<PORT> \
  --key-path /tmp/tui-zone/keys/seq-a.key \
  --authorized-key-path /tmp/tui-zone/keys/seq-a.key \
  --authorized-key-path /tmp/tui-zone/keys/seq-b.key \
  --authorized-key-path /tmp/tui-zone/keys/seq-c.key \
  --authorized-key-path /tmp/tui-zone/keys/seq-d.key \
  --configuration-threshold 3 \
  --withdraw-threshold 3 \
  --posting-timeframe 30 \
  --posting-timeout 30 \
  --out /tmp/tui-zone/artifacts/config-3of4.intent.json
```

Sign the config intent with two currently authorized keys:

```sh
cargo run -p logos-blockchain-tui-zone -- config sign \
  --key-path /tmp/tui-zone/keys/seq-a.key \
  --in /tmp/tui-zone/artifacts/config-3of4.intent.json \
  --out /tmp/tui-zone/artifacts/config-sig-a.json

cargo run -p logos-blockchain-tui-zone -- config sign \
  --key-path /tmp/tui-zone/keys/seq-b.key \
  --in /tmp/tui-zone/artifacts/config-3of4.intent.json \
  --out /tmp/tui-zone/artifacts/config-sig-b.json
```

Combine the signatures:

```sh
cargo run -p logos-blockchain-tui-zone -- config combine \
  --in /tmp/tui-zone/artifacts/config-3of4.intent.json \
  --sig /tmp/tui-zone/artifacts/config-sig-a.json \
  --sig /tmp/tui-zone/artifacts/config-sig-b.json \
  --out /tmp/tui-zone/artifacts/config-3of4.signed.json
```

Submit the signed config update:

```sh
cargo run -p logos-blockchain-tui-zone -- config submit \
  --node-url http://localhost:<PORT> \
  --key-path /tmp/tui-zone/keys/seq-a.key \
  --in /tmp/tui-zone/artifacts/config-3of4.signed.json
```

Check the full channel state:

```sh
cargo run -p logos-blockchain-tui-zone -- state full \
  --node-url http://localhost:<PORT> \
  --key-path /tmp/tui-zone/keys/seq-a.key
```

## [Cucumber] Stop cucumber scenario

Append:

```text
STOP
```
