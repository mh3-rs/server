# mh3sp server image. Configuration: environment variables, each with a default (see
# crates/server/src/config.rs). The SQLite database lives in the /opt/mh3sp/data volume (mount it to
# keep it; a website on the same machine shares the file).
# syntax=docker/dockerfile:1
FROM rust:1.98.0-bookworm AS build
WORKDIR /src
COPY . .
# FEATURES=prometheus builds the `…-prometheus` image (`/metrics/prometheus`).
ARG FEATURES=""
# The quest bots' tunnel is a private git dependency (over SSH in Cargo.toml): fetched over HTTPS with a Gitea token,
# given as a build secret (`--secret id=gitea,env=GITEA_AUTH`, GITEA_AUTH=user:token) and passed to git through its
# environment for this one command, so it's in no layer.
RUN --mount=type=secret,id=gitea,env=GITEA_AUTH \
    CARGO_NET_GIT_FETCH_WITH_CLI=true \
    GIT_CONFIG_COUNT=1 \
    GIT_CONFIG_KEY_0="url.https://${GITEA_AUTH}@git.pband.ch/.insteadOf" \
    GIT_CONFIG_VALUE_0="ssh://git@git.pband.ch/" \
    cargo build --release --locked -p server --features "$FEATURES"

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends libssl3 \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --home /opt/mh3sp mh3sp
WORKDIR /opt/mh3sp
COPY --from=build /src/target/release/mh3sp /usr/local/bin/mh3sp
COPY certs/dev certs/dev
RUN mkdir data && chown mh3sp data
VOLUME /opt/mh3sp/data
USER mh3sp
# TCP: OPN (TLS), LMP, FMP, health. UDP: the availability check, NAT negotiation, the relay (RELAY_PORTS). NAT
# negotiation needs the consoles' real addresses: run with host networking (see README.md).
EXPOSE 8200 8201 8202 8280 27900/udp 27901/udp 40000-40999/udp
# Liveness only: a database problem shows on /health/ready, not here (see crates/server/src/health.rs).
HEALTHCHECK --interval=15s --timeout=3s --start-period=20s --retries=3 CMD ["mh3sp", "healthcheck"]
ENTRYPOINT ["mh3sp"]
