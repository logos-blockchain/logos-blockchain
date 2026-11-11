---
title: "[CI GENERATED] Update Rust to version {{ env.RUST_VERSION }}"
labels: "ci-actions"
---
Update Rust to version {{ env.RUST_VERSION }}. Also, please check the output of the feature manager job to see if we can avoid importing unnecessary features and update the `Cargo.lock` file with `cargo update`.

The Dockerfiles pin their `rust:` base images by digest: update each tag and its digest together, with the digest that `docker buildx imagetools inspect rust:<tag>` reports.