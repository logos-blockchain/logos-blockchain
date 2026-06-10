---
name: Release Candidate Checklist
about: Checklist for releasing a new candidate
title: Release Checklist for X.Y.Z-rc.N
labels: release
---

<!---

Most of the template content is the same or very similar to what is in `release.md`. So any changes to this file should be reflected there where relevant, and viceversa.

--->

# IMPORTANT

**READ THIS BEFORE STARTING WITH THE RELEASE**

* If any changes other than release-specific ones are needed, e.g. a bugfix or some ceremony-related fix that is useful also for future releases, they should be merged with a PR against `master` and not pushed to the release branch. Then, there are two possible strategies:
    * the release continues from the same branch, in which case the fix is cherry-picked from `master` into the release branch and the branching/reset step as part of the branch setup is skipped
    * a new release candidate is restarted from the latest `master`: in this case the existing `release/X.Y.Z` branch is hard-reset back onto `master` and force-pushed, discarding the previous rc's commits from the branch tip (the tags remain)
* Progress on the checklist must be provided as comments to the issue.

---

## Branch Setup

- [ ] Edit the name of this issue to use the actual version being released
- [ ] Branch out from the latest `master` commit with a release branch named `release/X.Y.Z`. If this is not the first release candidate for this version, HARD reset the branch on top of `master` and force-push the new tip
- [ ] If this is not the first release candidate for this version, post the link of the previous release candidate GH release and the previous release candidate checklist. E.g., for the `X.Y.Z-rc.2` candidate, post the checklist and GH release for `X.Y.Z-rc.1`

## Devnet genesis (optional, only whenever a new devnet deployment - with a new genesis - is required)

- [ ] Update the inscription file at `deployment/ceremony/genesis/devnet/inscribe.yaml` by setting the `chain_id` field to `X.Y.Z-rc.N` and the `genesis_time` field to be approximately 10 mins in the future, following the existing [ISO 8601](https://en.wikipedia.org/wiki/ISO_8601) datetime format. No need to change the `entropy_sources`
- [ ] Commit and push the changes
- [ ] Manually trigger the [ceremony workflow][ceremony-workflow] from the `HEAD` of the release branch specifying the `devnet` image tag and the right version number `X.Y.Z-rc.N`
- [ ] Post the link to the workflow run to this issue for easier review
- [ ] Wait for the workflow run to complete. The workflow will push a new commit on the release branch with the updated devnet deployment settings.
- [ ] Checkout and hard reset the `devnet` branch to point to the latest commit of the current release branch
- [ ] Create a new symlink `compose.static.yml` -> `compose.setup.yml` with `ln -sf compose.setup.yml compose.static.yml`
- [ ] Commit and push to `devnet` branch to trigger the cleanup
- [ ] Wait around 1 minute for the previous deployment to be cleaned. Visit the [Devnet web UI][devnet-web-ui] and make sure it's in setup mode.

## Release candidate preparation

- [ ] Checkout and pull the release branch, it should contain the bot generated commit updating the deployment settings as its `HEAD`
- [ ] Bump the Cargo workspace version to match the new release version `X.Y.Z-rc.N`
- [ ] Re-generate the workspace `Cargo.lock` file with `cargo update -w`
- [ ] Verify the `Cargo.lock` is now up to date with `cargo update -w --locked`
- [ ] Verify the `HEAD` of the release branch has green CI ✅
- [ ] Commit the changes, and tag them with `X.Y.Z-rc.N`
- [ ] Push the commit and the tag
- [ ] Manually trigger the [Logos Blockchain tools build workflow][build-logos-tools-docker-workflow] from the `HEAD` of the release branch (with the latest changes) specifying the `devnet` image tag.
- [ ] Post the link to the workflow run to this issue for easier review
- [ ] Without waiting for the workflow to complete, move on to the next section

## Release candidate publication

- [ ] Manually trigger the [bundling workflow][release-bundling-workflow] from the `X.Y.Z-rc.N` tag on GitHub with the `release-candidate` input to prepare the GitHub release draft with the built binaries
- [ ] Post the link to the workflow run to this issue for easier review
- [ ] Wait for the bundling workflow to complete and generate a draft GitHub pre-release.
- [ ] Address checklist of the generated GitHub release in [https://github.com/logos-blockchain/logos-blockchain/releases](https://github.com/logos-blockchain/logos-blockchain/releases)
- [ ] Publish release
- [ ] Post the link to the published release to this issue for easier review
- [ ] Post the link to the Docker image building workflow as appearing in [the Actions section][node-docker-build-workflow]

## Devnet deployment

- [ ] Verify the Logos Blockchain tools Docker image was properly built and pushed to the [GitHub container registry][logos-tools-image-container-registry]
- [ ] Wait for the new Docker image to be built after the release is published. It must have the `X.Y.Z-rc.N` tag.
- [ ] Checkout `devnet` branch again and change the `compose.static.yml` symlink to now point to `compose.run.yml` with `ln -s -f compose.run.yml compose.static.yml`
- [ ] Update `.env.devnet` file to contain `NODE_IMAGE_LABEL=X.Y.Z-rc.N` set to version being released
- [ ] Commit and push the changes to trigger environment re-deployment
- [ ] Wait around 1 minute for deployment to be updated. Environment is now live.
- [ ] If needed, at any time you can download fleet nodes' configs and logs from [https://devnet.blockchain.logos.co/internal/node-data/](https://devnet.blockchain.logos.co/internal/node-data/)
- [ ] Go back to the [GitHub Release][github-release-candidate-section] section and finalize the release candidate

# Post-Release

- [ ] If this release candidate is ready to be "promoted" to a full release, open a new ticket using the release template and follow the checklist in there
- [ ] Update the release checklist template (this file and also `release.md`) or the GitHub release template with anything that was missing or that was fixed during the release process

---

[logos-tools-image-container-registry]: https://github.com/logos-blockchain/logos-blockchain/pkgs/container/logos-blockchain
[ceremony-workflow]: https://github.com/logos-blockchain/logos-blockchain/actions/workflows/genesis-ceremony.yml
[devnet-web-ui]: https://devnet.blockchain.logos.co/web
[build-logos-tools-docker-workflow]: https://github.com/logos-blockchain/logos-blockchain/actions/workflows/build-logos-tools.yml 
[release-bundling-workflow]: https://github.com/logos-blockchain/logos-blockchain/actions/workflows/prepare-release.yml
[github-release-candidate-section]: #release-candidate-publication
[node-docker-build-workflow]: https://github.com/logos-blockchain/logos-blockchain/actions/workflows/publish-node-image.yml
