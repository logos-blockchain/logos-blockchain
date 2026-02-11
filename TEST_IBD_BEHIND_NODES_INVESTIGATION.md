# Investigation Result: test_ibd_behind_nodes Failure

## Question
Find the PR where the end-to-end integration test `logos-blockchain-tests::test_cryptarchia_bootstrap test_ibd_behind_nodes` started to fail.

## Answer

**PR #2115: "fix(chain-leader): do not propose blocks while chain is in Bootstrapping mode"**
- **Merged**: February 5, 2026 at 14:21:15 UTC
- **Commit SHA**: `271a97ebd03f21a13e9ca72ef8411fd478960296`
- **Author**: @youngjoon-lee
- **PR Link**: https://github.com/logos-blockchain/logos-blockchain/pull/2115

## Test Failure Details

### Error Message
```
test test_ibd_behind_nodes ... FAILED

thread 'test_ibd_behind_nodes' (351207) panicked at tests/src/common/sync.rs:35:9:
Timeout (280s) waiting for validators to reach mode Online and height 10
```

### What the Test Does
The `test_ibd_behind_nodes` test validates Initial Block Download (IBD) for nodes joining late:
1. Starts 2 initial validators
2. **Waits for them to reach Online mode and height 10** ← This is where it fails
3. Starts a third "behind" node with IBD peers configured  
4. Expects the behind node to catch up via IBD and switch to Online mode

### Why PR #2115 Broke It

**The Change:**
PR #2115 modified the chain leader service to prevent block proposals during the Bootstrapping mode. The chain leader now waits until the chain switches to "Online mode" (after IBD + Prolonged Bootstrap Period) before proposing any blocks.

**The Impact:**
The test fails at step 2 because:
- Initial validators start in Bootstrapping mode
- They cannot propose blocks during Bootstrapping (due to PR #2115)
- Without block proposals, they cannot reach height 10
- The test times out (280 seconds) waiting for height 10
- Test never reaches the IBD-specific logic it was meant to test

### Timeline

| Date | Event | SHA | Status |
|------|-------|-----|--------|
| Feb 4, 2026 14:58 UTC | Last successful run | e3d7b9f (PR #2091) | test_ibd_behind_nodes **PASSED** |
| Feb 5, 2026 07:52 - 14:21 UTC | Several PRs merged, runs cancelled | Various | N/A |
| Feb 5, 2026 14:21 UTC | PR #2115 merged | 271a97e | test_ibd_behind_nodes **FAILED** |
| Feb 10, 2026 11:32 UTC | Confirmed failure | 41d5d6b | test_ibd_behind_nodes **FAILED** |
| Feb 10, 2026 14:06 UTC | Confirmed failure | feac5ab | test_ibd_behind_nodes **FAILED** |

## Related Failures

PR #2115 also broke the cucumber test "Orphan staggered fork start 2" for similar reasons - both tests expected block proposals during the Bootstrapping phase.

## Investigation Methodology

1. Analyzed GitHub Actions workflow runs for "Cucumber and end-to-end integration tests" (workflow ID: 224970967)
2. Identified last successful run: Feb 4, 2026 (run ID: 21676373889)
3. Identified first failing run: Feb 5-10, 2026 (run IDs: 21715094606, 21863167499, 21868104219)
4. Verified test existed in successful commit (e3d7b9f)
5. Examined job logs showing "End-to-end integration tests" step failures
6. Reviewed PR #2115 code changes to understand behavioral change

## References

- Failing workflow run (Feb 10): https://github.com/logos-blockchain/logos-blockchain/actions/runs/21868104219
- PR #2115: https://github.com/logos-blockchain/logos-blockchain/pull/2115
- Test file: `tests/src/tests/cryptarchia/bootstrap.rs`
- Full investigation: See `FINDING.md` in repository
