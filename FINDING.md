# Investigation Report: test_ibd_behind_nodes Failure

## Final Answer

**PR #2149: "feat: promote genesis state to deployment config"** broke the `test_ibd_behind_nodes` end-to-end integration test.

- **Commit SHA**: `1e370ca1084c15379437eaaaa6df4de0641b4cc9`
- **Merged**: February 9, 2026 at 14:14:25 UTC
- **Author**: @ntn-x2
- **GitHub PR**: https://github.com/logos-blockchain/logos-blockchain/pull/2149

## Evidence

### PR #2148: PASSED (macOS)
- **Workflow run**: [21820224914](https://github.com/logos-blockchain/logos-blockchain/actions/runs/21820224914)
- **Created**: Feb 9, 2026 09:51:50 UTC
- **Branch**: ho_configs
- **Title**: "chore: fix ntp config value and windows compile"
- **End-to-end integration tests**:
  - macOS job: **SUCCESS** ✓
  - Linux job: FAILURE (unrelated issue)

### PR #2149: FAILED (both platforms)
- **Workflow run**: [21824961194](https://github.com/logos-blockchain/logos-blockchain/actions/runs/21824961194)
- **Created**: Feb 9, 2026 12:24:17 UTC
- **Branch**: aa/genesis-state
- **Title**: "feat: promote genesis state to deployment config"
- **End-to-end integration tests**:
  - macOS job: **FAILURE** ✗
  - Linux job: **FAILURE** ✗

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
- Moving genesis state configuration from one location to another
- Restructuring how genesis state is accessed and initialized
- Changes to deployment configuration structure

The test failure indicates that validators cannot reach height 10 within the 280-second timeout, suggesting that the genesis state changes affected:
1. **Genesis block creation/initialization**: The genesis state may not be properly initialized in test scenarios
2. **Block production timing**: The restructured configuration may have delayed or prevented block production
3. **Test configuration setup**: Tests may not be properly setting up the genesis state in the new deployment config structure

## Timeline

1. **Feb 9, 09:51 UTC**: PR #2148 tested
   - Result: End-to-end tests **PASSED** on macOS
   
2. **Feb 9, 12:24 UTC**: PR #2149 tested
   - Result: End-to-end tests **FAILED** on both platforms
   
3. **Feb 9, 14:14 UTC**: PR #2149 merged to master
   - This introduced the breaking change

## References

- **PR #2148 (passing)**: https://github.com/logos-blockchain/logos-blockchain/actions/runs/21820224914
  - macOS job (success): https://github.com/logos-blockchain/logos-blockchain/actions/runs/21820224914/job/62951403306
- **PR #2149 (failing)**: https://github.com/logos-blockchain/logos-blockchain/actions/runs/21824961194
  - Linux job: https://github.com/logos-blockchain/logos-blockchain/actions/runs/21824961194/job/62967441910
  - macOS job: https://github.com/logos-blockchain/logos-blockchain/actions/runs/21824961194/job/62967441900
- **PR #2149 on GitHub**: https://github.com/logos-blockchain/logos-blockchain/pull/2149
- **Test file**: `tests/src/tests/cryptarchia/bootstrap.rs`

## Investigation History

This investigation went through multiple corrections:
1. Initially identified PR #2115 as the cause (incorrect)
2. Corrected to combination of PR #2158 + PR #2159 (incorrect)
3. **Final correction**: PR #2149 is the actual cause (correct)

The user provided specific workflow run evidence showing PR #2148 passed and PR #2149 failed, which led to this final accurate conclusion.
