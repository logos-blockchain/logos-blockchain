# syntax=docker/dockerfile:1@sha256:4edf897a3ffa55b89f906fc8cc78afdb3f1834cc9c7083565e611a8a7d5fe99e
# check=skip=SecretsUsedInArgOrEnv
# Ignore warnings about sensitive information as this is test data.
# The syntax directive pins docker/dockerfile 1.27.1.

# ===========================
# BUILD IMAGE
# ===========================
# Rust 1.99.0 on Debian 12 (debian:bookworm-20261005-slim).
FROM rust:1.99.0-slim-bookworm@sha256:2c3a22f0a5533ea2dd5a16627bc841228151faa2d4de2644ac9987e4a2f1f2fa AS builder

WORKDIR /logos-blockchain
COPY . .

# Install dependencies needed for building RocksDB.
RUN apt-get update && apt-get install -yq \
    build-essential git clang libssl-dev pkg-config ca-certificates curl unzip

# Build Rust Binaries, with their dependency list embedded so that image scanners and `cargo audit bin`
# can check them for vulnerable crates.
RUN cargo install cargo-auditable --locked --version 0.7.7
RUN cargo auditable build --locked --release

# ===========================
# NODE IMAGE
# ===========================
# Debian 12.15 (debian:bookworm-20261104-slim).
FROM debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc

LABEL maintainer="augustinas@status.im" \
    source="https://github.com/logos-blockchain/logos-blockchain" \
    description="Logos Blockchain node and other binaries image"

RUN apt-get update && apt-get install -yq \
    libstdc++6 libssl3 ca-certificates \
    python3 python3-pip curl git \
    && rm -rf /var/lib/apt/lists/*

# Copy Binaries
COPY --from=builder /logos-blockchain/target/release/logos-blockchain-node /usr/bin/
COPY --from=builder /logos-blockchain/target/release/logos-blockchain-faucet /usr/bin/

# Copy Scripts
COPY --from=builder /logos-blockchain/deployment/scripts /opt/scripts

# Runs as the node image's unprivileged user, with the same UID, since both images share `/node-data` in
# deployments: a new volume mounted there takes this ownership, an existing one has to be
# `chown -R 10001:10001`'d once.
RUN groupadd --system --gid 10001 logos && \
    useradd --system --uid 10001 --gid logos --create-home --home-dir /home/logos --shell /usr/sbin/nologin logos && \
    mkdir /node-data && \
    chown logos:logos /node-data

USER logos
WORKDIR /home/logos

EXPOSE 80 3000 8000 8080 8090 9000 18080 60000
ENTRYPOINT ["/usr/bin/logos-blockchain-node"]
