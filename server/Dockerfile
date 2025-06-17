# Dockerfile for the server

# --- Build Stage ---
FROM rust:1.78 as builder

# Install musl target
RUN rustup target add x86_64-unknown-linux-musl

# Create a new empty project to cache dependencies
WORKDIR /usr/src/app
RUN cargo new --bin server
WORKDIR /usr/src/app/server
COPY Cargo.toml Cargo.lock ./
RUN cargo build --release --target x86_64-unknown-linux-musl --all-features

# Copy your actual source code
COPY src ./src
RUN touch src/main.rs && cargo build --release --target x86_64-unknown-linux-musl --all-features

# --- Final Stage ---
# Use a minimal image
FROM scratch

# Copy the server binary from the builder stage
COPY --from=builder /usr/src/app/server/target/x86_64-unknown-linux-musl/release/server /server

# Set the binary as the entrypoint
ENTRYPOINT ["/server"] 