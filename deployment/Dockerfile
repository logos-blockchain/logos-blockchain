# syntax=docker/dockerfile:1
# check=skip=SecretsUsedInArgOrEnv
# Ignore warnings about sensitive information as this is test data.

# ===========================
# BUILD IMAGE
# ===========================
FROM rust:1.96.0-slim-bookworm AS builder

WORKDIR /logos-blockchain
COPY . .

# Install dependencies needed for building RocksDB.
RUN apt-get update && apt-get install -yq \
    build-essential git clang libssl-dev pkg-config ca-certificates curl unzip

# Install Bun for L2 demo frontend.
RUN curl -fsSL https://bun.sh/install | bash
ENV PATH="/root/.bun/bin:$PATH"

# Build Frontend Webapp
WORKDIR /logos-blockchain/deployment/l2-sequencer-archival-demo/webapp
RUN bun install --frozen-lockfile
RUN VITE_SEQUENCER_URL=/api/sequencer VITE_ARCHIVER_URL=/api/archiver bun run build

# Build Rust Binaries
WORKDIR /logos-blockchain
RUN cargo build --locked --release

# ===========================
# NODE IMAGE
# ===========================
FROM debian:bookworm-slim

LABEL maintainer="augustinas@status.im" \
    source="https://github.com/logos-blockchain/logos-blockchain" \
    description="Logos Blockchain node and other binaries image"

RUN apt-get update && apt-get install -yq \
    libstdc++6 libssl3 ca-certificates \
    python3 python3-pip curl git \
    && rm -rf /var/lib/apt/lists/*

# Copy Binaries
COPY --from=builder /logos-blockchain/target/release/logos-blockchain-node /usr/bin/
COPY --from=builder /logos-blockchain/target/release/logos-blockchain-cfgsync-server /usr/bin/
COPY --from=builder /logos-blockchain/target/release/logos-blockchain-cfgsync-client /usr/bin/
COPY --from=builder /logos-blockchain/target/release/logos-blockchain-demo-sequencer /usr/bin/
COPY --from=builder /logos-blockchain/target/release/logos-blockchain-demo-archiver /usr/bin/
COPY --from=builder /logos-blockchain/target/release/logos-blockchain-faucet /usr/bin/

# Copy Frontend & Nginx config
COPY --from=builder /logos-blockchain/deployment/l2-sequencer-archival-demo/webapp/dist /var/www/l2-demo
COPY --from=builder /logos-blockchain/deployment/l2-sequencer-archival-demo/nginx/nginx.conf /etc/nginx/nginx.conf

# Copy Scripts
COPY --from=builder /logos-blockchain/deployment/scripts /opt/scripts

EXPOSE 80 3000 8000 8080 8090 9000 18080 60000
ENTRYPOINT ["/usr/bin/logos-blockchain-node"]
