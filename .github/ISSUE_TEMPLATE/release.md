---
name: Release Checklist
about: Checklist for releasing a new version
title: Release Checklist for X.Y.Z
labels: release
---

<!---

Most of the template content is the same or very similar to what is in `release-candidate.md`. So any changes to this file should be reflected there where relevant, and viceversa.

--->

# IMPORTANT

**READ THIS BEFORE STARTING WITH THE RELEASE**

* This checklist should only be used with a release candidate that has been thoroughly tested and can be "promoted" to be a release. No changes other than what is needed to release it are supposed to be committed from the commit of the last release candidate being released
* Progress on the checklist must be provided as comments to the issue.

---

## Branch Setup

- [ ] Edit the name of this issue to use the actual version being released
- [ ] Verify that the `HEAD` of the release branch `release/X.Y.Z` is the same commit that was released in the latest rc
- [ ] Post the link of the latest release candidate GH release and the previous release candidate checklist that we are promoting to a full release
- [ ] Change the testnet deployment settings to use the version number in ALL protocol names, e.g., `/logos-blockchain-testnet-X.Y.Z/mempool/1.0.0`
- [ ] Apply any other changes to the testnet deployment settings and push the changes. If a ceremony will be run, stuff like genesis block can be ignored since it will be overridden as the outcome of the ceremony.

## Testnet ceremony (optional, only whenever a testnet ceremony is required)

- [ ] Manually trigger the [ceremony tools Docker build workflow][build-logos-tools-docker-workflow] from the `HEAD` of the release branch (with the latest changes) specifying the `testnet` image tag.
- [ ] Post the link to the workflow run to this issue for easier review
- [ ] Wait for the workflow run to complete
- [ ] Verify the right image with the right tag was pushed to the [GitHub container registry][logos-tools-image-container-registry]
- [ ] Checkout and hard reset the `testnet` branch to point to the latest commit on the current release branch
- [ ] Create a new symlink `compose.static.yml` -> `compose.setup.yml` with `ln -s -f compose.setup.yml compose.static.yml`
- [ ] Push to `testnet` branch to trigger the ceremony and generate a new genesis state
- [ ] Wait around 1 minute for deployment to be updated with the new changes and for the ceremony to happen. Until ready, you should see a `502` error while the containers restart when visiting [https://testnet.blockchain.logos.co/web/cfgsync/deployment-settings](https://testnet.blockchain.logos.co/web/cfgsync/deployment-settings)
- [ ] Download the new deployment configuration from the link above
- [ ] Copy-paste or attach the content of the deployment file to this issue for easier review
- [ ] Override the existing testnet deployment settings with the generated ones on the release branch
- [ ] Verify `git` shows a diff for the deployment file, specifically in the first operation of the genesis tx which includes the chain start time, otherwise it means something went wrong when downloading the new one from the deployment settings endpoint

## Release publication

- [ ] Bump the Cargo workspace version to match the new release version `X.Y.Z`
- [ ] Bump the version value for the C bindings (`logos-blockchain-c`) in the root `flake.nix` file to match the new release version `X.Y.Z`
- [ ] Verify the HEAD of the release branch has green CI ✅
- [ ] Tag the commit with `X.Y.Z` and push the tag
- [ ] Manually trigger the [bundling workflow][release-bundling-workflow] from the `X.Y.Z` tag on GitHub with the `release` input to prepare the GitHub release draft with the build binaries
- [ ] Post the link to the workflow run to this issue for easier review
- [ ] Wait for the bundling workflow to complete and generate a draft GitHub release.
- [ ] Address checklist of the generated GitHub release in [https://github.com/logos-blockchain/logos-blockchain/releases](https://github.com/logos-blockchain/logos-blockchain/releases)
- [ ] Publish release
- [ ] Post the link to the published release to this issue for easier review
- [ ] Post the link to the Docker image building workflow as appearing in [node-docker-build-workflow][the Actions section]

## Testnet deployment

- [ ] Wait for the new Docker image to be built after the release is published. It must have the `X.Y.Z` tag.
- [ ] Checkout `testnet` branch again and change the `compose.static.yml` symlink to now point to `compose.run.yml` with `ln -s -f compose.run.yml compose.static.yml`
- [ ] Update `.env.testnet` file to contain `NODE_IMAGE_LABEL=X.Y.Z` set to latest version
- [ ] Commit and push the changes to trigger environment re-deployment. Environment is now live.
- [ ] Wait around 1 minute for deployment to be updated
- [ ] If needed, at any time you can download fleet nodes' configs and logs from [https://testnet.blockchain.logos.co/internal/node-data/](https://testnet.blockchain.logos.co/internal/node-data/)
- [ ] Go back to the [GitHub Release][github-release-section] section and finalize the release

## Release branch wind-down

- [ ] Open a PR against `master` to merge the release branch into it. Make sure the diff between the two show only release-relevant changes. I.e., make sure no unrelated changes, e.g., bug-fixes have landed on the release branch instead of landing on `master`.

# Post-Release

- [ ] Update the release checklist template (this file and also `release-candidate.md`) or the GitHub release template with anything that was missing or that was fixed during the release process

---

[logos-tools-image-container-registry]: https://github.com/logos-blockchain/logos-blockchain/pkgs/container/logos-blockchain
[build-logos-tools-docker-workflow]: https://github.com/logos-blockchain/logos-blockchain/actions/workflows/build-logos-tools.yml 
[release-bundling-workflow]: https://github.com/logos-blockchain/logos-blockchain/actions/workflows/prepare-release.yml
[testnet-deployment-section]: #testnet-deployment
[github-release-section]: #release-publication
[node-docker-build-workflow]: https://github.com/logos-blockchain/logos-blockchain/actions/workflows/publish-node-image.yml