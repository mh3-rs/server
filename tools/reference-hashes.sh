#!/bin/sh
# Prints, for every event quest of an MH3SP checkout, the SHA-256 of its JSON and of the binary MH3SP's own generator
# makes from it (tools/reference-content.py, Python 2 in Docker). The lines go into
# crates/server/src/content/reference.sha256, which the test `quests_match_the_reference_generator` checks every
# quest of CONTENT_DIR against: hashes only, no content in this repository.
# Usage: tools/reference-hashes.sh <mh3sp checkout> >> crates/server/src/content/reference.sha256 (then sort -u)
set -eu
checkout=$(cd "$1" && pwd)
tools=$(cd "$(dirname "$0")" && pwd)
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
docker run --rm -v "$checkout":/mh3sp:ro -v "$tools":/tools:ro -v "$out":/out python:2.7-slim \
  python2 /tools/reference-content.py /mh3sp /out >&2
for bin in "$out"/*.bin; do
  name=$(basename "$bin" .bin)
  printf '%s %s\n' "$(shasum -a 256 "$checkout/event/$name.json" | cut -c1-64)" "$(shasum -a 256 "$bin" | cut -c1-64)"
done
