#!/bin/sh
# Start the dev environment (Postgres + Valkey) and make sure .env has every key the
# server and sqlx need. Idempotent: rerun it any time; existing .env values are kept.
set -eu
cd "$(dirname "$0")/.."

# key=default; only keys missing from .env are appended.
DEFAULTS='DATABASE_URL=postgres://mh3sp:mh3sp@localhost:55432/mh3sp
VALKEY_URL=redis://localhost:56379
RUST_LOG=info'

touch .env
echo "$DEFAULTS" | while IFS= read -r line; do
  key=${line%%=*}
  grep -q "^$key=" .env || { echo "$line" >> .env; echo "added $key to .env"; }
done

docker compose up -d --wait
echo "dev env ready: Postgres localhost:55432, Valkey localhost:56379 (stop: docker compose down)"
