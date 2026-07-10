#!/usr/bin/env python3
"""UART serial monitor for S9 boot debugging. Logs to file + stdout."""
import serial
import sys
import time
import os

PORT = sys.argv[1] if len(sys.argv) > 1 else "COM7"
BAUD = int(sys.argv[2]) if len(sys.argv) > 2 else 115200
LOG_FILE = sys.argv[3] if len(sys.argv) > 3 else f"boot_log_{time.strftime('%Y%m%d_%H%M%S')}.txt"

print(f"Opening {PORT} @ {BAUD} baud...")
print(f"Logging to: {LOG_FILE}")
print(f"Press Ctrl+C to stop.\n{'='*60}")

try:
    ser = serial.Serial(PORT, BAUD, timeout=0.1)
except serial.SerialException as e:
    print(f"ERROR: Cannot open {PORT}: {e}")
    print("Is the cable connected? Is another program using the port?")
    sys.exit(1)

with open(LOG_FILE, "w", encoding="utf-8", errors="replace") as log:
    log.write(f"# Serial capture: {PORT} @ {BAUD}\n")
    log.write(f"# Started: {time.strftime('%Y-%m-%d %H:%M:%S')}\n\n")
    try:
        while True:
            data = ser.read(4096)
            if data:
                text = data.decode("utf-8", errors="replace")
                sys.stdout.write(text)
                sys.stdout.flush()
                log.write(text)
                log.flush()
    except KeyboardInterrupt:
        print(f"\n{'='*60}")
        print(f"Capture saved to: {LOG_FILE}")
    finally:
        ser.close()
