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
  fetches can be made to fail; events can be delivered twice. The finality
  depth is the deployment's `security_param` of 30 (`FINALITY_DEPTH`), so
  what is mined but not finalized lives as long as it does on the network;
  the deep-fork chains give the chain a finality depth of plain blocks
  after a reorg to that depth, since nothing finalizes between two of them
  otherwise. The node
  applies the ledger rules, so a block never carries an invalid
  transaction, like a leader dropping what went stale. A post validates
  nothing against the ledger and a re-post of what the mempool holds
  succeeds, as on the node. The funding wallet hands out fee notes like
  the wallet service: a note funded into a transaction is reserved until
  it is spent in a block or LIB moved `PENDING_NOTE_EXPIRY_BLOCKS` (10)
  past the reservation, then handed out again. Handles are tagged
  per subscriber so a test knows which tip each sequencer has been told
  about. Several nodes form a network: `import_from` adopts another node's
  blocks, `gossip_with` exchanges mempools.
- `src/writers.rs`: other sequencers as the network shows them. A writer is
  an accredited key nobody runs an SDK for; in its turns the world puts its
  inscriptions, chained inscriptions, note re-keying transfers, withdraws and
  config changes straight into the blocks it mines, plain or fork, so forks
  can carry different notes and rules than the chain they replace. Like a
  sequencer it re-lands its own orphans while the chain would accept them.
- `src/chains.rs`: the catalogue. The SDK sees one thing, an ordered
  stream of block events plus the answers to its own calls, so a network
  condition is a shape of that stream and the shapes that matter are a
  list: linear growth, empty slots, half and reversed posting, delayed
  inclusion, shallow and deep forks, ties, a dormant branch revived, split
  tips, lagging and duplicate deliveries, LIB frozen then jumping, forks
  pruned, stream drops, node outages, restarts from checkpoint and from
  scratch, failing fetches, price moves, and the orderings known to matter
  (a fork after a price rise, a restart or a drop under a dormant fork,
  forks under a frozen LIB), plus two partitioning nodes. Each entry is a
  named, deterministic sequence of moves applied cyclically, and every
  story runs on every entry through the `scenario!` macro. Each entry
  promises what it shows (a reorg, a reconnect, a merge) and
  `World::finish` checks the promise once the story ran three cycles;
  `s00_chain_does_what_it_says` checks it on its own for every entry. The
  second tier, `chains::pair`, is every ordered pair of the base conditions
  two blocks apart (`for_each_base_condition!`), run on four core stories:
  about two thousand tests, where order is what is tested.
- `src/events.rs`: one `Event` per operation the world exposes, chain
  moves, client conditions and the zone's own actions (publish, deposit,
  withdraw, pin, config, hand-built tx, abandon, burst, adopt, stop and
  start a sequencer), applied by `World::apply_event`, which resolves what
  the state cannot do the way the network would: an impossible fork
  becomes a plain block, an action on an absent sequencer is skipped, a
  config keeps every running sequencer accredited. A catalogue move is two
  events; the random hunt draws a list of them.
- `src/consumer.rs`: the reference consumer, the least a zone has to do on
  top of the SDK's contract: publish its planned payloads once ready and
  re-publish its own orphaned inscriptions, tracking message lineage so an
  intent lands once however often the chain reorganizes. A test can hand it
  `Choices` to make between steps (abandon an orphan, burst in turn, adopt
  others' orphans) with `Claims` shared across a run's consumers so an
  adopted orphan lands once. It uses only the public sequencer API, so it
  can move into the SDK as the example consumer. `src/policy.rs` holds the
  `Policy` trait it implements.
- `src/layers.rs`: what a sequencer must get right, stacked. Each layer is
  only meaningful when the ones below it hold, so every violation is tagged
  and a run reports the lowest broken layer: `Chain` (the non-finalized
  view is the canonical chain's channel content, in order), `Lineage`
  (finalized txs reported once, in order; each payload finalizes once in
  one message lineage), `Configs` (the turn view follows the branch's
  config lineage), `Notes` (the wallet is the ledger's note set, deposits
  are reported once from canonical blocks), `Restarts` (a sequencer resumed
  from a checkpoint reports what one that never stopped would).
- `src/view_checker.rs`: the `ChannelUpdate` contract on every event, with
  entries identified by message (the same message lands again under a new
  tx hash when re-funded or rebuilt, and the contract has consumers dedup
  by `this_msg`) and by tx hash when they carry none; with
  `RUST_LOG=debug` it traces each event's prefix, adopted, orphaned,
  finalized and pending sets.
- `src/mirror.rs`: an LEZ-style consumer that rebuilds the channel view from
  `ChannelUpdate`s (append on extension, rebuild on conflict) and is compared
  with the node's canonical chain after every settle.
- `src/oracles.rs`: after every settle, once a sequencer has caught up with
  its node (connected, told about the tip, agreeing on LIB): reported
  deposits come from canonical blocks and every canonical deposit has been
  reported, the channel wallet view equals the ledger's channel notes
  (finalized against LIB, unfinalized against the tip), the turn view agrees
  with `round_robin`, and the finalized history it reported is the chain's,
  once each and in order. Four judge promptness on the sequencer's own two
  clocks, the stream and the paused tokio time its resubmit timer runs on:
  every post of an own inscription was in turn, with the configured
  margin, at the stream position the sequencer had reached (the node
  records the tips each subscriber's stream had delivered by the post); an
  own pending inscription chaining on the channel tip is posted as its
  owner sees the turn, or on the timer's next tick in turn, whose instants
  are known since the timer starts with the sequencer; a posted own
  inscription left unmined is re-funded neither before its funding stamp
  is `stale_refund_slots` LIB slots old nor later than the next tick in
  turn after that (the checkpoint carries each pending tx's stamp and the
  sequencer's LIB slot); a restart from a checkpoint restores every own
  pending message the chain had not finalized, or sheds it as orphaned.
  Hand-built txs the test submits whole are exempt, as the SDK posts them
  without a turn.
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
- `tests/scenarios/`: the stories, each run on every chain of the
  catalogue, by theme: `publishing.rs` (messages, checkpoints, same-key
  instances, turns, a foreign writer, the stale window's edge), `configs.rs` (re-keying, orphaning
  by config, posting timeouts), `notes.rs` (deposits, withdraws, pins,
  refunds, the wallet view), `bundles.rs` (multi-sig rounds and hand-built
  custom txs), `tiers.rs` (the catalogue's self-check and the pairs tier)
  and `known_issues.rs` (ignored single tests with the issue in the
  reason). `main.rs` holds the
  shared helpers and the `scenario!` macro. Every story runs the reference
  consumer; there are no scripted reaction policies. The balance-aware and
  sorted-republish cucumber scenarios were dropped: what they exercised in
  the SDK, a consumer that lets orphans die or picks up another sequencer's,
  is what the property's Abandon and Adopt steps draw at random, and the
  rest was the zone's own arithmetic.
- `tests/gas.rs`: the fee model: paid equals priced, unfunded txs are
  evicted, price rises are paid.

- `tests/properties.rs`: the property tests, run with everything else
  (`PROPTEST_CASES` raises the volume). A scenario is a list of `Event`s drawn from the
  whole vocabulary, 16 to 256 of them, applied in order. The
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
  and note transfers; plus restarts, outages and price moves. From the
  second rung the sequencers also make a zone's choices at random: let an
  orphan die, burst-publish on the next turn, adopt another sequencer's
  orphans (`Choices` on the reference consumer). A schedule that runs into
  a known SDK issue (`World::known_issue` recognises each from what the
  run left behind) is counted and logged instead of failed, so the hunt
  keeps drawing from the whole vocabulary and anything else that breaks in
  those regions still fails; `--success-output immediate` shows the
  counts. `SIM_EVENTS=min..max` sets the schedule length. A failing
  seed is written to `tests/properties.proptest-regressions` and replayed
  first by the next run, so a rare failure can be rerun with
  more logging; the schedule it prints is the hint for a hand-written
  story that names the mechanism, next to the other stories, or for a new
  catalogue entry when it is a chain shape nobody had listed. The seed
  file is not kept in git: a seed outlives neither a change to the
  strategy nor a fix. `PROPTEST_CASES` raises the volume,
  `PROPTEST_MAX_SHRINK_ITERS=0` shows a failing input without shrinking,
  `RUST_LOG=debug` adds the SDK's log and the per-event trace.

Known SDK issues are kept as `#[ignore]`d tests with the issue in the reason
string (`cargo test -- --ignored` runs them); they turn green when the fix
lands. Today: a pending atomic withdraw or pin deposit whose channel input
vanished with a reorged-out deposit is re-funded and resubmitted forever
while later publishes chain behind it; a config landing sheds the pending inscriptions
chained on the channel tip but not a custom (multi-inscription) tx chained
on one of them, which leaves the view unreported until the next event. The
property tests draw both, withdraws that may spend unfinalized notes and
writers' two-inscription txs, and recognise the two outcomes. The
frozen-LIB backlog stall of #3858 was one of these until the fix landed;
its story now runs green with the others.

A test needs `#[tokio::test(start_paused = true)]`; SDK timers fire only
through `Harness::advance`, and `World::step` advances the clock by a little
over a second per block so resubmits run at their live cadence. Never
advance the clock by whole slots: the SDK's slot clock anchors on the
standard library's monotonic clock while its turn wake-ups sleep on tokio's,
so hours of paused time make every wake-up deadline look past. Empty slots
are modelled by block slot gaps.
