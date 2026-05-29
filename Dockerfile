# syntax=docker/dockerfile:1
# check=skip=SecretsUsedInArgOrEnv
# Ignore warnings about sensitive information as this is test data.

ARG LB_NODE_VERSION=0.1.3
# Cache directory used by the circuits. Not meant to be overridden.
ARG LB_CACHE=/root/.cache/logos/blockchain

# ===========================
# BUILD IMAGE
# ===========================

FROM alpine:latest AS builder

ARG LB_NODE_VERSION

WORKDIR /logos-blockchain
COPY . .

RUN apk add --no-cache curl bash
RUN scripts/setup-logos-blockchain-node.sh "$LB_NODE_VERSION" "linux-$(uname -m)"

# ===========================
# NODE IMAGE
# ===========================

FROM debian:trixie-slim

ARG LB_CACHE

LABEL maintainer="augustinas@status.im" \
    source="https://github.com/logos-blockchain/logos-blockchain" \
    description="Logos blockchain node image"

# Copies the entire cache dir.
# We only need the circuits, but this is currently much simpler than just copying the circuits subdir.
# This might be addressed later, after the circuits directories structure is standardised.
COPY --from=builder $LB_CACHE $LB_CACHE
COPY --from=builder /usr/local/bin/logos-blockchain-node /usr/local/bin/logos-blockchain-node

EXPOSE 3000 8080 9000 60000

ENTRYPOINT ["logos-blockchain-node"]
