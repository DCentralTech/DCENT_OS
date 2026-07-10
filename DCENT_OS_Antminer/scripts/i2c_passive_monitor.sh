#!/bin/sh
# ============================================================================
# I2C PASSIVE REGISTER MONITOR — DCENT_OS
# ============================================================================
# PASSIVE mode: does NOT send any I2C transactions. Only reads AXI IIC
# registers via devmem and tails dcentrald's log for heartbeat events.
# Safe to run alongside dcentrald without bus contention.
#
# This is the preferred diagnostic when dcentrald is actively mining.
# The active version (i2c_heartbeat_capture.sh) should be used when
# dcentrald is STOPPED and you want to probe heartbeats independently.
#
# Usage:  /tmp/i2c_passive_monitor.sh [duration_seconds]
#         Default: 300 seconds (5 minutes)
#
# Output: /tmp/i2c_passive_regs.csv     — AXI IIC register snapshots (10Hz)
#         /tmp/i2c_passive_hb_log.csv   — Heartbeat events from dcentrald log
#         /tmp/i2c_passive_corr.txt     — Correlated timeline
#
# Copyright (C) 2026 D-Central Technologies — GPL-3.0
# ============================================================================

DURATION=${1:-300}

# AXI IIC register addresses
AXI=0x41600000
R_GIE=$((AXI + 0x01C))
R_ISR=$((AXI + 0x020))
R_IER=$((AXI + 0x028))
R_CR=$((AXI + 0x100))
R_SR=$((AXI + 0x104))
R_THIGH=$((AXI + 0x13C))
R_TLOW=$((AXI + 0x140))
R_TBUF=$((AXI + 0x138))

REG_CSV="/tmp/i2c_passive_regs.csv"
HB_LOG="/tmp/i2c_passive_hb_log.csv"
CORR="/tmp/i2c_passive_corr.txt"

read_reg() { devmem "$1" 32 2>/dev/null || echo "0xDEAD"; }
get_ts() { read up idle < /proc/uptime; echo "$up"; }

# ============================================================================
# Thread 1: Register snapshots at 10 Hz (100ms)
# ============================================================================
monitor_regs() {
    echo "tick,uptime,CR,SR,ISR,GIE,IER,THIGH,TLOW,TBUF" > "$REG_CSV"
    local tick=0
    local end=$(($(date +%s) + DURATION))
    local prev_cr="" prev_sr="" prev_gie="" prev_thigh=""

    while [ "$(date +%s)" -lt "$end" ]; do
        tick=$((tick + 1))
        local ts=$(get_ts)
        local cr=$(read_reg $R_CR)
        local sr=$(read_reg $R_SR)
        local isr=$(read_reg $R_ISR)
        local gie=$(read_reg $R_GIE)
        local ier=$(read_reg $R_IER)
        local thigh=$(read_reg $R_THIGH)
        local tlow=$(read_reg $R_TLOW)
        local tbuf=$(read_reg $R_TBUF)

        echo "${tick},${ts},${cr},${sr},${isr},${gie},${ier},${thigh},${tlow},${tbuf}" >> "$REG_CSV"

        # Detect state CHANGES (not just anomalies)
        if [ -n "$prev_cr" ] && [ "$cr" != "$prev_cr" ]; then
            echo "REG_CHANGE @ ${ts}: CR ${prev_cr} -> ${cr}" >&2
        fi
        if [ -n "$prev_sr" ]; then
            # SR changes frequently during I2C transactions (BB toggles), but
            # persistent BB=1 for >500ms is a stuck state
            local sr_val=$((${sr}))
            if [ $((sr_val & 0x04)) -ne 0 ]; then
                # Bus busy — check if it stays busy
                echo "BB=1 @ ${ts} SR=${sr}" >&2
            fi
        fi
        if [ -n "$prev_gie" ] && [ "$gie" != "$prev_gie" ]; then
            echo "REG_CHANGE @ ${ts}: GIE ${prev_gie} -> ${gie}" >&2
        fi
        if [ -n "$prev_thigh" ] && [ "$thigh" != "$prev_thigh" ]; then
            echo "REG_CHANGE @ ${ts}: THIGH ${prev_thigh} -> ${thigh}" >&2
        fi

        prev_cr="$cr"; prev_sr="$sr"; prev_gie="$gie"; prev_thigh="$thigh"

        usleep 100000 2>/dev/null || sleep 0
    done
    echo "[PASSIVE_REG] Done: ${tick} samples" >&2
}

# ============================================================================
# Thread 2: Scrape dcentrald log for heartbeat events
# ============================================================================
monitor_log() {
    echo "uptime,event,pic_addr,result,details" > "$HB_LOG"
    local end=$(($(date +%s) + DURATION))

    # Tail dcentrald log and extract heartbeat-related lines
    tail -f /tmp/dcentrald.log 2>/dev/null | while IFS= read -r line; do
        [ "$(date +%s)" -ge "$end" ] && break

        local ts=$(get_ts)

        # Match DIAG_HB lines (dcentrald's heartbeat diagnostic output)
        case "$line" in
            *DIAG_HB*OK*)
                local addr=$(echo "$line" | sed -n 's/.*PIC=\(0x[0-9A-Fa-f]*\).*/\1/p')
                local us=$(echo "$line" | sed -n 's/.*us=\([0-9]*\).*/\1/p')
                local tk=$(echo "$line" | sed -n 's/.*tick=\([0-9]*\).*/\1/p')
                echo "${ts},HB_OK,${addr},OK,tick=${tk}_us=${us}" >> "$HB_LOG"
                ;;
            *DIAG_HB*FAIL*)
                local addr=$(echo "$line" | sed -n 's/.*PIC=\(0x[0-9A-Fa-f]*\).*/\1/p')
                local us=$(echo "$line" | sed -n 's/.*us=\([0-9]*\).*/\1/p')
                local fails=$(echo "$line" | sed -n 's/.*consecutive=\([0-9]*\).*/\1/p')
                echo "${ts},HB_FAIL,${addr},FAIL,consecutive=${fails}_us=${us}" >> "$HB_LOG"
                ;;
            *DIAG_I2C_WRITE*NACK*)
                local addr=$(echo "$line" | sed -n 's/.*addr=\(0x[0-9A-Fa-f]*\).*/\1/p')
                local isr=$(echo "$line" | sed -n 's/.*ISR=\(0x[0-9A-Fa-f]*\).*/\1/p')
                local sr=$(echo "$line" | sed -n 's/.*SR=\(0x[0-9A-Fa-f]*\).*/\1/p')
                echo "${ts},I2C_NACK,${addr},NACK,ISR=${isr}_SR=${sr}" >> "$HB_LOG"
                ;;
            *heartbeat*recovered*)
                local addr=$(echo "$line" | sed -n 's/.*PIC 0x\([0-9A-Fa-f]*\).*/0x\1/p')
                echo "${ts},HB_RECOVER,${addr},OK,recovered" >> "$HB_LOG"
                ;;
            *heartbeat*failed*10*)
                local addr=$(echo "$line" | sed -n 's/.*PIC 0x\([0-9A-Fa-f]*\).*/0x\1/p')
                echo "${ts},HB_DEAD,${addr},DEAD,10_consecutive" >> "$HB_LOG"
                ;;
            *AXI*IIC*reset*)
                echo "${ts},AXI_RESET,N/A,RESET,${line}" >> "$HB_LOG"
                ;;
            *bus.*stuck*)
                echo "${ts},BUS_STUCK,N/A,STUCK,${line}" >> "$HB_LOG"
                ;;
        esac
    done &
    local TAIL_PID=$!

    # Wait for duration then kill the tail
    sleep $DURATION
    kill $TAIL_PID 2>/dev/null
    echo "[PASSIVE_LOG] Done scraping dcentrald log" >&2
}

# ============================================================================
# CORRELATION: Merge register snapshots with heartbeat events
# ============================================================================
correlate() {
    echo "=== I2C HEARTBEAT FAILURE CORRELATION ===" > "$CORR"
    echo "Generated: $(date)" >> "$CORR"
    echo "" >> "$CORR"

    if [ ! -f "$HB_LOG" ] || [ ! -f "$REG_CSV" ]; then
        echo "Missing data files — cannot correlate" >> "$CORR"
        return
    fi

    # Find first heartbeat failure event
    local first_fail_line=$(grep "HB_FAIL\|HB_DEAD\|I2C_NACK" "$HB_LOG" | head -1)
    if [ -z "$first_fail_line" ]; then
        echo "NO HEARTBEAT FAILURES DETECTED during ${DURATION}s capture." >> "$CORR"
        echo "All heartbeats succeeded. The bug did not reproduce." >> "$CORR"
        echo "" >> "$CORR"
        echo "Try: longer capture, or trigger by adding I2C load (temp sensor reads)" >> "$CORR"
        return
    fi

    local fail_time=$(echo "$first_fail_line" | cut -d',' -f1)
    echo "FIRST HEARTBEAT FAILURE at uptime=${fail_time}s" >> "$CORR"
    echo "Event: $first_fail_line" >> "$CORR"
    echo "" >> "$CORR"

    # Find register snapshot closest to failure time
    # (Register CSV has uptime in column 2)
    echo "--- Register state AROUND failure time (${fail_time}s) ---" >> "$CORR"
    echo "" >> "$CORR"

    # Get the integer part of fail_time for matching
    local fail_int=$(echo "$fail_time" | cut -d'.' -f1)
    local before=$((fail_int - 2))
    local after=$((fail_int + 2))

    # Header
    echo "tick | uptime | CR       | SR       | ISR      | GIE      | IER      | THIGH    | TLOW     | TBUF" >> "$CORR"
    echo "---- | ------ | -------- | -------- | -------- | -------- | -------- | -------- | -------- | --------" >> "$CORR"

    # Find register snapshots within +/- 2 seconds of failure
    while IFS=',' read -r tick ts cr sr isr gie ier thigh tlow tbuf; do
        [ "$tick" = "tick" ] && continue  # skip header
        local ts_int=$(echo "$ts" | cut -d'.' -f1)
        if [ "$ts_int" -ge "$before" ] && [ "$ts_int" -le "$after" ] 2>/dev/null; then
            printf "%4s | %6s | %8s | %8s | %8s | %8s | %8s | %8s | %8s | %8s\n" \
                "$tick" "$ts" "$cr" "$sr" "$isr" "$gie" "$ier" "$thigh" "$tlow" "$tbuf" >> "$CORR"
        fi
    done < "$REG_CSV"

    echo "" >> "$CORR"

    # All failure events
    echo "--- All heartbeat failure events ---" >> "$CORR"
    grep "HB_FAIL\|HB_DEAD\|I2C_NACK\|BUS_STUCK\|AXI_RESET" "$HB_LOG" >> "$CORR"
    echo "" >> "$CORR"

    # Key questions answered
    echo "=== DIAGNOSTIC QUESTIONS ===" >> "$CORR"
    echo "" >> "$CORR"

    # Q1: Did timing registers get zeroed?
    if grep -q "0x00000000.*0x00000000" "$REG_CSV" 2>/dev/null; then
        echo "Q1: THIGH/TLOW zeroed? YES — SOFTR happened without timing restore" >> "$CORR"
    else
        echo "Q1: THIGH/TLOW zeroed? NO — timing registers stayed intact" >> "$CORR"
    fi

    # Q2: Did GIE toggle?
    local gie_changes=$(grep "GIE" /tmp/i2c_diag_anomalies.log 2>/dev/null | wc -l)
    echo "Q2: GIE toggled? ${gie_changes} times" >> "$CORR"

    # Q3: Did bus go stuck-busy?
    local bb_count=$(grep "BB=1" /tmp/i2c_passive_regs_anomalies.log 2>/dev/null | wc -l)
    echo "Q3: Bus-busy events? ${bb_count}" >> "$CORR"

    # Q4: Did CR get disabled?
    local cr_disabled=$(grep "CR.*0x00000000" "$REG_CSV" 2>/dev/null | wc -l)
    echo "Q4: CR disabled (EN=0)? ${cr_disabled} times" >> "$CORR"

    echo "" >> "$CORR"
    echo "=== RAW DATA FILES ===" >> "$CORR"
    echo "  ${REG_CSV} — register snapshots ($(wc -l < "$REG_CSV" 2>/dev/null) lines)" >> "$CORR"
    echo "  ${HB_LOG} — heartbeat events ($(wc -l < "$HB_LOG" 2>/dev/null) lines)" >> "$CORR"
}

# ============================================================================
# MAIN
# ============================================================================
echo "=============================================="
echo " DCENT_OS I2C PASSIVE Monitor"
echo " Duration: ${DURATION}s  (register reads only, NO I2C writes)"
echo " Safe to run alongside dcentrald"
echo "=============================================="

if ! pidof dcentrald >/dev/null 2>&1; then
    echo ""
    echo "WARNING: dcentrald is NOT running. No heartbeat log to scrape."
    echo "         Use i2c_heartbeat_capture.sh for active probing instead."
    echo ""
fi

# Baseline
echo ""
echo "=== BASELINE ==="
echo "CR:    $(read_reg $R_CR)"
echo "SR:    $(read_reg $R_SR)"
echo "ISR:   $(read_reg $R_ISR)"
echo "GIE:   $(read_reg $R_GIE)"
echo "IER:   $(read_reg $R_IER)"
echo "THIGH: $(read_reg $R_THIGH)"
echo "TLOW:  $(read_reg $R_TLOW)"
echo "TBUF:  $(read_reg $R_TBUF)"
echo ""

# Launch threads
monitor_regs 2>/tmp/i2c_passive_regs_anomalies.log &
REG_PID=$!

monitor_log &
LOG_PID=$!

echo "Monitoring: REG_PID=$REG_PID LOG_PID=$LOG_PID"
echo "  Register anomalies: tail -f /tmp/i2c_passive_regs_anomalies.log"
echo ""

wait $REG_PID 2>/dev/null
wait $LOG_PID 2>/dev/null

echo ""
echo "Capture complete. Correlating..."
correlate

echo ""
cat "$CORR"

echo ""
echo "=== FILES ==="
echo "  ${REG_CSV}"
echo "  ${HB_LOG}"
echo "  ${CORR}"
