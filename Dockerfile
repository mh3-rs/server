# mh3sp server image. Configuration: environment variables, each with a default (see
# crates/server/src/config.rs). The SQLite database lives in the /opt/mh3sp/data volume (mount it to
# keep it; a website on the same machine shares the file).
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
COPY certs/dev certs/dev
RUN mkdir data && chown mh3sp data
VOLUME /opt/mh3sp/data
USER mh3sp
# OPN (TLS), LMP, FMP, health.
EXPOSE 8200 8201 8202 8280
# Liveness only: a database problem shows on /health/ready, not here (see crates/server/src/health.rs).
HEALTHCHECK --interval=15s --timeout=3s --start-period=20s --retries=3 CMD ["mh3sp", "healthcheck"]
ENTRYPOINT ["mh3sp"]
