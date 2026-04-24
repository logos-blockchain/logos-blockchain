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
- [ ] Change the devnet deployment settings to use the version number in ALL protocol names, e.g., `/logos-blockchain-devnet-X.Y.Z-rc.N/mempool/1.0.0`
- [ ] Apply any other changes to the devnet deployment settings and push the changes. If a ceremony will be run, stuff like genesis block can be ignored since it will be overridden as the outcome of the ceremony.

## Devnet ceremony (optional, only whenever a devnet ceremony is required)

- [ ] Manually trigger the [ceremony tools Docker build workflow][build-logos-tools-docker-workflow] from the `HEAD` of the release branch (with the latest changes) specifying the `devnet` image tag.
- [ ] Post the link to the workflow run to this issue for easier review
- [ ] Wait for the workflow run to complete
- [ ] Verify the right image with the right tag was pushed to the [GitHub container registry][logos-tools-image-container-registry]
- [ ] Checkout and hard reset the `devnet` branch to point to the latest commit on the current release branch
- [ ] Create a new symlink `compose.static.yml` -> `compose.setup.yml` with `ln -s -f compose.setup.yml compose.static.yml`
- [ ] Push to `devnet` branch to trigger the ceremony and generate a new genesis state
- [ ] Wait around 1 minute for deployment to be updated with the new changes and for the ceremony to happen. Until ready, you should see a `502` error while the containers restart when visiting [https://devnet.blockchain.logos.co/web/cfgsync/deployment-settings](https://devnet.blockchain.logos.co/web/cfgsync/deployment-settings)
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
- [ ] Manually trigger the [bundling workflow][release-bundling-workflow] from the `X.Y.Z-rc.N` tag on GitHub with the `release-candidate` input to prepare the GitHub release draft with the build binaries
- [ ] Post the link to the workflow run to this issue for easier review
- [ ] Wait for the bundling workflow to complete and generate a draft GitHub pre-release. While the release is in progress, follow the steps in the [Devnet deployment][devnet-deployment-section] section below.
- [ ] Address checklist of the generated GitHub release in [https://github.com/logos-blockchain/logos-blockchain/releases](https://github.com/logos-blockchain/logos-blockchain/releases)
- [ ] Publish release
- [ ] Post the link to the published release to this issue for easier review

## Devnet deployment

- [ ] Checkout `devnet` branch again and change the `compose.static.yml` symlink to now point to `compose.run.yml` with `ln -s -f compose.run.yml compose.static.yml`
- [ ] Commit and push the changes to trigger environment re-deployment. Environment is now live.
- [ ] Wait around 1 minute for deployment to be updated
- [ ] If needed, at any time you can download fleet nodes' configs and logs from [https://devnet.blockchain.logos.co/internal/node-data/](https://devnet.blockchain.logos.co/internal/node-data/)
- [ ] Go back to the [GitHub Release][github-release-candidate-section] section and finalize the release candidate

# Post-Release

- [ ] If this release candidate is ready to be "promoted" to a full release, open a new ticket using the release template and follow the checklist in there
- [ ] Update the release checklist template (this file and also `release.md`) or the GitHub release template with anything that was missing or that was fixed during the release process

---

[logos-tools-image-container-registry]: https://github.com/logos-blockchain/logos-blockchain/pkgs/container/logos-blockchain
[build-logos-tools-docker-workflow]: https://github.com/logos-blockchain/logos-blockchain/actions/workflows/build-logos-tools.yml 
[release-bundling-workflow]: https://github.com/logos-blockchain/logos-blockchain/actions/workflows/prepare-release.yml
[devnet-deployment-section]: #devnet-deployment
[github-release-candidate-section]: #release-candidate-publication
