# Build stage
FROM rust:1-alpine AS builder

ARG TARGETPLATFORM

# Install build dependencies (rustls only, no OpenSSL needed)
RUN apk add --no-cache musl-dev

WORKDIR /build

# Copy manifests
COPY Cargo.toml Cargo.lock build.rs ./

# Copy source code
COPY src ./src

# Determine the Rust target based on platform
RUN RUST_TARGET=""; \
    case "$TARGETPLATFORM" in \
        linux/amd64) RUST_TARGET=x86_64-unknown-linux-musl ;; \
        linux/arm64) RUST_TARGET=aarch64-unknown-linux-musl ;; \
        *) echo "Unsupported platform: $TARGETPLATFORM" && exit 1 ;; \
    esac && \
    echo "Building for target: $RUST_TARGET" && \
    if [ "$RUST_TARGET" != "x86_64-unknown-linux-musl" ]; then \
        rustup target add "$RUST_TARGET"; \
    fi && \
    cargo build --release --target "$RUST_TARGET" && \
    mkdir -p /build/output && \
    cp "/build/target/$RUST_TARGET/release/mariadb_exporter" /build/output/

# Runtime stage
FROM alpine:latest

# Install runtime dependencies
RUN apk add --no-cache \
    ca-certificates \
    mariadb-client

# Create non-root user
RUN addgroup -g 10001 exporter && \
    adduser -D -u 10001 -G exporter exporter

WORKDIR /app

# Copy binary from builder
COPY --from=builder /build/output/mariadb_exporter /usr/local/bin/mariadb_exporter

# Make binary executable
RUN chmod +x /usr/local/bin/mariadb_exporter

# Switch to non-root user
USER exporter

# Default port
EXPOSE 9306

# Health check
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD wget --no-verbose --tries=1 --spider http://localhost:9306/health || exit 1

# Default command - using socket connection
# Override with podman run -e MARIADB_EXPORTER_DSN="..."
ENV MARIADB_EXPORTER_DSN="mysql:///mysql?socket=/var/run/mysqld/mysqld.sock&user=exporter"

ENTRYPOINT ["/usr/local/bin/mariadb_exporter"]
