#!/bin/sh
# Run exactly what CI runs (.woodpecker/push.yml calls this script).
# Usage: scripts/ci-tests.sh            native, fast
#        scripts/ci-tests.sh --docker   inside the CI image, for exact parity
#        scripts/ci-tests.sh --woodpecker  the real pipeline files through woodpecker-cli (services included)
set -eu
cd "$(dirname "$0")/.."

if [ "${1:-}" = "--docker" ]; then
  image=$(sed -n 's/^ *image: *//p' .woodpecker/push.yml | head -1)
  exec docker run --rm -v "$PWD":/src -w /src -v mh3sp-rs-ci-target:/target \
    -e CARGO_TARGET_DIR=/target "$image" scripts/ci-tests.sh
fi

if [ "${1:-}" = "--woodpecker" ]; then
  # The local runner mounts the working tree instead of cloning it.
  exec woodpecker-cli exec --backend-engine docker --local --repo-path "$PWD" .woodpecker/
fi

rustup component add rustfmt clippy >/dev/null 2>&1
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
