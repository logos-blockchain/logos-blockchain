# syntax=docker/dockerfile:1
# check=skip=SecretsUsedInArgOrEnv
# Ignore warnings about sensitive information as this is test data.

ARG VERSION=v0.2.0

# ===========================
# BUILD IMAGE
# ===========================

FROM rust:1.92.0-slim-bookworm AS builder

ARG VERSION

LABEL maintainer="augustinas@status.im" \
    source="https://github.com/logos-co/nomos-node" \
    description="Nomos node build image"

WORKDIR /nomos
COPY . .

# Install dependencies needed for building RocksDB.
RUN apt-get update && apt-get install -yq \
    git gcc g++ clang libssl-dev pkg-config ca-certificates curl

RUN chmod +x scripts/setup-nomos-circuits.sh && \
    scripts/setup-nomos-circuits.sh "$VERSION" "/opt/circuits"

ENV NOMOS_CIRCUITS=/opt/circuits

RUN cargo build --locked --release -p nomos-node

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
COPY --from=builder /nomos/target/release/nomos-node /usr/local/bin/nomos-node

ENV NOMOS_CIRCUITS=/opt/circuits

EXPOSE 3000 8080 9000 60000

ENTRYPOINT ["nomos-node"]
