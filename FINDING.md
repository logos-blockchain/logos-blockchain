# Investigation Report: test_ibd_behind_nodes Failure

## Correction

**My previous conclusion was incorrect.** PR #2115 did NOT break the `test_ibd_behind_nodes` test.

## Question
Find the merged PR where the end-to-end integration test `logos-blockchain-tests::test_cryptarchia_bootstrap test_ibd_behind_nodes` started to fail.

## Answer

**The combination of PR #2158 and PR #2159 broke the test.**

Most likely culprit: **PR #2159: "chore: move chain start time to deployment config"** when merged on top of PR #2158.

## Details

### PR #2158: "fix: Blend panic with empty membership"
- **Merged**: February 10, 2026 at 11:32:12 UTC
- **Commit SHA**: `41d5d6b30f5906e67a079b0c142a964da9fb5d71`
- **Author**: @ntn-x2
- **Changes**: Made ZK info optional when Blend membership is empty

### PR #2159: "chore: move chain start time to deployment config"  
- **Merged**: February 10, 2026 at 14:06:19 UTC
- **Commit SHA**: `feac5ab97ef6dfcebcf6536363a5f330cb79b5e0`
- **Author**: @ntn-x2
- **GitHub PR**: https://github.com/logos-blockchain/logos-blockchain/pull/2159
- **Changes**: Moved chain start time from user config to deployment config

## Timeline

1. **PR #2158 merged to master** (Feb 10, 11:32:12 UTC)
   - Commit: `41d5d6b30f5906e67a079b0c142a964da9fb5d71`
   
2. **PR #2159 tested on its branch** (Feb 10, 11:43:15 UTC)
   - Workflow run: [21863493082](https://github.com/logos-blockchain/logos-blockchain/actions/runs/21863493082/job/63098488328)
   - Status: `test_ibd_behind_nodes` **PASSED** (114.600s)
   - **Important**: This branch did NOT include PR #2158's changes yet

3. **PR #2159 merged to master** (Feb 10, 14:06:19 UTC)
   - Now includes both PR #2158 and PR #2159 changes
   - Commit: `feac5ab97ef6dfcebcf6536363a5f330cb79b5e0`

4. **Master workflow run after PR #2159** (Feb 10, 14:06:23 UTC)
   - Workflow run: [21868104219](https://github.com/logos-blockchain/logos-blockchain/actions/runs/21868104219)
   - Status: `test_ibd_behind_nodes` **FAILED** (timeout 280s)

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

## Why The Combination Broke the Test

PR #2159 moved the chain start time from user config to deployment config. When tested on its own branch (without PR #2158), it passed. However, when merged to master on top of PR #2158's changes to Blend membership handling (making ZK info optional when membership is empty), the combination introduced a configuration issue.

Possible causes:
1. **Deployment config structure change**: PR #2159 changes how configuration is structured, potentially breaking test setup
2. **Interaction with Blend changes**: PR #2158's empty membership handling may interact badly with the new deployment config
3. **Missing chain start time**: Tests may not be properly setting chain start time in the new deployment config structure

## Key Evidence

The critical evidence is that **PR #2159 passed tests on its branch** (before merging), but **failed on master** (after merging with PR #2158). This clearly indicates the combination of both PRs caused the failure, not either PR individually.

## References

- **Passing run (PR #2159 branch)**: https://github.com/logos-blockchain/logos-blockchain/actions/runs/21863493082/job/63098488328
- **Failing run (master after merge)**: https://github.com/logos-blockchain/logos-blockchain/actions/runs/21868104219
- **PR #2158**: https://github.com/logos-blockchain/logos-blockchain/pull/2158
- **PR #2159**: https://github.com/logos-blockchain/logos-blockchain/pull/2159
- **Test file**: `tests/src/tests/cryptarchia/bootstrap.rs`

## Notes on Investigation Methodology

This investigation was corrected after initially concluding PR #2115 was the cause. The user provided evidence showing PR #2159's branch test run where `test_ibd_behind_nodes` passed, which contradicted the initial conclusion and led to re-analysis of the timeline.
