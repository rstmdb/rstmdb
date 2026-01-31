# syntax=docker/dockerfile:1.4

# ============================================================================
# Stage 1: Chef - Prepare dependency recipe
# ============================================================================
FROM rust:1.83-slim-bookworm AS chef

RUN cargo install cargo-chef --locked
WORKDIR /app

# ============================================================================
# Stage 2: Planner - Analyze dependencies
# ============================================================================
FROM chef AS planner

# Copy workspace Cargo.toml and modify to exclude bench crate
COPY Cargo.toml Cargo.lock ./
COPY rstmdb-protocol/Cargo.toml rstmdb-protocol/
COPY rstmdb-wal/Cargo.toml rstmdb-wal/
COPY rstmdb-core/Cargo.toml rstmdb-core/
COPY rstmdb-storage/Cargo.toml rstmdb-storage/
COPY rstmdb-server/Cargo.toml rstmdb-server/
COPY rstmdb-client/Cargo.toml rstmdb-client/
COPY rstmdb-cli/Cargo.toml rstmdb-cli/

# Remove bench from workspace members
RUN sed -i 's/"rstmdb-bench",\?//g' Cargo.toml

# Create dummy source files for dependency resolution
RUN mkdir -p src && echo "fn main() {}" > src/main.rs && \
    for crate in rstmdb-protocol rstmdb-wal rstmdb-core rstmdb-storage rstmdb-server rstmdb-client rstmdb-cli; do \
    mkdir -p $crate/src && echo "" > $crate/src/lib.rs; \
    done && \
    echo "fn main() {}" > rstmdb-cli/src/main.rs

RUN cargo chef prepare --recipe-path recipe.json

# ============================================================================
# Stage 3: Builder - Build dependencies (cached) and application
# ============================================================================
FROM chef AS builder

# Install build dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Build dependencies (this layer is cached unless Cargo.toml/Cargo.lock change)
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

# Copy actual source code (excluding bench)
COPY Cargo.toml Cargo.lock ./
RUN sed -i 's/"rstmdb-bench",\?//g' Cargo.toml
COPY src/ src/
COPY rstmdb-protocol/ rstmdb-protocol/
COPY rstmdb-wal/ rstmdb-wal/
COPY rstmdb-core/ rstmdb-core/
COPY rstmdb-storage/ rstmdb-storage/
COPY rstmdb-server/ rstmdb-server/
COPY rstmdb-client/ rstmdb-client/
COPY rstmdb-cli/ rstmdb-cli/

# Build the server and CLI
RUN cargo build --release -p rstmdb -p rstmdb-cli && \
    strip target/release/rstmdb target/release/rstmdb-cli

# ============================================================================
# Stage 4: Runtime - Minimal production image
# ============================================================================
FROM debian:bookworm-slim AS runtime

# Install runtime dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -r -u 1000 -s /bin/false rstmdb \
    && mkdir -p /data /config \
    && chown -R rstmdb:rstmdb /data /config

# Copy binaries from builder
COPY --from=builder /app/target/release/rstmdb /usr/local/bin/
COPY --from=builder /app/target/release/rstmdb-cli /usr/local/bin/

# Set up runtime environment
USER rstmdb
WORKDIR /data

# Default configuration via environment variables
ENV RUST_LOG=info \
    RSTMDB_BIND=0.0.0.0:7401 \
    RSTMDB_DATA=/data

# Expose the default port
EXPOSE 7401

# Run the server
ENTRYPOINT ["rstmdb"]
