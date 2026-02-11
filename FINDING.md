# Finding: Which PR Caused Test Failures

## Summary

This document investigates TWO separate test failures on master:
1. ~~Cucumber test: "Orphan staggered fork start 2"~~ (Initial investigation - not the issue)
2. **End-to-end integration test: `test_ibd_behind_nodes`** (Current focus)

---

## Investigation 2: test_ibd_behind_nodes Failure

### Question
Find the merged to master PR where the end-to-end integration test `logos-blockchain-tests::test_cryptarchia_bootstrap test_ibd_behind_nodes` first started to fail.

### Answer

**PR #2115: "fix(chain-leader): do not propose blocks while chain is in Bootstrapping mode"**

### Details
- **Commit SHA**: `271a97ebd03f21a13e9ca72ef8411fd478960296`
- **Merged**: February 5, 2026 at 14:21:15 UTC
- **Author**: youngjoon-lee (@youngjoon-lee)
- **GitHub PR**: https://github.com/logos-blockchain/logos-blockchain/pull/2115
- **First failing workflow run**: 21715094606 (or 21863167499/21868104219 for confirmed failures)

### Timeline

1. **Last Successful Run** (Feb 4, 2026 14:58:15 UTC)
   - Commit: `e3d7b9fa1b3b52ed63715dc0fcbb63bf62d2ab81`
   - PR #2091: "test: add some cryptarchia cucumber tests"
   - Workflow run: 21676373889
   - Status: `test_ibd_behind_nodes` test **PASSED**

2. **First Failing Run** (Feb 5, 2026 14:21:19 UTC or Feb 10 confirmed)
   - Commit: `271a97ebd03f21a13e9ca72ef8411fd478960296`  
   - PR #2115: "fix(chain-leader): do not propose blocks while chain is in Bootstrapping mode"
   - Workflow runs: 21715094606, 21863167499 (Feb 10, 11:32 UTC), 21868104219 (Feb 10, 14:06 UTC)
   - Status: `test_ibd_behind_nodes` test **FAILED** with timeout

### Why This PR Broke test_ibd_behind_nodes

The test failure has the same root cause as the cucumber test failure:

**The Problem:**
PR #2115 modified the chain leader to wait until the chain enters "Online mode" (after IBD + Prolonged Bootstrap Period) before starting block proposals. Previously, blocks could be proposed immediately after IBD completed, even during the Bootstrapping phase.

**Impact on test_ibd_behind_nodes:**
The test `test_ibd_behind_nodes` specifically tests Initial Block Download (IBD) for nodes that join late. The test:
1. Starts 2 initial validators
2. Waits for them to reach Online mode and height 10
3. Starts a third "behind" node with IBD peers configured
4. Expects the behind node to catch up via IBD and switch to Online mode within 10 seconds

**The failure:**
```
thread 'test_ibd_behind_nodes' panicked at tests/src/common/sync.rs:35:9:
Timeout (280s) waiting for validators to reach mode Online and height 10
```

The test is timing out at step 2 - waiting for the initial validators to reach Online mode and height 10. This is because PR #2115 prevents block proposals during Bootstrapping mode, so the validators can't reach height 10 until they exit Bootstrapping, which takes much longer than the test's timeout allows.

### Relationship to Cucumber Test Failure

Both failures stem from the same PR #2115:
- **Cucumber test "Orphan staggered fork start 2"**: Failed because it expected orphan blocks during bootstrapping
- **End-to-end test "test_ibd_behind_nodes"**: Failed because initial validators couldn't reach height 10 before timing out

### Verification

The test file `tests/src/tests/cryptarchia/bootstrap.rs` existed and contained `test_ibd_behind_nodes` in commit e3d7b9f (last successful run), confirming the test was passing before PR #2115.

### Details
- **Commit SHA**: `271a97ebd03f21a13e9ca72ef8411fd478960296`
- **Merged**: February 5, 2026 at 14:21:15 UTC
- **Author**: youngjoon-lee (@youngjoon-lee)
- **GitHub PR**: https://github.com/logos-blockchain/logos-blockchain/pull/2115
- **First failing workflow run**: 21715094606

### Timeline

1. **Last Successful Run** (Feb 4, 2026 14:58:15 UTC)
   - Commit: `e3d7b9fa1b3b52ed63715dc0fcbb63bf62d2ab81`
   - PR #2091: "test: add some cryptarchia cucumber tests"
   - Workflow run: 21676373889

2. **Cancelled Runs** (Feb 5, 2026 morning)
   - Several PRs merged but their workflow runs were cancelled
   - PRs #2110, #2105, #2124, #2123, #2102, #2112, #2127, #2126

3. **First Failing Run** (Feb 5, 2026 14:21:19 UTC)
   - Commit: `271a97ebd03f21a13e9ca72ef8411fd478960296`  
   - PR #2115: "fix(chain-leader): do not propose blocks while chain is in Bootstrapping mode"
   - Workflow run: 21715094606

### Why This PR Caused the Failure

The PR modified the chain leader behavior to prevent block proposals during the Bootstrapping mode. Specifically, it added a wait condition where the chain leader service now waits until the chain switches to "Online mode" (after IBD + Prolonged Bootstrap Period) before starting block proposals.

**Key changes from PR #2115:**
- Added `wait_until_chain_becomes_online()` API to chain service
- Chain leader now waits for the chain to exit Bootstrapping mode before proposing blocks
- Previously, blocks could be proposed immediately after IBD completed, even during Bootstrapping

**Impact on tests:**
The "Orphan staggered fork start 2" cucumber test scenario likely tests:
- Orphaned block handling during early chain operation
- Fork resolution during chain startup/bootstrapping
- Timing-sensitive block proposal behavior

By delaying block proposals until after the chain is fully online (post-Bootstrapping), this PR fundamentally changed the timing of when blocks are created in test scenarios. The test likely expected blocks to be proposed during the Bootstrapping phase, and this behavioral change broke that assumption.

### Methodology

This finding was determined by:
1. Analyzing GitHub Actions workflow runs for the "Cucumber and end-to-end integration tests" workflow (ID: 224970967)
2. Filtering for runs on the master branch with event type "push"
3. Identifying the last successful run before failures began
4. Examining all PRs merged between the last success and first failure
5. Identifying the first non-cancelled failing run after the successful one

See `/tmp/investigation_report.md` for the complete analysis.
