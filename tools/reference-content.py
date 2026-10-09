#!/usr/bin/env python2
"""Reference event quest binaries from an MH3SP checkout's own generator (Python 2, like MH3SP's live branch).

Usage: tools/reference-content.py <mh3sp checkout> <out dir>
Writes <out dir>/<quest file name>.bin for every event quest; the Rust server's must be byte-identical
(`cargo test -p server --lib reference`, with CONTENT_DIR=<checkout> and REFERENCE_DIR=<out dir>).
"""
import json
import os
import sys

checkout, out = os.path.abspath(sys.argv[1]), os.path.abspath(sys.argv[2])
os.chdir(checkout)  # the generator reads `event/...` relative paths
sys.path.insert(0, checkout)
from mh import quest_utils  # noqa: E402

if not os.path.isdir(out):
    os.makedirs(out)
count = 0
for name in sorted(os.listdir("event")):
    if not name.endswith(".json") or name in ("quest_rotation.json", "trading_post.json"):
        continue
    with open(os.path.join("event", name)) as f:
        quest = quest_utils.byteify(json.load(f))
    binary = quest_utils.make_binary_event_quest(quest)
    with open(os.path.join(out, name[:-len(".json")] + ".bin"), "wb") as f:
        f.write(binary)
    count += 1
print("%d quests in %s" % (count, out))
