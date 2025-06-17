# Dockerfile for the server

# --- Build Stage ---
FROM rust:1.78 as builder

# Set up the workspace
WORKDIR /usr/src/app
COPY . .

# Build the server binary
RUN cargo build --release -p server

# --- Final Stage ---
FROM debian:buster-slim

# Copy the server binary from the builder stage
COPY --from=builder /usr/src/app/target/release/server /usr/local/bin/server

# Set the binary as the entrypoint
ENTRYPOINT ["/usr/local/bin/server"] 