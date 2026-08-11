# =============================================================================
# LCode - Rust CLI code agent
# Multi-stage build: builder → runtime (distroless)
# =============================================================================

# ---- Builder stage ----
FROM rust:1.94-slim AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src/ src/

# Build dependencies first for better layer caching
RUN cargo build --release

# ---- Runtime stage ----
FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    git \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/lcode /usr/local/bin/lcode

WORKDIR /workspace
ENTRYPOINT ["lcode"]
CMD ["--help"]
