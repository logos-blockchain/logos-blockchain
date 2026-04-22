---
name: Release Checklist
about: Checklist for releasing a new version
title: Release Checklist for X.Y.Z
labels: release
---

# IMPORTANT

**READ THIS BEFORE STARTING WITH THE RELEASE**

* If any changes other than release-specific ones are needed, e.g. a bugfix or some ceremony-related fix that is useful also for future releases, they should be merged with a PR against `master` and not pushed to the release branch. Then, there are two possible strategies:
    * the release continues with a new release candidate, in which case the fix is cherry-picked from `master` into the release branch
    * the release is aborted in favor of a new version forked from `master`: in this case the release branch is merged into master to update any devnet-related changes, before starting the process for the new release with the next release version `X.Y.(Z+1)`
* Progress on the checklist must be provided as comments to the issue.

---

# Branch Setup (done once per full release cycle - skipped for new release candidates)

- [ ] Branch out from the latest `master` commit with a release branch, e.g., `release/0.1.3`
- [ ] Apply and push any changes to the devnet deployment settings. If a ceremony will be run, stuff like genesis block can be ignored since it will be overridden as the outcome of the ceremony

# Devnet (release candidates)

## Devnet ceremony (optional, only whenever a devnet ceremony is required)

- [ ] Manually trigger the [ceremony tools Docker build workflow][build-logos-tools-docker-workflow] from the `HEAD` of the release branch (with the latest changes) specifying the `devnet` image tag.
- [ ] Post the link to the workflow run to this issue for easier review
- [ ] Wait for the workflow run to complete
- [ ] Verify the right image with the right tag was pushed to the [GitHub container registry][logos-tools-image-container-registry]
- [ ] Checkout and hard reset the `devnet` branch to point to the latest commit on the current release branch
- [ ] Create a new symlink `compose.static.yml` -> `compose.setup.yml` with `ln -s -f compose.setup.yml compose.static.yml`
- [ ] Push to `devnet` branch to trigger a new deployment
- [ ] Wait around 1 minute for deployment to be updated with the new changes and for the ceremony to happen. Until ready, you should see a `502` error while the containers restart when visiting [https://devnet.blockchain.logos.co/web/cfgsync/deployment-settings](https://devnet.blockchain.logos.co/web/cfgsync/deployment-settings
- [ ] Download the new deployment configuration from the link above
- [ ] Verify that the `time.chain_start_time` value in the deployment file indicates the right start time, which should be within the last few minutes
- [ ] Copy-paste or attach the content of the deployment file to this issue for easier review
- [ ] Override the existing devnet deployment settings with the generated ones on the release branch
- [ ] Verify `git` shows a diff for the deployment file, otherwise it means something went wrong when downloading the new one from the deployment settings endpoint

## Release candidate publication

- [ ] Bump the Cargo workspace version to match the new release version `X.Y.Z-rc.N`
- [ ] Bump the version value for the C bindings (`logos-blockchain-c`) in the root `flake.nix` file to match the new release version `X.Y.Z-rc.N`
- [ ] Verify the HEAD of the release branch has green CI ✅
- [ ] Tag the commit with `X.Y.Z-rc.N` and push the tag
- [ ] Manually trigger the [bundling workflow][release-bundling-workflow] from the `X.Y.Z-rc.N` tag on GitHub
- [ ] Post the link to the workflow run to this issue for easier review
- [ ] Wait for the bundling workflow to complete and generate a draft GitHub pre-release. While the release is in progress, follow the steps in the [Devnet deployment][devnet-deployment-section] section below.
- [ ] Address checklist of the generated GitHub release
- [ ] Publish release
- [ ] Post the link to the published release to this issue for easier review

## Devnet deployment

- [ ] Checkout `devnet` branch again and change the `compose.static.yml` symlink to now point to `compose.run.yml` with `ln -s -f compose.run.yml compose.static.yml`
- [ ] Commit and push the changes to trigger environment re-deployment. Environment is now live.
- [ ] Wait around 1 minute for deployment to be updated
- [ ] If needed, at any time you can download fleet nodes' configs and logs from [https://devnet.blockchain.logos.co/internal/node-data/](https://devnet.blockchain.logos.co/internal/node-data/)
- [ ] Go back to the [GitHub Release][github-release-candidate-section] section and finalize the release candidate

# Testnet (releases)

## Testnet ceremony (optional, only whenever a testnet ceremony is required)

- [ ] Manually trigger the [ceremony tools Docker build workflow][build-logos-tools-docker-workflow] from the `HEAD` of the release branch (with the latest changes) specifying the `testnet` image tag.
- [ ] Post the link to the workflow run to this issue for easier review
- [ ] Wait for the workflow run to complete
- [ ] Verify the right image with the right tag was pushed to the [GitHub container registry][logos-tools-image-container-registry]
- [ ] Checkout and hard reset the `testnet` branch to point to the latest commit on the current release branch
- [ ] Create a new symlink `compose.static.yml` -> `compose.setup.yml` with `ln -s -f compose.setup.yml compose.static.yml`
- [ ] Push to `testnet` branch to trigger a new deployment
- [ ] Wait around 1 minute for deployment to be updated with the new changes and for the ceremony to happen. Until ready, you should see a `502` error while the containers restart when visiting [https://testnet.blockchain.logos.co/web/cfgsync/deployment-settings](https://testnet.blockchain.logos.co/web/cfgsync/deployment-settings
- [ ] Download the new deployment configuration from the link above
- [ ] Verify that the `time.chain_start_time` value in the deployment file indicates the right start time, which should be within the last few minutes
- [ ] Copy-paste or attach the content of the deployment file to this issue for easier review
- [ ] Override the existing testnet deployment settings with the generated ones on the release branch
- [ ] Verify `git` shows a diff for the deployment file, otherwise it means something went wrong when downloading the new one from the deployment settings endpoint

## Release publication

- [ ] Bump the Cargo workspace version to match the new release version `X.Y.Z`
- [ ] Bump the version value for the C bindings (`logos-blockchain-c`) in the root `flake.nix` file to match the new release version `X.Y.Z`
- [ ] Verify the HEAD of the release branch has green CI ✅
- [ ] Tag the commit with `X.Y.Z` and push the tag
- [ ] Manually trigger the [bundling workflow][release-bundling-workflow] from the `X.Y.Z` tag on GitHub
- [ ] Post the link to the workflow run to this issue for easier review
- [ ] Wait for the bundling workflow to complete and generate a draft GitHub release. While the release is in progress, follow the steps in the [Testnet deployment][testnet-deployment-section] section below.
- [ ] Address checklist of the generated GitHub release
- [ ] Publish release
- [ ] Post the link to the published release to this issue for easier review

## Testnet deployment

- [ ] Checkout `testnet` branch again and change the `compose.static.yml` symlink to now point to `compose.run.yml` with `ln -s -f compose.run.yml compose.static.yml`
- [ ] Commit and push the changes to trigger environment re-deployment. Environment is now live.
- [ ] Wait around 1 minute for deployment to be updated
- [ ] If needed, at any time you can download fleet nodes' configs and logs from [https://testnet.blockchain.logos.co/internal/node-data/](https://testnet.blockchain.logos.co/internal/node-data/)
- [ ] Go back to the [GitHub Release][github-release-section] section and finalize the release
- [ ] Merge the release branch into `master`. Make sure the diff between the two (minus any commits that landed on `master` in the meanwhile) show only release-relevant changes. I.e., make sure no unrelated changes, e.g., bug-fixes have landed on the release branch instead of landing on `master`.

# Post-Release

- [ ] Update the release checklist template (this file) or the GitHub release template with anything that was missing or that was fixed during the release process

---

[logos-tools-image-container-registry]: https://github.com/logos-blockchain/logos-blockchain/pkgs/container/logos-blockchain
[build-logos-tools-docker-workflow]: https://github.com/logos-blockchain/logos-blockchain/actions/workflows/build-logos-tools.yml 
[release-bundling-workflow]: https://github.com/logos-blockchain/logos-blockchain/actions/workflows/prepare-release.yml
[devnet-deployment-section]: #devnet-deployment
[testnet-deployment-section]: #testnet-deployment
[github-release-candidate-section]: #release-candidate-publication
[github-release-section]: #release-publication
