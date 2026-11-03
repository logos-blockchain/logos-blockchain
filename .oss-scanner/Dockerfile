# Image for Anthropic's OSS scanner (https://github.com/anthropics/oss-scanner).

FROM rust:1.99.0-bookworm

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
  build-essential git clang libssl-dev pkg-config ca-certificates curl unzip jq \
  && rm -rf /var/lib/apt/lists/*

# scanner contract: the checkout lives inside the image, at /src
COPY . /src
WORKDIR /src

# rust-toolchain.toml asks for clippy, which the image's minimal toolchain leaves out.
RUN rustup component add clippy

# Every crate in Cargo.lock, for every target, so that builds with other features stay offline too.
RUN cargo fetch --locked

# rust-rapidsnark's build script downloads rapidsnark into its OUT_DIR, which changes with the profile, the features
# and RUSTFLAGS, so an offline build in any other configuration would download it again. Fetch it once, with the
# crate's own script and version, and point the build script at it.
RUN set -eu; \
  crate="$(cargo metadata --locked --format-version 1 \
  | jq -r '.packages[] | select(.name == "rust-rapidsnark") | .manifest_path | rtrimstr("/Cargo.toml")')"; \
  host="$(rustc -vV | sed -n 's/^host: //p')"; \
  OUT_DIR=/opt/rapidsnark TARGET="$host" RAPIDSNARK_VERSION="$(cat "$crate/../RAPIDSNARK_VERSION")" \
  sh "$crate/download_rapidsnark.sh"; \
  ln -s "/opt/rapidsnark/rapidsnark/${host%%-*}" /opt/rapidsnark/lib
ENV RAPIDSNARK_LIB_DIR=/opt/rapidsnark/lib

# The circuits' -sys crates download into ~/.cache/logos/blockchain, which every configuration shares. Download it with
# one of them first: unpacked while the others' build scripts already run, it would look changed to them, and the
# first cargo command in a scan would rebuild most of the node.
RUN cargo build --locked --release -p logos-blockchain-circuits-signature-sys

# Everything cargo needs is fetched. From here on it must not try to reach a registry.
ENV CARGO_NET_OFFLINE=true

# Build node in release mode with `testing` feature and set e2e env variable
RUN cargo build --locked --release -p logos-blockchain-node --features testing
ENV LOGOS_BLOCKCHAIN_NODE_BIN=/src/target/release/logos-blockchain-node