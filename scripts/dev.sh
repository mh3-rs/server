#!/bin/sh
# Start the dev environment (Postgres + Valkey), make sure .env has every key the server and
# sqlx need, and create config.toml from config.example.toml. Idempotent: rerun it any time;
# existing .env values and an existing config.toml are kept.
#   scripts/dev.sh --reset   also wipes the Postgres and Valkey data first (needed after a schema change: before
#                            1.0.0 the schema file is rewritten in place, which sqlx refuses on an existing database)
set -eu
cd "$(dirname "$0")/.."

if [ "${1:-}" = "--reset" ]; then
  docker compose down -v
  rm -rf data
  echo "dev data wiped"
fi

# key=default; only keys missing from .env are appended.
DEFAULTS='DATABASE_URL=postgres://mh3sp:mh3sp@localhost:55432/mh3sp
VALKEY_URL=redis://localhost:56379
RUST_LOG=info,sqlx=warn'

touch .env
echo "$DEFAULTS" | while IFS= read -r line; do
  key=${line%%=*}
  grep -q "^$key=" .env || { echo "$line" >> .env; echo "added $key to .env"; }
done

[ -f config.toml ] || { cp config.example.toml config.toml; echo "created config.toml"; }

# Created up front: Docker on macOS creates a missing bind-mount folder too late for Postgres 18's entrypoint.
mkdir -p data/postgres
docker compose up -d --wait
echo "dev env ready: Postgres localhost:55432, Valkey localhost:56379 (stop: docker compose down)"
echo "run the server: cargo run -p server"
