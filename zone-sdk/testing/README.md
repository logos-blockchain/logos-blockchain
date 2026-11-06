# zone-sdk testing

Deterministic test bed for the zone SDK. A simulated node applies the Mantle
channel rules and answers every `adapter::Node` call from one block tree;
the harness drives real, unmodified `ZoneSequencer`s against it on a paused
tokio clock, so a scenario runs in milliseconds and never depends on timing.

**Why.** The SDK's hard cases are forks, late or duplicate deliveries, a
frozen LIB, restarts, outages and price moves in a particular order. Against
a real node those happen when they happen and cannot be reproduced. Here
every condition is a named sequence of moves, every story runs on every
one of them, and random schedules hunt for the orderings nobody listed.

**What.** Two kinds of test, judged by the same checkers:

- stories on a catalogue of 25 chain shapes, plus every ordered pair of the
  base conditions: about 2 780 deterministic tests;
- property schedules of 16 to 256 random events, drawn one layer at a time
  so the lowest failing rung names what the SDK gets wrong.

Only invariants are asserted: the view is the canonical chain, every payload
finalizes exactly once in one lineage, the wallet is the ledger's note set,
a restarted sequencer reports what one that never stopped would.

**How.**

```sh
cargo test -p logos-blockchain-zone-sdk-testing               # everything
cargo test -p logos-blockchain-zone-sdk-testing -- --ignored  # known SDK issues still open
PROPTEST_CASES=200 SIM_EVENTS=64..512 cargo test -p logos-blockchain-zone-sdk-testing properties
RUST_LOG=debug ...                                            # SDK log plus the per-event trace
```

A test needs `#[tokio::test(start_paused = true)]` and advances the clock
only through the harness; empty slots are block slot gaps, never whole slots
of paused time.

**Layout.** `src/node.rs` and `src/ledger.rs` are the simulated node;
`src/chains.rs` the catalogue; `src/events.rs` the event vocabulary;
`src/world.rs` and `src/harness.rs` drive a run; `src/writers.rs` the other
sequencers; `src/consumer.rs` the reference consumer; `src/layers.rs`,
`src/view_checker.rs`, `src/mirror.rs` and `src/oracles.rs` the checkers;
`tests/scenarios/` the stories by theme, `tests/properties.rs` the ladder,
`tests/gas.rs` the fee model.

[ARCHITECTURE.md](ARCHITECTURE.md) explains all of it top-down: the
simulated node, the step vocabulary, each catalogue entry, how a random
schedule is generated and judged, and how a failure becomes a story.
