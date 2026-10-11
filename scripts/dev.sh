#!/bin/sh
# Prepare the dev environment: `.env` with what sqlx's tools read, and `mh3sp.toml` with what the server needs in dev
# (every other key has a default, see crates/server/src/config.rs). Idempotent: existing files are kept.
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
SQLX_OFFLINE=true'
touch .env
echo "$DEFAULTS" | while IFS= read -r line; do
  key=${line%%=*}
  grep -q "^$key=" .env || { echo "$line" >> .env; echo "added $key to .env"; }
done

# The addresses suit the lab: Dolphin on this machine, an unpatched disc whose NAT negotiation goes to AltWFC through
# the hosts file; the content is the live branch's worktree beside this repository. The secret of an older `.env` is
# kept: the dev database's consoles stay recognised.
if [ ! -f mh3sp.toml ]; then
  secret=$(sed -n 's/^SECRET=//p' .env)
  cat > mh3sp.toml <<TOML
external_address = "127.0.0.1"
natneg_hostname = "natneg1.gs.nintendowifi.net"
secret = "${secret:-$(openssl rand -hex 32)}"
content_dir = "../content"
TOML
  echo "wrote mh3sp.toml"
fi

mkdir -p data

echo "dev env ready: database data/mh3sp.db (created on first start)"
echo "run the server: cargo run -p server"
