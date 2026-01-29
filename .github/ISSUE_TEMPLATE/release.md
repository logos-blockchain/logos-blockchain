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
- [ ] Prepare (or auto-generate) changelog with commit history since last release
- [ ] Manually trigger the bundling workflow providing the `X.Y.Z` tag name
- [ ] Wait for the bundling workflow to complete and generate a draft GitHub pre-release
- [ ] Download the appropriate version of the Linux and MacOS circuits from the circuits repo
- [ ] Update GitHub Release
  - [ ] Include changelog
  - [ ] Attach the Linux and MacOS circuits

## Post-Release
- [ ] Update the release checklist with anything that was missing or that was fixed

---