#!/bin/sh
set -u

LOGFILE="/tmp/dcentrald.latest_mining_probe.log"
CONFIG="/tmp/dcentrald.latest_canary_modern.toml"
BIN="/tmp/dcentrald.latest_probe"

cleanup_latest_probe() {
    for PID in $(ps | grep latest_probe | grep -v grep | awk '{print $1}'); do
        kill -9 "$PID" 2>/dev/null || true
    done
}

echo "[probe] stopping managed dcentrald"
/etc/init.d/S82dcentrald stop || true
sleep 2
cleanup_latest_probe
rm -f "$LOGFILE"

echo "[probe] starting latest mining canary"
"$BIN" --config "$CONFIG" >"$LOGFILE" 2>&1 &
PID=$!
echo "START_PID=$PID"

sleep 35

echo "[probe] requesting graceful shutdown"
kill -TERM "$PID" 2>/dev/null || true
wait "$PID"
CODE=$?

echo "EXIT_CODE=$CODE"
echo "--- LOG ---"
tail -n 180 "$LOGFILE"

echo "[probe] restarting managed dcentrald"
/etc/init.d/S82dcentrald start || true
