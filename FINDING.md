# Investigation Report: test_ibd_behind_nodes Failure

## Final Answer

**PR #2149: "feat: promote genesis state to deployment config"** broke the `test_ibd_behind_nodes` end-to-end integration test.

- **Commit SHA**: `1e370ca1084c15379437eaaaa6df4de0641b4cc9`
- **Merged**: February 9, 2026 at 14:14:25 UTC
- **Author**: @ntn-x2
- **GitHub PR**: https://github.com/logos-blockchain/logos-blockchain/pull/2149

## Precise Evidence from End-to-End Integration Test Logs

### PR #2148: test_ibd_behind_nodes PASSED ✓

**Workflow run**: [21820224914](https://github.com/logos-blockchain/logos-blockchain/actions/runs/21820224914/job/62951403257)

From the "End-to-end integration tests" step output:
```
Nextest run ID b3f3142a-7b25-4976-84c7-0ed99cddc8b5 with nextest profile: default
   Starting 6 tests across 8 binaries (1 test skipped)
 TRY 1 SLOW [> 60.000s] logos-blockchain-tests::test_cryptarchia_bootstrap test_ibd_behind_nodes
 TRY 1 SLOW [>120.000s] logos-blockchain-tests::test_cryptarchia_bootstrap test_ibd_behind_nodes
       PASS [ 154.680s] logos-blockchain-tests::test_cryptarchia_bootstrap test_ibd_behind_nodes
```

**Result**: Test **PASSED** in 154.680 seconds ✓

### PR #2149: test_ibd_behind_nodes FAILED ✗

**Workflow run**: [21824961194](https://github.com/logos-blockchain/logos-blockchain/actions/runs/21824961194/job/62967441910)

From the "End-to-end integration tests" step output:
```
    Summary [1936.188s] 6 tests run: 5 passed (3 slow, 1 flaky), 1 failed, 1 skipped
  FLAKY 2/3 [ 577.146s] logos-blockchain-tests::test_cryptarchia_happy_path two_nodes_happy
 TRY 3 FAIL [ 285.272s] logos-blockchain-tests::test_cryptarchia_bootstrap test_ibd_behind_nodes
error: test run failed
```

**Result**: Test **FAILED** after 3 retries (285.272 seconds on try 3) ✗

## Test Failure Details

### Error Message
```
test test_ibd_behind_nodes ... FAILED

thread 'test_ibd_behind_nodes' panicked at tests/src/common/sync.rs:35:9:
Timeout (280s) waiting for validators to reach mode Online and height 10
```

### What the Test Does
The test validates Initial Block Download (IBD) for nodes joining late:
1. Starts 2 initial validators
2. **Waits for them to reach Online mode and height 10** ← Fails here
3. Starts a third "behind" node with IBD peers configured
4. Expects the behind node to catch up via IBD

## Why PR #2149 Broke the Test

PR #2149 promoted the genesis state to the deployment configuration. This involved:
- Moving genesis state configuration from user config to deployment config
- Restructuring how genesis state is accessed and initialized
- Changes to deployment configuration structure

The test failure indicates that validators cannot reach height 10 within the 280-second timeout, suggesting that the genesis state changes affected:
1. **Genesis block creation/initialization**: The genesis state may not be properly initialized in test scenarios
2. **Block production timing**: The restructured configuration may have delayed or prevented block production
3. **Test configuration setup**: Tests may not be properly setting up the genesis state in the new deployment config structure

## Timeline

1. **Feb 9, 09:51 UTC**: PR #2148 tested
   - Branch: ho_configs
   - Title: "chore: fix ntp config value and windows compile"
   - Result: `test_ibd_behind_nodes` **PASSED** (154.680s)
   
2. **Feb 9, 12:24 UTC**: PR #2149 tested
   - Branch: aa/genesis-state
   - Title: "feat: promote genesis state to deployment config"
   - Result: `test_ibd_behind_nodes` **FAILED** (285.272s after 3 retries)
   
3. **Feb 9, 14:14 UTC**: PR #2149 merged to master
   - This introduced the breaking change

## References

- **PR #2148 (test passing)**: 
  - Workflow: https://github.com/logos-blockchain/logos-blockchain/actions/runs/21820224914
  - Linux job: https://github.com/logos-blockchain/logos-blockchain/actions/runs/21820224914/job/62951403257
  
- **PR #2149 (test failing)**:
  - Workflow: https://github.com/logos-blockchain/logos-blockchain/actions/runs/21824961194
  - Linux job: https://github.com/logos-blockchain/logos-blockchain/actions/runs/21824961194/job/62967441910
  - macOS job: https://github.com/logos-blockchain/logos-blockchain/actions/runs/21824961194/job/62967441900
  
- **PR #2149 on GitHub**: https://github.com/logos-blockchain/logos-blockchain/pull/2149
- **Test file**: `tests/src/tests/cryptarchia/bootstrap.rs`
- **Sync helper**: `tests/src/common/sync.rs` (line 35 where timeout occurs)

## Command Used to Run Tests

From the workflow:
```bash
cargo nextest run --locked --jobs 1 --retries 2 -p logos-blockchain-tests --no-fail-fast
```

## Investigation History

This investigation went through multiple corrections before reaching the accurate conclusion:
1. ~~PR #2115~~ ← incorrect (no evidence)
2. ~~PR #2158 + PR #2159~~ ← incorrect (misread timeline)  
3. **PR #2149** ← **CORRECT** (specific test log evidence)

The user provided precise test output showing:
- PR #2148: `test_ibd_behind_nodes` PASSED in 154.680s
- PR #2149: `test_ibd_behind_nodes` FAILED after 285.272s (timeout)

This conclusively identifies PR #2149 as the root cause.
