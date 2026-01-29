---
title: "Release Checklist for [X.Y.Z]"
labels: "release"
---

Progress on the checklist must be provided as comments to the issue.

## Versioning Reference

| Change Type | Version Bump | Example |
|-------------|--------------|---------|
| Circuits change or any other hard fork changes | Major | `1.0.0` → `2.0.0` |
| New non-consensus feature (soft fork) | Minor | `1.0.0` → `1.1.0` |
| Bug fix | Patch | `1.0.0` → `1.0.1` |

---

## Branch Setup
- [ ] Choose a release captain and a release co-captain
- [ ] Verify the commit on `master` we are branching off for the release has green CI ✅
- [ ] Branch off of `master` into branch `release/X.Y.Z`
- [ ] Update and commit workspace version in root `Cargo.toml` to `X.Y.Z`
- [ ] Tag initial release candidate with `X.Y.Z-rc.1`
- [ ] Push branch and tag

## Internal Testing (RC Phase)
- [ ] Verify that CI jobs related to the new tag are green ✅
- [ ] Wait for the bundling workflow to complete and download build artifacts
- [ ] Download the appropriate version of the Linux and MacOS circuits from the circuits repo
- [ ] Run all required tests for `X.Y.Z-rc.N`
- [ ] If issues found
  - [ ] If the bugfix is compatible with `master`
    - [ ] Fix on `master`
    - [ ] Cherry-pick fix to `release/X.Y.Z`
  - [ ] If the bugfix is not compatible with `master`
    - [ ] Fix directly on `release/X.Y.Z`
  - [ ] Tag new RC with `X.Y.Z-rc.(N+1)`
  - [ ] Push branch and tag

Repeat process until no more issues are found, documenting under the issue.

## Promotion to Public Release Candidate
- [ ] Prepare changelog
  - [ ] If new major
    - [ ] Major changes since last major release
  - [ ] If new minor
    - [ ] Minor changes since last minor release
  - [ ] If new patch
    - [ ] Patch changes since previous patch of same minor
  - [ ] Instructions on how to set up the circuits
- [ ] Create GitHub Release (mark as **pre-release**)
  - [ ] Point to same commit as tested RC tag
  - [ ] Attach platform bundles
  - [ ] Include changelog
- [ ] If issues found, fix and publish a new tag `X.Y.Z-rc.(N+1)`, and repeat the RC Phase process.

## Final Release
- [ ] Tag final release (same commit as last RC) with `X.Y.Z`
- [ ] Draft GitHub Release (not pre-release)
  - [ ] Attach final platform bundles
  - [ ] Include changelog
  - [ ] Review draft
- [ ] Publish GitHub Release

## Post-Release
- [ ] Delete release branch (tag preserved)

---