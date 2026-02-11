# Finding: Which PR Caused "Orphan staggered fork start 2" Test to Fail

## Question
Find the merged to master PR before this one where the test first started to fail.

## Answer

**PR #2115: "fix(chain-leader): do not propose blocks while chain is in Bootstrapping mode"**

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

### Why This PR Likely Caused the Failure

The PR modified the chain leader behavior to prevent block proposals during the Bootstrapping mode. The "Orphan staggered fork start 2" cucumber test scenario likely tests:
- Orphaned block handling
- Fork resolution during chain startup
- Timing-sensitive block proposal behavior

By changing when blocks can be proposed, this PR affected the test's expected behavior for handling orphan blocks in a staggered fork scenario during chain bootstrapping.

### Methodology

This finding was determined by:
1. Analyzing GitHub Actions workflow runs for the "Cucumber and end-to-end integration tests" workflow (ID: 224970967)
2. Filtering for runs on the master branch with event type "push"
3. Identifying the last successful run before failures began
4. Examining all PRs merged between the last success and first failure
5. Identifying the first non-cancelled failing run after the successful one

See `/tmp/investigation_report.md` for the complete analysis.
