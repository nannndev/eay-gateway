# eay-gateway in a container. Build: docker build -t eay-gateway .
# Run (admin API needs a token because the container listens on all interfaces):
#   docker run -d -p 127.0.0.1:20129:20129 -v eay-data:/data \
#     -e EAY_GATEWAY_ADMIN_TOKEN=change-me ghcr.io/nannndev/eay-gateway

FROM rust:1-slim-bookworm AS build
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN cargo build --release --locked --bin eay-gateway

FROM debian:bookworm-slim
# ca-certificates for HTTPS to providers, libssl3 for the TLS backend; tini reaps MCP child processes.
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3 tini \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home-dir /data --shell /usr/sbin/nologin eay \
 && mkdir /data && chown eay /data
COPY --from=build /src/target/release/eay-gateway /usr/local/bin/eay-gateway
USER eay
ENV EAY_GATEWAY_HOST=0.0.0.0 \
    EAY_GATEWAY_PORT=20129 \
    EAY_GATEWAY_DATA_DIR=/data
VOLUME ["/data"]
EXPOSE 20129
HEALTHCHECK --interval=30s --timeout=3s CMD ["eay-gateway", "--version"]
ENTRYPOINT ["/usr/bin/tini", "--", "eay-gateway"]
