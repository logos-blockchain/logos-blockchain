#!/bin/bash

# The same check as the Clippy jobs of the Post-merge checks workflow: Clippy on every feature combination of every
# crate.
CARGO_BUILD_WARNINGS="deny" cargo hack clippy --workspace --feature-powerset --all-targets --keep-going
