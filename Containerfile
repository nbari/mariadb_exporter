# Build stage
FROM rust:1-alpine AS builder

# Install build dependencies
RUN apk add --no-cache \
    musl-dev \
    openssl-dev \
    openssl-libs-static

WORKDIR /build

# Copy manifests
COPY Cargo.toml Cargo.lock build.rs ./

# Copy source code
COPY src ./src

# Build release binary
RUN cargo build --release --target x86_64-unknown-linux-musl

# Runtime stage
FROM alpine:latest

# Install runtime dependencies
RUN apk add --no-cache \
    ca-certificates \
    mariadb-client

# Create non-root user
RUN addgroup -g 999 exporter && \
    adduser -D -u 999 -G exporter exporter

WORKDIR /app

# Copy binary from builder
COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/mariadb_exporter /usr/local/bin/mariadb_exporter

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
