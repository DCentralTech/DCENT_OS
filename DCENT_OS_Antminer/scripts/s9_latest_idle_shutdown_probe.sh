#!/bin/sh
set -eu

LOGFILE="/tmp/dcentrald.latest_idle.log"
CONFIG="/tmp/dcentrald.latest_idle.toml"
BIN="/tmp/dcentrald.latest_probe"

# Clean up any prior probe instances so ports 8081/4029 are free.
for PID in $(ps | grep latest_probe | grep -v grep | awk '{print $1}'); do
    kill -9 "$PID" 2>/dev/null || true
done

rm -f "$LOGFILE"

"$BIN" --config "$CONFIG" >"$LOGFILE" 2>&1 &
PID=$!
echo "START_PID=$PID"

sleep 6

kill -TERM "$PID" 2>/dev/null || true
wait "$PID"
CODE=$?
echo "EXIT_CODE=$CODE"
echo "--- LOG ---"
tail -n 120 "$LOGFILE"
