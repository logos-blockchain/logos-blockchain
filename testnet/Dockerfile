# syntax=docker/dockerfile:1
# check=skip=SecretsUsedInArgOrEnv
# Ignore warnings about sensitive information as this is test data.

ARG VERSION=v0.3.1

# ===========================
# BUILD IMAGE
# ===========================

FROM rust:1.92.0-slim-bookworm AS builder

ARG VERSION

LABEL maintainer="augustinas@status.im" \
    source="https://github.com/logos-co/nomos-node" \
    description="Nomos testnet build image"

WORKDIR /nomos
COPY . .

# Install dependencies needed for building RocksDB.
RUN apt-get update && apt-get install -yq \
    git gcc g++ clang libssl-dev pkg-config ca-certificates curl

RUN chmod +x scripts/setup-nomos-circuits.sh && \
    scripts/setup-nomos-circuits.sh "$VERSION" "/opt/circuits"

ENV NOMOS_CIRCUITS=/opt/circuits

RUN cargo build --locked --release --all-features

# ===========================
# NODE IMAGE
# ===========================

FROM debian:bookworm-slim

ARG VERSION

LABEL maintainer="augustinas@status.im" \
    source="https://github.com/logos-co/nomos-node" \
    description="Nomos node image"

RUN apt-get update && apt-get install -yq \
    libstdc++6 \
    libssl3 \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /opt/circuits /opt/circuits

COPY --from=builder /nomos/target/release/nomos-node /usr/bin/nomos-node
COPY --from=builder /nomos/target/release/nomos-executor /usr/bin/nomos-executor
COPY --from=builder /nomos/target/release/nomos-cli /usr/bin/nomos-cli
COPY --from=builder /nomos/target/release/cfgsync-server /usr/bin/cfgsync-server
COPY --from=builder /nomos/target/release/cfgsync-client /usr/bin/cfgsync-client

ENV NOMOS_CIRCUITS=/opt/circuits

EXPOSE 3000 8080 9000 60000

ENTRYPOINT ["/usr/bin/nomos-node"]
