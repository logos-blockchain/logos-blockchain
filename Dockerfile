# syntax=docker/dockerfile:1
# check=skip=SecretsUsedInArgOrEnv
# Ignore warnings about sensitive information as this is test data.

ARG LB_CIRCUITS_VERSION=v0.4.1
ARG LB_NODE_VERSION=0.1.3

# ===========================
# BUILD IMAGE
# ===========================

FROM alpine:latest AS builder

ARG LB_CIRCUITS_VERSION
ARG LB_NODE_VERSION

WORKDIR /logos-blockchain
COPY . .

RUN apk add --no-cache curl bash
RUN scripts/setup-logos-blockchain-circuits.sh "$LB_CIRCUITS_VERSION" "/opt/circuits"
RUN scripts/setup-logos-blockchain-node.sh "$LB_NODE_VERSION" "linux-$(uname -m)"

# ===========================
# NODE IMAGE
# ===========================

FROM debian:trixie-slim

ARG LB_CIRCUITS_VERSION

LABEL maintainer="augustinas@status.im" \
    source="https://github.com/logos-blockchain/logos-blockchain" \
    description="Logos blockchain node image"

COPY --from=builder /opt/circuits /opt/circuits
COPY --from=builder /usr/local/bin/logos-blockchain-node /usr/local/bin/logos-blockchain-node

ENV LOGOS_BLOCKCHAIN_CIRCUITS=/opt/circuits

EXPOSE 3000 8080 9000 60000

ENTRYPOINT ["logos-blockchain-node"]
