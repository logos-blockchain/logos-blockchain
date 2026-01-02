# syntax=docker/dockerfile:1
# check=skip=SecretsUsedInArgOrEnv
# Ignore warnings about sensitive information as this is test data.

ARG VERSION=v0.3.1

# ===========================
# BUILD IMAGE
# ===========================
FROM rust:1.92.0-slim-bookworm AS builder

ARG VERSION

WORKDIR /nomos
COPY . .

# Install dependencies needed for building RocksDB.
RUN apt-get update && apt-get install -yq \
    git gcc g++ clang libssl-dev pkg-config ca-certificates curl unzip

# Install Bun for L2 demo frontend.
RUN curl -fsSL https://bun.sh/install | bash
ENV PATH="/root/.bun/bin:$PATH"

# Build Frontend Webapp
WORKDIR /nomos/testnet/l2-sequencer-archival-demo/webapp
RUN bun install --frozen-lockfile
RUN VITE_SEQUENCER_URL=/api/sequencer VITE_ARCHIVER_URL=/api/archiver bun run build

# Build Rust Binaries & Circuits
WORKDIR /nomos
ENV NOMOS_CIRCUITS=/opt/circuits
RUN scripts/setup-nomos-circuits.sh "$VERSION" "$NOMOS_CIRCUITS"
RUN cargo build --locked --release --all-features

# ===========================
# NODE IMAGE
# ===========================
FROM debian:bookworm-slim

ARG VERSION

LABEL maintainer="augustinas@status.im" \
    source="https://github.com/logos-co/nomos-node" \
    description="Logos Blockchain node and other binaries image"

RUN apt-get update && apt-get install -yq \
    libstdc++6 libssl3 ca-certificates nginx \
    && rm -rf /var/lib/apt/lists/*

# Copy Circuits
COPY --from=builder /opt/circuits /opt/circuits
ENV NOMOS_CIRCUITS=/opt/circuits

# Copy Binaries
COPY --from=builder /nomos/target/release/nomos-node /usr/bin/
COPY --from=builder /nomos/target/release/nomos-executor /usr/bin/
COPY --from=builder /nomos/target/release/nomos-cli /usr/bin/
COPY --from=builder /nomos/target/release/cfgsync-server /usr/bin/
COPY --from=builder /nomos/target/release/cfgsync-client /usr/bin/
COPY --from=builder /nomos/target/release/demo-sequencer /usr/bin/
COPY --from=builder /nomos/target/release/logos-blockchain-archiver /usr/bin/

# Copy Frontend & Nginx config
COPY --from=builder /nomos/testnet/l2-sequencer-archival-demo/webapp/dist /var/www/l2-demo
COPY --from=builder /nomos/testnet/l2-sequencer-archival-demo/nginx/nginx.conf /etc/nginx/nginx.conf

EXPOSE 80 3000 8080 8090 9000 18080 60000
ENTRYPOINT ["/usr/bin/nomos-node"]
