#!/bin/sh
# Prepare the dev environment: make sure .env has the keys the server and sqlx use in dev (every other
# setting has a default, see crates/server/src/config.rs). Idempotent: existing .env values are kept.
#   scripts/dev.sh --reset   also deletes the dev database (needed after a schema change: before 1.0.0 the schema
#                            file is rewritten in place, which sqlx refuses on an existing database)
set -eu
cd "$(dirname "$0")/.."

if [ "${1:-}" = "--reset" ]; then
  rm -f data/mh3sp.db data/mh3sp.db-wal data/mh3sp.db-shm
  echo "dev database deleted"
fi

# key=default; only keys missing from .env are appended. The addresses suit the lab: Dolphin on this machine, an
# unpatched disc whose NAT negotiation goes to AltWFC through the hosts file.
DEFAULTS='DATABASE_URL=sqlite://data/mh3sp.db
RUST_LOG=info,sqlx=warn
SQLX_OFFLINE=true
EXTERNAL_ADDRESS=127.0.0.1
NATNEG_HOSTNAME=natneg1.gs.nintendowifi.net
SECRET='"$(openssl rand -hex 32)"

touch .env
echo "$DEFAULTS" | while IFS= read -r line; do
  key=${line%%=*}
  grep -q "^$key=" .env || { echo "$line" >> .env; echo "added $key to .env"; }
done

mkdir -p data

echo "dev env ready: database data/mh3sp.db (created on first start)"
echo "run the server: cargo run -p server"
