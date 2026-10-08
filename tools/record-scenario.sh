#!/bin/sh
# Record a corpus scenario with the lab: restarts Python MH3SP on a fresh database, boots
# the consoles in Dolphin, waits until every Dolphin window is closed, then imports the
# captures into corpus/<scenario>.
#
# Usage: tools/record-scenario.sh <scenario> [consoles, default 1]
# Env:   LAB (default ../mh3sp/lab), MH3_GAME (default /Volumes/USB/mh3/rmhp08.rvz)
set -eu
cd "$(dirname "$0")/.."
[ $# -ge 1 ] || { echo "usage: $0 <scenario> [consoles]"; exit 1; }
SCENARIO=$1
CONSOLES=${2:-1}
LAB=$(cd "${LAB:-../mh3sp/lab}" && pwd)
MH3SP=$(dirname "$LAB")
GAME=${MH3_GAME:-/Volumes/USB/mh3/rmhp08.rvz}

# devN -> lab profile and MAC (dev1 uses the original profile, others are clones).
profile() { [ "$1" = 1 ] && echo dolphin-user || echo "p$1"; }
mac() { printf '00:17:ab:b3:3b:%02x' $((0x2a + $1)); }
dolphin_running() { pgrep -f "Dolphin -u $LAB/$(profile "$1") " >/dev/null; }

# Preconditions.
grep -q '# >>> mh3sp' /etc/hosts || { echo "hosts redirection is off: sudo $LAB/hosts.sh on"; exit 1; }
lsof -nP -iUDP:27901 >/dev/null || { echo "AltWFC is not running: $LAB/altwfc.sh"; exit 1; }
[ -f "$GAME" ] || { echo "game image not found: $GAME"; exit 1; }
i=1; while [ $i -le "$CONSOLES" ]; do
  dolphin_running $i && { echo "dev$i's Dolphin is already running: close it first"; exit 1; }
  i=$((i + 1))
done

# Python MH3SP on a fresh database, so every recording starts from the same server state.
pkill -f 'master_server.py -- --log-to-window' || true
while pgrep -f 'master_server.py -- --log-to-window' >/dev/null; do sleep 1; done
rm -f "$MH3SP/mh3sp.db"
nohup "$LAB/server.sh" > "$LAB/server-$SCENARIO.log" 2>&1 &
until lsof -nP -iTCP:8202 -sTCP:LISTEN >/dev/null; do sleep 1; done
echo "MH3SP $(git -C "$MH3SP" rev-parse --short HEAD) running on a fresh database"

marker=$(mktemp)
i=1; while [ $i -le "$CONSOLES" ]; do
  nohup "$LAB/play.sh" "$GAME" "$(profile $i)" "$(mac $i)" > "$LAB/dolphin-dev$i.log" 2>&1 &
  echo "dev$i booting ($(profile $i))"
  i=$((i + 1))
done

echo "Play '$SCENARIO', then close every Dolphin window."
sleep 5
i=1; while [ $i -le "$CONSOLES" ]; do
  while dolphin_running $i; do sleep 2; done
  i=$((i + 1))
done

# One new capture per console.
set --
i=1; while [ $i -le "$CONSOLES" ]; do
  captures=$(find "$LAB/$(profile $i)/Dump/SSL" -name '*.pcap' -newer "$marker")
  [ "$(echo "$captures" | grep -c .)" = 1 ] || { echo "dev$i: expected one new capture, got: $captures"; exit 1; }
  set -- "$@" "dev$i=$captures"
  i=$((i + 1))
done
rm -f "$marker"

cargo run -q -p testkit --bin corpus -- import "corpus/$SCENARIO" "$@"

if [ ! -f "corpus/$SCENARIO/scenario.toml" ]; then
  cat > "corpus/$SCENARIO/scenario.toml" <<EOF
description = ""
steps = []
consoles = $CONSOLES
oracle = "sepalani/MH3SP $(git -C "$MH3SP" rev-parse --short HEAD)"
recorded = "$(date +%F)"
# Milestone from which the Rust server must replay this scenario cleanly.
milestone = ""
EOF
  echo "fill in corpus/$SCENARIO/scenario.toml"
fi
