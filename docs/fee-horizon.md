# Fee horizon projection

`epoch_headroom` is the amount of future elapsed chain time, measured in epochs
from transaction preparation, for which the wallet provisions mandatory-fee
headroom. It is not a count of storage-price updates. The public value accepts
any number of decimal digits, but extra digits are discarded rather than
rounded. The canonical internal representation is integer tenths, so `1.39`
becomes `1.3`, never `1.4`. Values are range-checked after truncation:
`100.09` is accepted as `100.0`, while `100.10` truncates to `100.1` and is
rejected because the maximum is `100.0`. Negative and non-finite values remain
invalid.

For a preparation tip with slot `s`, and `E` slots per epoch, the wallet
calculates the slot horizon as:

```text
horizon_slots = ceil(epoch_headroom_tenths * E / 10)
valid_until_slot = s + horizon_slots
```

The upward rounding is intentional: a request never receives less elapsed time
than requested. The resulting slot is authoritative; an epoch value in the
quote is only metadata.

## Storage projection

Storage-market updates occur at epoch boundaries. The wallet counts the actual
boundary slots strictly after the preparation slot and at or before
`valid_until_slot`, then applies the protocol's maximum upward transition once
per boundary. Each transition is independently rounded upward:

```text
next_storage_price = ceil(current_storage_price * 9 / 8)
```

The rounding must not be moved outside the loop. Starting at price `1`, three
transitions are `1 -> 2 -> 3 -> 4`, rather than one rounded evaluation of a
fractional power. Headroom does not interpolate a price inside an epoch.

For example, with preparation slot `57` and `2,000` slots per epoch:

```text
headroom 0.8: valid slot 1,657; boundaries 0; storage 1 -> 1
headroom 1.3: valid slot 2,657; boundary 2,000; storage 1 -> 2
headroom 3.0: valid slot 6,057; boundaries 2,000, 4,000, 6,000;
              storage 1 -> 2 -> 3 -> 4
```

If preparation is at slot `1,900`, `0.8` epochs reaches slot `3,500` and
crosses the `2,000` boundary, so one transition is applied.

This storage component is a deterministic protocol ceiling for the covered
epoch transitions.

## Execution projection

Execution-market state updates after produced blocks. Empty slots and epoch
boundaries do not update it. The wallet reads both the current execution base
fee and the current execution EMA, because an EMA above target represents
congestion momentum that remains even if future blocks immediately return to
target usage.

The estimate converts slot time into expected produced blocks using the active
consensus slot-activation configuration. It does not hardcode a blocks-per-
epoch value. The exact estimate is:

```text
average_slots_per_block =
    average_slots_for_blocks(1, slot_activation_coeff)

expected_execution_blocks =
    ceil(horizon_slots / average_slots_per_block)
```

The average is the active consensus integer slot-activation estimate, and the
division is rounded upward with integer ceiling division. For each expected
block the wallet assumes `G_target = 1,596,730` execution gas, then reuses the
ledger's integer arithmetic:

```text
new_ema = floor((assumed_block_gas + 9 * old_ema) / 10)
new_base_fee = ceil(old_base_fee * (7 * G_target + new_ema)
                         / (8 * G_target))
```

`new_ema` uses integer floor division, `new_base_fee` uses integer ceiling
division, and the maximum execution price observed at any simulated block,
including the starting price, is selected. Target-load blocks provide an
explainable mean-reversion baseline: an EMA above target can still raise the
price, while an EMA below target moves toward equilibrium.

Execution projection is an estimate and is not a protocol-guaranteed upper
bound. Sustained above-target demand may exceed it, and actual block
production may differ from the expected rate. Actual funding and transaction
validity still use live chain rules; the horizon does not create artificial
transaction expiry.

## Public API and fee semantics

The public wallet funding endpoint accepts a policy alongside the builder:

```json
{
  "tip": null,
  "tx_builder": "...",
  "change_public_key": "...",
  "funding_public_keys": ["..."],
  "max_tx_fee": 100000,
  "fee_policy": {
    "epoch_headroom": 1.3,
    "priority_fee": 500
  }
}
```

The high-level transfer endpoint accepts the same `fee_policy`. `epoch_headroom`
provisions additional mandatory-fee reserve; `priority_fee` is an independent,
explicit excess intended to incentivise inclusion. Both can be requested
together. The final transaction fee, including projected mandatory fees and the
priority fee, is checked against `max_tx_fee` where supplied.

When no new policy is supplied, existing funding requests retain their legacy
`priority_fee` behaviour and receive no horizon quote. Supplying both a
non-zero legacy top-level `priority_fee` and a `fee_policy` is rejected rather
than risking an accidental double tip. The policy is resolved in the wallet
service, outside `MantleTxBuilder`, so builders remain serializable and chain-
context independent.

Policy funding returns quote metadata identifying the exact preparation tip,
slot, epoch, valid slot, crossed storage boundaries, expected execution blocks,
live and projected prices, starting EMA, estimation model, live/projected
mandatory fees, explicit priority fee, and total projected fee. This metadata
is diagnostic; the on-chain transaction contains no separate reserve bucket.
