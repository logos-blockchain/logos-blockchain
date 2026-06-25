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

## Testnet genesis (optional, only whenever a new testnet deployment - with a new genesis - is required)

- [ ] Update the inscription file at `deployment/ceremony/genesis/testnet/inscribe.yaml` by setting the `chain_id` field to `X.Y.Z` and the `genesis_time` field to be approximately 10 mins in the future, following the existing [ISO 8601](https://en.wikipedia.org/wiki/ISO_8601) datetime format. No need to change the `entropy_sources`
- [ ] Commit and push the changes
- [ ] Manually trigger the [ceremony workflow][ceremony-workflow] from the `HEAD` of the release branch specifying the `testnet` image tag and the right version number `X.Y.Z`
- [ ] Post the link to the workflow run to this issue for easier review
- [ ] Wait for the workflow run to complete. The workflow will push a new commit on the release branch overwriting the binary's embedded deployment settings (`nodes/node/binary/src/config/deployment/settings.yaml`) with the testnet settings.
- [ ] Checkout and hard reset the `testnet` branch to point to the latest commit of the current release branch
- [ ] Create a new symlink `compose.static.yml` -> `compose.setup.yml` with `ln -sf compose.setup.yml compose.static.yml`
- [ ] Commit and push to `testnet` branch to trigger the cleanup
- [ ] Wait around 1 minute for the previous deployment to be cleaned. Visit the [Testnet web UI][testnet-web-ui] and make sure it's in setup mode.

## Release preparation

- [ ] Checkout and pull the release branch, it should contain the bot generated commit updating the deployment settings as its `HEAD`
- [ ] Bump the Cargo workspace version to match the new release version `X.Y.Z`
- [ ] Re-generate the workspace `Cargo.lock` file with `cargo update -w`
- [ ] Verify the `Cargo.lock` is now up to date with `cargo update -w --locked`
- [ ] Verify the `HEAD` of the release branch has green CI ✅
- [ ] Commit the changes, and tag them with `X.Y.Z`
- [ ] Push the commit and the tag
- [ ] Manually trigger the [Logos Blockchain tools build workflow][build-logos-tools-docker-workflow] from the `HEAD` of the release branch (with the latest changes) specifying the `testnet` image tag.
- [ ] Post the link to the workflow run to this issue for easier review
- [ ] Without waiting for the workflow to complete, move on to the next section

## Release publication

- [ ] Manually trigger the [bundling workflow][release-bundling-workflow] from the `X.Y.Z` tag on GitHub with the `release` input to prepare the GitHub release draft with the built binaries
- [ ] Post the link to the workflow run to this issue for easier review
- [ ] Wait for the bundling workflow to complete and generate a draft GitHub pre-release.
- [ ] Address checklist of the generated GitHub release in [https://github.com/logos-blockchain/logos-blockchain/releases](https://github.com/logos-blockchain/logos-blockchain/releases)
- [ ] Publish release
- [ ] Post the link to the published release to this issue for easier review
- [ ] Post the link to the Docker image building workflow as appearing in [the Actions section][node-docker-build-workflow]

## Testnet deployment

- [ ] Verify the Logos Blockchain tools Docker image was properly built and pushed to the [GitHub container registry][logos-tools-image-container-registry]
- [ ] Wait for the new Docker image to be built after the release is published. It must have the `X.Y.Z` tag.
- [ ] Checkout `testnet` branch again and change the `compose.static.yml` symlink to now point to `compose.run.yml` with `ln -s -f compose.run.yml compose.static.yml`
- [ ] Update `.env.testnet` file to contain `NODE_IMAGE_LABEL=X.Y.Z` set to version being released
- [ ] Commit and push the changes to trigger environment re-deployment
- [ ] Wait around 1 minute for deployment to be updated. Environment is now live.
- [ ] If needed, at any time you can download fleet nodes' configs and logs from [https://testnet.blockchain.logos.co/internal/node-data/](https://testnet.blockchain.logos.co/internal/node-data/)
- [ ] Go back to the [GitHub Release][github-release-section] section and finalize the release

# Post-Release

- [ ] Update the release checklist template (this file and also `release-candidate.md`) or the GitHub release template with anything that was missing or that was fixed during the release process

---

[logos-tools-image-container-registry]: https://github.com/logos-blockchain/logos-blockchain/pkgs/container/logos-blockchain
[ceremony-workflow]: https://github.com/logos-blockchain/logos-blockchain/actions/workflows/genesis-ceremony.yml
[testnet-web-ui]: https://testnet.blockchain.logos.co/web
[build-logos-tools-docker-workflow]: https://github.com/logos-blockchain/logos-blockchain/actions/workflows/build-logos-tools.yml 
[release-bundling-workflow]: https://github.com/logos-blockchain/logos-blockchain/actions/workflows/prepare-release.yml
[github-release-section]: #release-publication
[node-docker-build-workflow]: https://github.com/logos-blockchain/logos-blockchain/actions/workflows/publish-node-image.yml
