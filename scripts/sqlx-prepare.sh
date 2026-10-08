#!/bin/sh
# Regenerate .sqlx/: the query metadata sqlx's macros check queries against when there is no database (CI, Docker,
# and dev, where .env sets SQLX_OFFLINE=true). Run after changing a query or the schema; commit the result.
#
# What `cargo sqlx prepare` does, without needing sqlx-cli built with its SQLite driver: build against a scratch
# database with the macros writing their metadata into SQLX_OFFLINE_DIR.
set -eu
cd "$(dirname "$0")/.."
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cat migrations/*.sql | sqlite3 "$tmp/prepare.db"
rm -rf .sqlx && mkdir .sqlx
# Touch the sources so every query macro runs again.
find crates -name '*.rs' -exec touch {} +
DATABASE_URL="sqlite://$tmp/prepare.db" SQLX_OFFLINE=false SQLX_OFFLINE_DIR="$PWD/.sqlx" \
  cargo check --workspace --all-targets --quiet
echo "$(ls .sqlx | wc -l | tr -d ' ') queries in .sqlx/"
