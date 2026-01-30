---
title: "Release Checklist for [X.Y.Z]"
labels: "release"
---

Progress on the checklist must be provided as comments to the issue.

---

## Branch Setup
- [ ] Verify the HEAD of `master` has green CI ✅
- [ ] Tag commit with `X.Y.Z` and push the tag

## GitHub Release
- [ ] Manually trigger the bundling workflow from the `X.Y.Z` tag on GitHub
- [ ] Wait for the bundling workflow to complete and generate a draft GitHub pre-release
- [ ] Address checklist of the generated GitHub release
- [ ] Publish release

  ## Post-Release
- [ ] Update the release checklist template (this file) or the GitHub release template with anything that was missing or that was fixed during the release process

---