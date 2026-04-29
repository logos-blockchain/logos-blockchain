#!/bin/bash

RUSTFLAGS="-D warnings" cargo hack --feature-powerset --no-dev-deps --keep-going check
