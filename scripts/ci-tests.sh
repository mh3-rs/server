#!/bin/sh
# Run exactly what CI runs (.woodpecker/push.yml calls this script).
# Usage: scripts/ci-tests.sh            native, fast
#        scripts/ci-tests.sh --docker   inside the CI image, for exact parity
#        scripts/ci-tests.sh --woodpecker  the real pipeline files through woodpecker-cli
set -eu
cd "$(dirname "$0")/.."

if [ "${1:-}" = "--docker" ]; then
  image=$(sed -n 's/^ *image: *\(rust:.*\)/\1/p' .woodpecker/push.yml | head -1)
  exec docker run --rm -v "$PWD":/src -w /src -v mh3sp-rs-ci-target:/target \
    -e CARGO_TARGET_DIR=/target "$image" scripts/ci-tests.sh
fi

if [ "${1:-}" = "--woodpecker" ]; then
  # The local runner mounts the working tree instead of cloning it.
  exec woodpecker-cli exec --backend-engine docker --local --repo-path "$PWD" .woodpecker/
fi

# Event content for the tests: MH3SP's files at the commit the corpus was recorded with. Never in this repository:
# CONTENT_DIR if set, else the MH3SP checkout next to it, else a clone in the target folder.
MH3SP_COMMIT=f53b06c
if [ -z "${CONTENT_DIR:-}" ] && [ ! -d ../mh3sp/event ]; then
  CONTENT_DIR="${CARGO_TARGET_DIR:-target}/mh3sp-$MH3SP_COMMIT"
  if [ ! -d "$CONTENT_DIR/event" ]; then
    git clone --quiet https://github.com/sepalani/MH3SP "$CONTENT_DIR"
    git -C "$CONTENT_DIR" checkout --quiet "$MH3SP_COMMIT"
  fi
  export CONTENT_DIR
fi

rustup component add rustfmt clippy >/dev/null 2>&1
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
