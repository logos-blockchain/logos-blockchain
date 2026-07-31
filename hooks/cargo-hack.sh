#!/bin/bash

CARGO_BUILD_WARNINGS="deny" cargo hack --feature-powerset --no-dev-deps --keep-going --all-targets check
