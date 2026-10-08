#!/bin/sh
# Prepare the dev environment: make sure .env has every key the server and sqlx need, and create config.toml
# from config.example.toml. Idempotent: existing .env values and an existing config.toml are kept.
#   scripts/dev.sh --reset   also deletes the dev database (needed after a schema change: before 1.0.0 the schema
#                            file is rewritten in place, which sqlx refuses on an existing database)
set -eu
cd "$(dirname "$0")/.."

if [ "${1:-}" = "--reset" ]; then
  rm -f data/mh3sp.db data/mh3sp.db-wal data/mh3sp.db-shm
  echo "dev database deleted"
fi

# key=default; only keys missing from .env are appended.
DEFAULTS='DATABASE_URL=sqlite://data/mh3sp.db
RUST_LOG=info,sqlx=warn'

touch .env
echo "$DEFAULTS" | while IFS= read -r line; do
  key=${line%%=*}
  grep -q "^$key=" .env || { echo "$line" >> .env; echo "added $key to .env"; }
done

[ -f config.toml ] || { cp config.example.toml config.toml; echo "created config.toml"; }
mkdir -p data

echo "dev env ready: database data/mh3sp.db (created on first start)"
echo "run the server: cargo run -p server"
