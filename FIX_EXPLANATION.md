# Fix for test_ibd_behind_nodes Failure

## Problem

After PR #2149 ("feat: promote genesis state to deployment config"), the test `test_ibd_behind_nodes` started failing with the error:

```
Timeout (280s) waiting for validators to reach mode Online and height 10
```

## Root Cause

PR #2149 moved the genesis state from user configuration to deployment configuration and renamed the function `default_e2e_deployment_settings()` to `e2e_deployment_settings_with_genesis_tx(genesis_tx)`.

The function `e2e_deployment_settings_with_genesis_tx` includes this line:
```rust
chain_start_time: OffsetDateTime::now_utc(),
```

In the failing tests, this function was being called multiple times in a loop - once for each validator:

```rust
for config in general_configs.iter().take(n_initial_validators) {
    let config = create_validator_config(
        config.clone(),
        e2e_deployment_settings_with_genesis_tx(genesis_tx.clone()),  // ← Called in loop!
    );
    initial_validators.push(Validator::spawn(config).await.unwrap());
}
```

Since each call to `OffsetDateTime::now_utc()` returns a slightly different timestamp (milliseconds apart), each validator ended up with a different `chain_start_time`.

When validators have different chain start times, they disagree on when blocks should be produced:
- Some validators think the chain has already started and blocks should be produced
- Others think the chain hasn't started yet

This prevented the validators from reaching consensus and producing blocks, causing the test to timeout.

## Solution

Create the deployment settings ONCE before the loop, then clone it for each validator:

```rust
// Create deployment settings once to ensure all validators have the same chain start time
let deployment_settings = e2e_deployment_settings_with_genesis_tx(genesis_tx.clone());

for config in general_configs.iter().take(n_initial_validators) {
    let config = create_validator_config(
        config.clone(),
        deployment_settings.clone(),  // ← Clone the same settings
    );
    initial_validators.push(Validator::spawn(config).await.unwrap());
}
```

This ensures all validators share the exact same deployment configuration, including the critical `chain_start_time`.

## Files Modified

1. **tests/src/tests/cryptarchia/bootstrap.rs**
   - Fixed initial validators creation loop
   - Fixed behind node creation

2. **tests/src/tests/cryptarchia/orphan.rs**
   - Fixed initial validators creation loop  
   - Fixed behind node creation

3. **tests/src/tests/cryptarchia/immutable_blocks.rs**
   - Already correct (no changes needed)

## Testing

The fix should be validated by running:

```bash
cargo nextest run --locked --jobs 1 --retries 2 -p logos-blockchain-tests test_ibd_behind_nodes
```

Expected result: Test should pass consistently within the normal time range (previously 154s before the bug was introduced).
