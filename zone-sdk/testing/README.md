# zone-sdk testing

Deterministic test bed for the zone SDK. A simulated node applies the Mantle
channel rules and answers every `adapter::Node` call from one block tree; the
harness drives real `ZoneSequencer`s against it on a paused tokio clock, so a
scenario runs in milliseconds and never depends on timing.

```
cargo test -p logos-blockchain-zone-sdk-testing
```

- `src/ledger.rs`: channel state, channel notes and plain notes; Ed25519
  proofs verified, ZK proofs trusted. Every transaction pays the real
  execution and storage gas (`MainnetGasProfile`) at the node's gas prices,
  genesis prices by default (`SimNode::set_gas_prices`).
- `src/node.rs`: `SimNode`, the block tree. The test decides every block:
  parent, slot, which posted transactions it takes and in what order
  (`TxSelection`), which extra transactions go in ahead of them, and
  whether the block is announced to subscribers at all (a lagging
  subscriber misses events). LIB trails the tip by the finality depth, or
  stays frozen like under the bootstrap fork-choice rule and jumps on thaw;
  forks below LIB can be pruned so their blocks are no longer fetchable;
  fetches can be made to fail; events can be delivered twice. The node
  applies the ledger rules, so a block never carries an invalid
  transaction, like a leader dropping what went stale. Handles are tagged
  per subscriber so a test knows which tip each sequencer has been told
  about. Several nodes form a network: `import_from` adopts another node's
  blocks, `gossip_with` exchanges mempools.
- `src/writers.rs`: other sequencers as the network shows them. A writer is
  an accredited key nobody runs an SDK for; in its turns the world puts its
  inscriptions, chained inscriptions, note re-keying transfers, withdraws and
  config changes straight into the blocks it mines, plain or fork, so forks
  can carry different notes and rules than the chain they replace. Like a
  sequencer it re-lands its own orphans while the chain would accept them.
- `src/consumer.rs`: the reference consumer, the least a zone has to do on
  top of the SDK's contract: publish its planned payloads once ready and
  re-publish its own orphaned inscriptions, tracking message lineage so an
  intent lands once however often the chain reorganizes. It uses only the
  public sequencer API, so it can move into the SDK as the example
  consumer. `src/policy.rs` holds the `Policy` trait it implements.
- `src/layers.rs`: what a sequencer must get right, stacked. Each layer is
  only meaningful when the ones below it hold, so every violation is tagged
  and a run reports the lowest broken layer: `Chain` (the non-finalized
  view is the canonical chain's channel content, in order), `Lineage`
  (finalized txs reported once, in order; each payload finalizes once in
  one message lineage), `Configs` (the turn view follows the branch's
  config lineage), `Notes` (the wallet is the ledger's note set, deposits
  are reported once from canonical blocks), `Restarts` (a sequencer resumed
  from a checkpoint reports what one that never stopped would).
- `src/view_checker.rs`: the `ChannelUpdate` contract on every event; with
  `RUST_LOG=debug` it traces each event's prefix, adopted, orphaned,
  finalized and pending sets.
- `src/mirror.rs`: an LEZ-style consumer that rebuilds the channel view from
  `ChannelUpdate`s (append on extension, rebuild on conflict) and is compared
  with the node's canonical chain after every settle.
- `src/oracles.rs`: after every settle, once a sequencer has caught up with
  its node (connected, told about the tip, agreeing on LIB): reported
  deposits come from canonical blocks, the channel wallet view equals the
  ledger's channel notes (finalized against LIB, unfinalized against the
  tip), the turn view agrees with `round_robin`, and the finalized history
  it reported is the chain's, once each and in order.
- `src/harness.rs`: owns the sequencers, polls `next_event` until quiescent,
  runs each sequencer's policy and the checkers inline, and replays
  finalized history like an indexer.
- `src/world.rs`: scenario helpers, the `Network` behaviours the scenario
  ports run on (linear, shallow forks, deep forks to the finality depth,
  stream drops, delayed inclusion, two partitioning nodes with sequencers on
  both sides) and the shape primitives the property composes: extend with a
  slot gap, fork of any depth and length (winning, tying or dormant), revive
  a dormant branch, lagging and split-tip deliveries, and the writers'
  traffic (`add_writer`, `enable_foreign_traffic`).
- `tests/scenarios.rs`: the zone cucumber scenarios plus posting-timeout and
  foreign-writer scenarios, each run on every `Network`. The consumer logic
  the cucumber features script (balance-aware and sorted republishing, the
  deposit and multi-sig lifecycles, custom multi-inscription txs) lives in
  `tests/scenarios/drivers.rs`, one driver per scenario; every other
  scenario runs the reference consumer.
- `tests/gas.rs`: the fee model: paid equals priced, unfunded txs are
  evicted, price rises are paid.

- `tests/properties.rs`: every step draws three things independently. The
  chain's shape: a block with a slot gap, a fork of random depth and length,
  a revival of the dormant branch, silent blocks before an announced one, a
  tip moved silently behind an announced sibling, a duplicate delivery, LIB
  frozen or thawed, forks pruned. The block's content: all, none, half or
  the reversed order of what the sequencers posted, beside whatever the
  writers in turn put in. The client's condition: a dropped stream, a node
  outage, a restart from checkpoint or from scratch, failing fetches, a gas
  price move, a user deposit, a withdraw. One to three sequencers and one
  or two writers share the channel under random posting rules, on one node
  or two partitioning ones. Only invariants are asserted: every planned
  payload finalizes exactly once in one lineage, the sequencers' own
  pending sets drain, and every layer holds, reported lowest first.
  The vocabulary is drawn one layer at a time, so the tests form a ladder
  and the lowest failing rung names what the SDK gets wrong: only other
  writers' inscriptions on a moving chain; plus the sequencers' own
  messages; plus config changes and posting rules; plus deposits, withdraws
  and note transfers; plus restarts, outages and price moves. No failing
  seed is persisted: a persisted seed only reproduces its case while the
  strategy that drew it is unchanged, so the schedule a run shrinks to is
  the hint for a hand-written scenario that names the mechanism, next to
  the other scenarios. `PROPTEST_CASES` raises the volume,
  `PROPTEST_MAX_SHRINK_ITERS=0` shows a failing input without shrinking,
  `RUST_LOG=debug` adds the SDK's log and the per-event trace.

Known SDK issues are kept as `#[ignore]`d tests with the issue in the reason
string (`cargo test -- --ignored` runs them); they turn green when the fix
lands. Today: a pending atomic withdraw whose channel input vanished with a
reorged-out deposit is re-funded and resubmitted forever while later
publishes chain behind it, and a config landing sheds the pending
inscriptions chained on the channel tip but not a custom (multi-inscription)
tx chained on one of them, which leaves the view unreported until the next
event. The ignored property widens the vocabulary to both: withdraws that
may spend unfinalized notes, and writers' two-inscription txs.

A test needs `#[tokio::test(start_paused = true)]`; SDK timers fire only
through `Harness::advance`, and `World::step` advances the clock by a little
over a second per block so resubmits run at their live cadence. Never
advance the clock by whole slots: the SDK's slot clock anchors on the
standard library's monotonic clock while its turn wake-ups sleep on tokio's,
so hours of paused time make every wake-up deadline look past. Empty slots
are modelled by block slot gaps.
