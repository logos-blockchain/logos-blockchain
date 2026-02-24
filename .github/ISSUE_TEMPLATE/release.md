---
name: Release Checklist
about: Checklist for releasing a new version
title: Release Checklist for [X.Y.Z]
labels: release
---

Progress on the checklist must be provided as comments to the issue.

---

## Ceremony (optional, only whenever a ceremony is required)

- [ ] <span style="color:red">**!! IMPORTANT: Make sure that *ALL* the deployment settings are committed to `master` before starting the ceremony. The only value that is *expected* to be out-of-sync is the cryptarchia genesis state, which will be a result of running the ceremony**</span>.
- [ ] Checkout `master` and tag commit with `pre-X.Y.Z` and push the tag
- [ ] Manually trigger the [testnet Docker workflow][testnet-docker-workflow] using the `pre-X.Y.Z` tag and using the `devnet` image tag
- [ ] Wait for the workflow run to complete
- [ ] Checkout and force reset the `testnet` branch to point to the tagged commit
- [ ] Create a new symlink `compose.static.yml` -> `compose.devnet.setup.yml`
- [ ] Push to `testnet` branch to trigger a new deployment
- [ ] Wait around 1 minute for deployment to be updated with the new changes and for the ceremony to happen
- [ ] Download the new deployment configuration from `https://devnet.blockchain.logos.co/node/0/cfgsync/deployment-settings`

## Deployment Settings Update

- [ ] Checkout `master` and push a new commit on top of `pre-X.Y.Z` with the updated devnet settings
- [ ] Verify the HEAD of `master` has green CI ✅
- [ ] Tag the commit with `X.Y.Z` and push the tag

## GitHub Release

- [ ] Manually trigger the [bundling workflow][bundling-workflow] from the `X.Y.Z` tag on GitHub
- [ ] Wait for the bundling workflow to complete and generate a draft GitHub pre-release
- [ ] Address checklist of the generated GitHub release
- [ ] Publish release

## Devnet deployment

- [ ] Wait for the [Docker image workflow][docker-build-workflow] to complete
- [ ] Checkout `testnet` branch and change the `compose.static.yml` symlink to now point to `compose.devnet.run.yml`
- [ ] Commit and push the changes to trigger environment re-deployment. Environment is now live.

## Post-Release

- [ ] Update the release checklist template (this file) or the GitHub release template with anything that was missing or that was fixed during the release process

---

[testnet-docker-workflow]: https://github.com/logos-blockchain/logos-blockchain/actions/workflows/publish-testnet-image.yml 
[bundling-workflow]: https://github.com/logos-blockchain/logos-blockchain/actions/workflows/prepare-release.yml
[docker-build-workflow]: https://github.com/logos-blockchain/logos-blockchain/actions/workflows/publish-node-image.yml