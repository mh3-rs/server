#!/bin/sh
# (Re)start the reference server, Python MH3SP, on a fresh database, so recordings and replays
# all start from the same server state. Returns once it listens.
# Env: LAB (default ../mh3sp/lab); LOG (default $LAB/server.log)
set -eu
cd "$(dirname "$0")/.."
LAB=$(cd "${LAB:-../mh3sp/lab}" && pwd)
MH3SP=$(dirname "$LAB")
LOG=${LOG:-$LAB/server.log}

pkill -f 'master_server.py -- --log-to-window' || true
while pgrep -f 'master_server.py -- --log-to-window' >/dev/null; do sleep 0.2; done
rm -f "$MH3SP/mh3sp.db"
# Python MH3SP doesn't set SO_REUSEADDR: right after a restart its ports can still be held
# by the OS, and it exits with "Address already in use". Retry until it binds.
# The server must not inherit our stdout/stderr, or whoever reads them would wait for it to exit.
until lsof -nP -iTCP:8202 -sTCP:LISTEN >/dev/null; do
  if ! pgrep -f 'master_server.py -- --log-to-window' >/dev/null; then
    nohup "$LAB/server.sh" > "$LOG" 2>&1 < /dev/null &
  fi
  sleep 0.5
done
echo "MH3SP $(git -C "$MH3SP" rev-parse --short HEAD) running on a fresh database"
