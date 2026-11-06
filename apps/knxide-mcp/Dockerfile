# knxide-mcp as a long-lived Streamable HTTP server. Build from the repository root:
#
#   docker build -f apps/knxide-mcp/Dockerfile -t knxide-mcp .
#   docker run -e KNXIDE_GATEWAY=192.168.1.50 -e KNXIDE_MCP_TOKEN=... -p 8092:8092 \
#       -v /path/to/knx.xml:/data/knx.xml:ro knxide-mcp --allow-write
#
# Builds natively on x86_64 and aarch64.

FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY apps apps
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p knxide-mcp \
    && cp target/release/knxide-mcp /usr/local/bin/knxide-mcp

FROM debian:bookworm-slim
COPY --from=build /usr/local/bin/knxide-mcp /usr/local/bin/knxide-mcp
RUN useradd --system --no-create-home knxide
USER knxide
ENV KNXIDE_MCP_LISTEN=0.0.0.0:8092 \
    KNXIDE_PROJECT=/data/knx.xml
EXPOSE 8092
ENTRYPOINT ["knxide-mcp"]
