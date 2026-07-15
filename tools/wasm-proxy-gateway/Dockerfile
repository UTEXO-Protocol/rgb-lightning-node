FROM rust:1.88-bookworm AS builder
WORKDIR /app

COPY Cargo.toml ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/wasm-proxy-gateway /usr/local/bin/wasm-proxy-gateway

EXPOSE 3001
ENV RUST_LOG=info

ENTRYPOINT ["/usr/local/bin/wasm-proxy-gateway"]
