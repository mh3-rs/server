# mh3sp server image. Configuration: /opt/mh3sp/config.toml (a copy of config.example.toml;
# mount your own to change it), DATABASE_URL overrides its URL.
FROM rust:1.98.0-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p server

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends libssl3 \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --home /opt/mh3sp mh3sp
WORKDIR /opt/mh3sp
COPY --from=build /src/target/release/mh3sp /usr/local/bin/mh3sp
COPY config.example.toml config.toml
COPY certs/dev certs/dev
USER mh3sp
# OPN (TLS), LMP, FMP, health.
EXPOSE 8200 8201 8202 8280
# Liveness only: a Postgres outage shows on /health/ready, not here (see crates/server/src/health.rs).
HEALTHCHECK --interval=15s --timeout=3s --start-period=20s --retries=3 CMD ["mh3sp", "healthcheck"]
ENTRYPOINT ["mh3sp"]
