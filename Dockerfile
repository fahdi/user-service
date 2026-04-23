# Multi-stage Dockerfile for user-service (following auth-service patterns)
FROM rust:1.95-slim as builder

# Install system dependencies for building
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Set working directory
WORKDIR /app

# Copy Cargo files first for better caching
COPY Cargo.toml Cargo.lock ./

# Create dummy source files to build dependencies
RUN mkdir src && echo "fn main() {}" > src/main.rs && echo "pub fn lib_placeholder() {}" > src/lib.rs
RUN cargo build --release
RUN rm -rf src target/release/deps/user_service* target/release/deps/libuser_service* target/release/user-service target/release/.fingerprint/user-service*

# Copy source code
COPY src ./src

# Build the actual application
RUN cargo build --release

# Runtime stage - use slim image for smaller size
FROM debian:bookworm-slim

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user for security
RUN useradd -r -s /bin/false -m -d /app appuser

# Set working directory
WORKDIR /app

# Copy the binary from builder stage
COPY --from=builder /app/target/release/user-service /app/user-service

# Change ownership to non-root user
RUN chown -R appuser:appuser /app

# Switch to non-root user
USER appuser

# Expose port (user-service runs on 8083)
EXPOSE 8083

# Health check
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:8083/health || exit 1

# Run the service
CMD ["./user-service"]