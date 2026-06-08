# syntax=docker/dockerfile:1

# Stage 1: build the frontend
FROM node:22-alpine AS frontend
WORKDIR /app/frontend
COPY frontend/package.json frontend/package-lock.json* ./
RUN npm ci
COPY frontend/ ./
RUN npm run build

# Stage 2: build the rust binary (with embedded frontend)
FROM rust:1.85-slim AS backend
WORKDIR /app
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev git ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock* ./
COPY crates/ ./crates/
COPY migrations/ ./migrations/
COPY --from=frontend /app/frontend/public ./frontend/public
RUN cargo build --release --bin provider-stack

# Stage 3: minimal runtime
FROM gcr.io/distroless/cc-debian12
COPY --from=backend /app/target/release/provider-stack /usr/local/bin/provider-stack
EXPOSE 3000
ENTRYPOINT ["/usr/local/bin/provider-stack"]
