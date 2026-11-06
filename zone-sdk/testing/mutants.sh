#!/usr/bin/env bash
# Mutation-test the zone-sdk sequencer against the simulation bed.
#
# Pass 1 runs every chosen mutant through the random tier's top rung
# (`every_schedule_finalizes_each_payload_once`, the whole vocabulary) with a
# handful of schedules: the question is whether random schedules catch it.
# Pass 2 runs the survivors through the catalogue: a survivor the catalogue
# catches is a gap in the random tier, one nobody catches is equivalent or a
# gap in the bed. Both passes resume where they left off (`--iterate` skips
# mutants already caught), so the script can be killed and restarted.
# cargo-mutants works on copies of the tree; the checkout is never modified.
#
#   cargo install cargo-mutants
#   zone-sdk/testing/mutants.sh            # both passes
#   zone-sdk/testing/mutants.sh list       # only count the mutants
#
# Results: zone-sdk/testing/mutants/random/mutants.out/{caught,missed,
# timeout,unviable}.txt after pass 1, mutants/catalogue/mutants.out/ after
# pass 2; each mutant's test output is under log/ next to them.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
# Runs cargo as the shell does: whatever the build needs in the environment,
# such as SDKROOT on macOS, is set before calling this.

# The files that hold the sequencer's logic; the actor plumbing, handles,
# client wrappers and type conversions are left out.
MUTATE=(-p logos-blockchain-zone-sdk
  --file zone-sdk/src/sequencer/state.rs
  --file zone-sdk/src/sequencer/block_fetch/mod.rs
  --file zone-sdk/src/sequencer/zone_sequencer.rs
  --file zone-sdk/src/sequencer/block_states.rs
  --file zone-sdk/src/sequencer/channel_wallet.rs
  --file zone-sdk/src/sequencer/backfill.rs
  --file zone-sdk/src/sequencer/tx_builder.rs
  --file zone-sdk/src/sequencer/slot_clock.rs)
# The baseline also runs the mutated crate's own tests under the filter, which
# matches none of them: `--no-tests=pass` keeps that from failing the baseline.
TESTS=(--test-package logos-blockchain-zone-sdk-testing --test-tool nextest)
# Noise the bed cannot be expected to see (formatting, logging, debug
# output) and accessors, subscriptions and forwarding with no logic in them.
NOISE=(--exclude-re 'fmt|Debug|Display|log_|diagnostic|subscribe_|::handle |::client |::lib |has_block|is_ready|is_connected|_count |tracked_|::info |tx_hash |emit_now|sleep_until|instant_of|system_time_to_instant|wait_reconnect_delay|slot_to_u64|::checkpoint |channel_wallet |channel_history |spendable_channel_wallet')
RANDOM_OUT=zone-sdk/testing/mutants/random
CATALOGUE_OUT=zone-sdk/testing/mutants/catalogue

if [[ "${1:-}" == "list" ]]; then
  cargo mutants "${MUTATE[@]}" "${NOISE[@]}" --list | tee zone-sdk/testing/mutants/list.txt | wc -l
  exit 0
fi

mkdir -p "$RANDOM_OUT" "$CATALOGUE_OUT/mutants.out"
echo "== pass 1: random tier, top rung, $(date)"
# No seed file: cargo-mutants reuses a tree copy from mutant to mutant, and a
# seed one mutant's failure left behind would be replayed against the next.
PROPTEST_CASES=8 PROPTEST_MAX_SHRINK_ITERS=0 PROPTEST_DISABLE_FAILURE_PERSISTENCE=true \
cargo mutants "${MUTATE[@]}" "${TESTS[@]}" "${NOISE[@]}" \
  --output "$RANDOM_OUT" --iterate --shuffle --jobs 6 --timeout 1500 \
  -- --no-tests=pass -E 'test(every_schedule_finalizes_each_payload_once)' || true

echo "== pass 2: catalogue on the survivors, $(date)"
# Seed pass 2 with pass 1's verdicts so only its survivors are rerun.
for verdict in caught timeout unviable; do
  cp "$RANDOM_OUT/mutants.out/$verdict.txt" "$CATALOGUE_OUT/mutants.out/$verdict.txt" 2>/dev/null || true
done
cargo mutants "${MUTATE[@]}" "${TESTS[@]}" "${NOISE[@]}" \
  --output "$CATALOGUE_OUT" --iterate --shuffle --jobs 3 --timeout 420 \
  -- --no-tests=pass -E 'binary(scenarios)' || true

echo "== done, $(date)"
for d in "$RANDOM_OUT/mutants.out" "$CATALOGUE_OUT/mutants.out"; do
  echo "$d: caught $(wc -l < "$d/caught.txt" 2>/dev/null || echo 0), missed $(wc -l < "$d/missed.txt" 2>/dev/null || echo 0), timeout $(wc -l < "$d/timeout.txt" 2>/dev/null || echo 0), unviable $(wc -l < "$d/unviable.txt" 2>/dev/null || echo 0)"
done
