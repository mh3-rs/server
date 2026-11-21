#!/bin/sh
# Run exactly what CI runs (.woodpecker/ci.yaml calls this script, a step per part).
# Usage: scripts/ci.sh [lint | test]        native, fast: one part, or both with no argument
#        scripts/ci.sh --docker [lint | test]  inside the CI image, for exact parity
#        scripts/ci.sh --woodpecker         the checks' pipeline (ci.yaml) through woodpecker-cli
set -eu
cd "$(dirname "$0")/.."

if [ "${1:-}" = "--docker" ]; then
  shift
  image=$(sed -n 's/^ *image: *\(rust:.*\)/\1/p' .woodpecker/ci.yaml | head -1)
  exec docker run --rm -v "$PWD":/src -w /src -v mh3sp-rs-ci-target:/target \
    -e CARGO_TARGET_DIR=/target "$image" scripts/ci.sh "$@"
fi

if [ "${1:-}" = "--woodpecker" ]; then
  # The local runner mounts the working tree instead of cloning it. Only the checks: rc.yaml and release.yaml build and
  # publish images, and `exec` ignores `depends_on` between workflows.
  exec woodpecker-cli exec --backend-engine docker --local --repo-path "$PWD" .woodpecker/ci.yaml
fi

# The format, the lints as errors and the audit.
run_lint() {
  rustup component add rustfmt clippy >/dev/null 2>&1
  cargo fmt --all --check
  cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
  # The server as shipped (no features): what the `bot` feature gates must leave it clean.
  cargo clippy -p server --all-targets --locked -- -D warnings
  # Known vulnerabilities, licenses, sources (deny.toml).
  command -v cargo-deny >/dev/null || { echo 'cargo-deny missing: cargo install cargo-deny --locked' >&2; exit 1; }
  cargo deny check
}

run_tests() {
  # Event content for the tests: MH3SP's files at the commit the corpus was recorded with. Never in this repository:
  # CONTENT_DIR if set, else the MH3SP checkout next to it when it is at that commit (any other commit fails the
  # content tests), else a clone at that commit in the target folder (from that checkout when there is one: no
  # network).
  MH3SP_COMMIT=f53b06c
  if [ -z "${CONTENT_DIR:-}" ]; then
    at_commit() { [ "$(git -C "$1" rev-parse HEAD 2>/dev/null)" = "$(git -C "$1" rev-parse "$MH3SP_COMMIT^{commit}" 2>/dev/null)" ]; }
    if [ -d ../reference/event ] && at_commit ../reference; then
      CONTENT_DIR=../reference
    else
      CONTENT_DIR="${CARGO_TARGET_DIR:-target}/mh3sp-$MH3SP_COMMIT"
      if [ ! -d "$CONTENT_DIR/event" ]; then
        source=https://github.com/sepalani/MH3SP
        [ -d ../reference/.git ] && source=../reference
        git clone --quiet "$source" "$CONTENT_DIR"
        git -C "$CONTENT_DIR" checkout --quiet "$MH3SP_COMMIT"
      fi
    fi
  fi
  # Absolute: cargo runs each test from its crate's folder. Exported always: the content tests skip without it.
  CONTENT_DIR=$(cd "$CONTENT_DIR" && pwd)
  export CONTENT_DIR

  cargo test --workspace --locked
  # The features' own code (the server's unit tests with each; testkit's tests run with `bot` already).
  cargo test -p server --lib --features prometheus --locked
  cargo test -p server --lib --features bot --locked
}

# One call per line, never `lint && test`: `set -e` is off on the left of `&&`, which would hide a failing lint.
case "${1:-}" in
lint) run_lint ;;
test) run_tests ;;
"")
  run_lint
  run_tests
  ;;
*)
  echo "usage: scripts/ci.sh [--docker] [lint | test] | --woodpecker" >&2
  exit 2
  ;;
esac
