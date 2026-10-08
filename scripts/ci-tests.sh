#!/bin/sh
# Run exactly what CI runs (.woodpecker/push.yml calls this script).
# Usage: scripts/ci-tests.sh            native, fast
#        scripts/ci-tests.sh --docker   inside the CI image, for exact parity
#        scripts/ci-tests.sh --woodpecker  the real pipeline files through woodpecker-cli (services included)
set -eu
cd "$(dirname "$0")/.."

if [ "${1:-}" = "--docker" ]; then
  image=$(sed -n 's/^ *image: *\(rust:.*\)/\1/p' .woodpecker/push.yml | head -1)
  # Tests reach the dev Postgres (scripts/dev.sh) through the host.
  exec docker run --rm -v "$PWD":/src -w /src -v mh3sp-rs-ci-target:/target \
    --add-host host.docker.internal:host-gateway \
    -e CARGO_TARGET_DIR=/target \
    -e DATABASE_URL=postgres://mh3sp:mh3sp@host.docker.internal:55432/mh3sp \
    "$image" scripts/ci-tests.sh
fi

if [ "${1:-}" = "--woodpecker" ]; then
  # The local runner mounts the working tree instead of cloning it.
  exec woodpecker-cli exec --backend-engine docker --local --repo-path "$PWD" .woodpecker/
fi

# Natively, use the dev environment's URLs unless they're already set (CI sets them).
if [ -z "${DATABASE_URL:-}" ] && [ -f .env ]; then
  set -a; . ./.env; set +a
fi

rustup component add rustfmt clippy >/dev/null 2>&1
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
