# syntax=docker/dockerfile:1
# mh3sp server image. Configuration: /opt/mh3sp/mh3sp.toml, every key with a default (deploy/README.md,
# crates/server/src/config.rs). The SQLite database lives in the /opt/mh3sp/data volume (mount it to keep it).
FROM rust:1.98.0-bookworm AS build
WORKDIR /src
COPY . .
# FEATURES=prometheus builds the `…-prometheus` image (`/metrics/prometheus`).
ARG FEATURES=""
RUN cargo build --release --locked -p server --features "$FEATURES"

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends libssl3 libcap2-bin \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 999 --home /opt/mh3sp mh3sp
WORKDIR /opt/mh3sp
COPY --from=build /src/target/release/mh3sp /usr/local/bin/mh3sp
# The Nintendo login listens on port 80 (the patched game calls plain HTTP there), below 1024: the binary may bind it
# without running as root.
RUN setcap cap_net_bind_service=+ep /usr/local/bin/mh3sp
RUN mkdir data && chown mh3sp data
VOLUME /opt/mh3sp/data
USER mh3sp
# TCP: the Nintendo login (HTTP), OPN (TLS), LMP, FMP, health. UDP: the availability check, NAT negotiation, the
# relay (relay_ports). Publish them unchanged: NAT negotiation needs the consoles' real addresses, which rootful Docker
# on Linux keeps (see deploy/README.md).
EXPOSE 80 8200 8201 8202 8280 27900/udp 27901/udp 40000-40099/udp
# Liveness only: a database problem shows on /health/ready, not here (see crates/server/src/health.rs).
HEALTHCHECK --interval=15s --timeout=3s --start-period=20s --retries=3 CMD ["mh3sp", "healthcheck"]
ENTRYPOINT ["mh3sp"]
