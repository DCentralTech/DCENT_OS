#!/usr/bin/env python3
"""
Serial console recovery for bricked S9 miners.

Connects to U-Boot serial console, interrupts boot, switches firmware slot.

Usage:
  python serial_recover.py [PORT] [ACTION]

  PORT    = COM7 (default)
  ACTION  = diagnose | switch_slot | shell

  diagnose     - Capture boot log only (no commands sent)
  switch_slot  - Interrupt U-Boot, set firmware=1, saveenv, reset
  shell        - Interactive serial shell (for manual recovery)
"""
import serial
import sys
import time
import threading
import os

PORT = "COM7"
BAUD = 115200
ACTION = "diagnose"

for arg in sys.argv[1:]:
    if arg.startswith("COM") or arg.startswith("/dev/"):
        PORT = arg
    elif arg in ("diagnose", "switch_slot", "shell"):
        ACTION = arg

LOG_FILE = f"recovery_{time.strftime('%Y%m%d_%H%M%S')}.txt"


def log_and_print(log, text):
    sys.stdout.write(text)
    sys.stdout.flush()
    log.write(text)
    log.flush()


def read_until(ser, log, pattern, timeout=30):
    buf = ""
    start = time.time()
    while time.time() - start < timeout:
        data = ser.read(1024)
        if data:
            text = data.decode("utf-8", errors="replace")
            log_and_print(log, text)
            buf += text
            if pattern in buf:
                return buf
    return buf


def send_cmd(ser, log, cmd, delay=0.5):
    log_and_print(log, f"\n>>> SENDING: {cmd}\n")
    ser.write((cmd + "\n").encode())
    time.sleep(delay)
    data = ser.read(4096)
    if data:
        text = data.decode("utf-8", errors="replace")
        log_and_print(log, text)
        return text
    return ""


def diagnose(ser, log):
    log_and_print(log, "\n=== DIAGNOSE MODE ===\n")
    log_and_print(log, "Power cycle the miner now. Capturing boot log...\n")
    log_and_print(log, "Press Ctrl+C to stop.\n\n")
    try:
        while True:
            data = ser.read(4096)
            if data:
                text = data.decode("utf-8", errors="replace")
                log_and_print(log, text)
    except KeyboardInterrupt:
        pass


def switch_slot(ser, log):
    log_and_print(log, "\n=== SWITCH SLOT MODE ===\n")
    log_and_print(log, "Will interrupt U-Boot and switch to firmware=1 (BraiinsOS).\n")
    log_and_print(log, "Power cycle the miner NOW...\n\n")

    # Spam interrupt chars to catch U-Boot bootdelay
    log_and_print(log, "Sending interrupt characters...\n")
    start = time.time()
    caught = False
    buf = ""

    while time.time() - start < 60:
        # Send multiple interrupt chars (space, newline, escape)
        ser.write(b" \n \n \n")
        time.sleep(0.05)

        data = ser.read(4096)
        if data:
            text = data.decode("utf-8", errors="replace")
            log_and_print(log, text)
            buf += text

            if "Zynq>" in buf or "zynq>" in buf or "U-Boot>" in buf or "=>" in buf:
                caught = True
                log_and_print(log, "\n\n*** U-BOOT PROMPT CAUGHT! ***\n\n")
                break

            if "autoboot" in buf.lower() or "hit any key" in buf.lower():
                ser.write(b"\n\n\n\n\n")
                time.sleep(0.1)

    if not caught:
        log_and_print(log, "\nFailed to catch U-Boot prompt within 60s.\n")
        log_and_print(log, "Check: bootdelay may be 0, or serial TX wiring may be wrong.\n")
        return False

    time.sleep(0.5)
    ser.read(4096)  # flush

    # Print current env
    log_and_print(log, "\n--- Reading current firmware variable ---\n")
    send_cmd(ser, log, "printenv firmware", delay=1.0)

    # Switch to firmware 1 (BraiinsOS)
    log_and_print(log, "\n--- Setting firmware=1 ---\n")
    send_cmd(ser, log, "setenv firmware 1", delay=0.5)

    # Clear upgrade_stage and first_boot
    send_cmd(ser, log, "setenv upgrade_stage", delay=0.5)
    send_cmd(ser, log, "setenv first_boot", delay=0.5)

    # Save environment
    log_and_print(log, "\n--- Saving environment ---\n")
    resp = send_cmd(ser, log, "saveenv", delay=3.0)

    # Verify
    log_and_print(log, "\n--- Verifying ---\n")
    send_cmd(ser, log, "printenv firmware", delay=1.0)

    # Reset
    log_and_print(log, "\n--- Resetting (booting firmware 1 / BraiinsOS) ---\n")
    send_cmd(ser, log, "reset", delay=1.0)

    # Capture boot output for 30s
    log_and_print(log, "\n--- Capturing boot log (30s) ---\n")
    boot_start = time.time()
    while time.time() - boot_start < 30:
        data = ser.read(4096)
        if data:
            text = data.decode("utf-8", errors="replace")
            log_and_print(log, text)

    log_and_print(log, "\n\n=== RECOVERY COMPLETE ===\n")
    log_and_print(log, "If BraiinsOS booted, the miner should be reachable via SSH.\n")
    log_and_print(log, "Check: ping 203.0.113.39\n")
    return True


def interactive_shell(ser, log):
    log_and_print(log, "\n=== INTERACTIVE SHELL ===\n")
    log_and_print(log, "Type commands. Ctrl+C to exit.\n\n")

    def reader():
        try:
            while True:
                data = ser.read(1024)
                if data:
                    text = data.decode("utf-8", errors="replace")
                    log_and_print(log, text)
        except Exception:
            pass

    t = threading.Thread(target=reader, daemon=True)
    t.start()

    try:
        while True:
            cmd = input()
            ser.write((cmd + "\n").encode())
            time.sleep(0.1)
    except (KeyboardInterrupt, EOFError):
        pass


print(f"Serial Recovery Tool — {PORT} @ {BAUD}")
print(f"Action: {ACTION}")
print(f"Log: {LOG_FILE}")
print("=" * 60)

try:
    ser = serial.Serial(PORT, BAUD, timeout=0.1)
except serial.SerialException as e:
    print(f"ERROR: Cannot open {PORT}: {e}")
    sys.exit(1)

with open(LOG_FILE, "w", encoding="utf-8", errors="replace") as log:
    log.write(f"# Serial recovery: {PORT} @ {BAUD}, action={ACTION}\n")
    log.write(f"# Started: {time.strftime('%Y-%m-%d %H:%M:%S')}\n\n")

    if ACTION == "diagnose":
        diagnose(ser, log)
    elif ACTION == "switch_slot":
        switch_slot(ser, log)
    elif ACTION == "shell":
        interactive_shell(ser, log)

    ser.close()
    print(f"\nLog saved: {LOG_FILE}")
