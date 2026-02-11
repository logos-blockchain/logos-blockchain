# Finding: Which PR Caused Test Failures

## Summary

This document investigates TWO separate test failures on master:
1. ~~Cucumber test: "Orphan staggered fork start 2"~~ (Initial investigation - not the issue)
2. **End-to-end integration test: `test_ibd_behind_nodes`** (Current focus)

---

# Finding: Which PR Caused Test Failures

## Correction

**My previous conclusion was incorrect.** PR #2115 did NOT break the `test_ibd_behind_nodes` test. The test was actually passing in workflow runs around that time.

## Investigation: test_ibd_behind_nodes Failure

### Question
Find the merged to master PR where the end-to-end integration test `logos-blockchain-tests::test_cryptarchia_bootstrap test_ibd_behind_nodes` first started to fail.

### Answer

**The combination of PR #2158 and PR #2159 broke the test.**

Most likely culprit: **PR #2159: "chore: move chain start time to deployment config"** when merged on top of PR #2158.

### Details

**PR #2158**: "fix: Blend panic with empty membership"
- **Merged**: February 10, 2026 at 11:32:12 UTC
- **Commit SHA**: `41d5d6b30f5906e67a079b0c142a964da9fb5d71`
- **Author**: @ntn-x2

**PR #2159**: "chore: move chain start time to deployment config"  
- **Merged**: February 10, 2026 at 14:06:19 UTC
- **Commit SHA**: `feac5ab97ef6dfcebcf6536363a5f330cb79b5e0`
- **Author**: @ntn-x2
- **GitHub PR**: https://github.com/logos-blockchain/logos-blockchain/pull/2159

### Timeline

1. **PR #2158 merged to master** (Feb 10, 11:32:12 UTC)
   - Commit: `41d5d6b30f5906e67a079b0c142a964da9fb5d71`
   
2. **PR #2159 tested on its branch** (Feb 10, 11:43:15 UTC)
   - Workflow run: 21863493082
   - Status: `test_ibd_behind_nodes` **PASSED** (114.600s)
   - Note: This branch did NOT include PR #2158's changes yet

3. **PR #2159 merged to master** (Feb 10, 14:06:19 UTC)
   - Now includes both PR #2158 and PR #2159 changes
   - Commit: `feac5ab97ef6dfcebcf6536363a5f330cb79b5e0`

4. **Master workflow run after PR #2159** (Feb 10, 14:06:23 UTC)
   - Workflow run: 21868104219
   - Status: `test_ibd_behind_nodes` **FAILED** (timeout 280s)

### Why The Combination Broke the Test

PR #2159 moved the chain start time from user config to deployment config. When combined with PR #2158's changes to Blend membership handling (making ZK info optional when membership is empty), this appears to have introduced a configuration mismatch or timing issue that causes the test to timeout.

The test failure suggests validators can't reach height 10 within the timeout period, possibly due to:
1. Changed deployment configuration structure breaking test setup
2. Interaction between empty Blend membership handling and deployment config
3. Missing or incorrect chain start time configuration in test scenarios

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
