#!/bin/sh
# Record a corpus scenario with the lab: restarts Python MH3SP on a fresh database, boots
# the consoles in Dolphin, waits until every Dolphin window is closed, then imports the
# captures into corpus/<scenario>.
#
# Usage: tools/record-scenario.sh <scenario> [devN ...]   (default: dev1)
# dev1 is the lab's original profile; other devN are cloned from it without its console
# identity and network file, so a new devN behaves like a console going online for the first time.
# Env:   LAB (default ../mh3sp/lab), MH3_GAME (default /Volumes/USB/mh3/rmhp08.rvz)
set -eu
cd "$(dirname "$0")/.."
[ $# -ge 1 ] || { echo "usage: $0 <scenario> [devN ...]"; exit 1; }
SCENARIO=$1
shift
[ $# -gt 0 ] || set -- dev1
CONSOLES=$(for c in "$@"; do echo "${c#dev}"; done)
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
for i in $CONSOLES; do
  dolphin_running $i && { echo "dev$i's Dolphin is already running: close it first"; exit 1; }
done

# Python MH3SP on a fresh database, so every recording starts from the same server state.
pkill -f 'master_server.py -- --log-to-window' || true
while pgrep -f 'master_server.py -- --log-to-window' >/dev/null; do sleep 1; done
rm -f "$MH3SP/mh3sp.db"
nohup "$LAB/server.sh" > "$LAB/server-$SCENARIO.log" 2>&1 &
until lsof -nP -iTCP:8202 -sTCP:LISTEN >/dev/null; do sleep 1; done
echo "MH3SP $(git -C "$MH3SP" rev-parse --short HEAD) running on a fresh database"

marker=$(mktemp)
for i in $CONSOLES; do
  nohup "$LAB/play.sh" "$GAME" "$(profile $i)" "$(mac $i)" > "$LAB/dolphin-dev$i.log" 2>&1 &
  echo "dev$i booting ($(profile $i))"
done

echo "Play '$SCENARIO', then close every Dolphin window."
sleep 5
for i in $CONSOLES; do
  while dolphin_running $i; do sleep 2; done
done

# One new capture per console.
set --
for i in $CONSOLES; do
  captures=$(find "$LAB/$(profile $i)/Dump/SSL" -name '*.pcap' -newer "$marker")
  [ "$(echo "$captures" | grep -c .)" = 1 ] || { echo "dev$i: expected one new capture, got: $captures"; exit 1; }
  set -- "$@" "dev$i=$captures"
done
rm -f "$marker"

cargo run -q -p testkit --bin corpus -- import "corpus/$SCENARIO" "$@"

if [ ! -f "corpus/$SCENARIO/scenario.toml" ]; then
  cat > "corpus/$SCENARIO/scenario.toml" <<EOF
description = ""
steps = []
consoles = $(echo $CONSOLES | wc -w | tr -d " ")
oracle = "sepalani/MH3SP $(git -C "$MH3SP" rev-parse --short HEAD)"
recorded = "$(date +%F)"
# Milestone from which the Rust server must replay this scenario cleanly.
milestone = ""
EOF
  echo "fill in corpus/$SCENARIO/scenario.toml"
fi
