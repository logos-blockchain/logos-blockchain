# syntax=docker/dockerfile:1
# check=skip=SecretsUsedInArgOrEnv
# Ignore warnings about sensitive information as this is test data.

ARG VERSION=v0.2.0

# ===========================
# BUILD IMAGE
# ===========================

FROM rust:1.93.0-slim-bookworm AS builder

ARG VERSION

LABEL maintainer="augustinas@status.im" \
    source="https://github.com/logos-blockchain/logos-blockchain" \
    description="Logos blockchain node build image"

WORKDIR /logos-blockchain
COPY . .

# Install dependencies needed for building RocksDB.
RUN apt-get update && apt-get install -yq \
    git gcc g++ clang libssl-dev pkg-config ca-certificates curl

RUN chmod +x scripts/setup-logos-blockchain-circuits.sh && \
    scripts/setup-logos-blockchain-circuits.sh "$VERSION" "/opt/circuits"

ENV LOGOS_BLOCKCHAIN_CIRCUITS=/opt/circuits

RUN cargo build --locked --release -p logos-blockchain-node

# ===========================
# NODE IMAGE
# ===========================

FROM debian:bookworm-slim

ARG VERSION

LABEL maintainer="augustinas@status.im" \
    source="https://github.com/logos-blockchain/logos-blockchain" \
    description="Logos blockchain node image"

RUN apt-get update && apt-get install -yq \
    libstdc++6 \
    libssl3 \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /opt/circuits /opt/circuits
COPY --from=builder /logos-blockchain/target/release/logos-blockchain-node /usr/local/bin/logos-blockchain-node

ENV LOGOS_BLOCKCHAIN_CIRCUITS=/opt/circuits

EXPOSE 3000 8080 9000 60000

ENTRYPOINT ["logos-blockchain-node"]