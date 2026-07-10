#!/bin/sh
# ============================================================================
# I2C HEARTBEAT DIAGNOSTIC CAPTURE — DCENT_OS
# ============================================================================
# Captures AXI IIC register state + PIC heartbeat success/fail simultaneously
# during boot. Correlates the exact moment heartbeats start failing with
# the I2C controller register values.
#
# Deploy: scp this to miner, run BEFORE dcentrald starts (or alongside it).
# Usage:  /tmp/i2c_heartbeat_capture.sh [duration_seconds] [interval_ms]
#         Default: 120 seconds, 100ms interval
#
# Output: /tmp/i2c_diag_regs.csv    — AXI IIC register snapshots
#         /tmp/i2c_diag_hb.csv      — PIC heartbeat results per tick
#         /tmp/i2c_diag_readback.csv — PIC read-back test results
#         /tmp/i2c_diag_summary.txt  — Human-readable summary
#
# Copyright (C) 2026 D-Central Technologies — GPL-3.0
# ============================================================================

DURATION=${1:-120}
INTERVAL_US=${2:-100000}  # 100ms in microseconds (usleep units)

# AXI IIC base address (Xilinx axi_iic IP on S9 Zynq)
AXI_BASE=0x41600000

# Register addresses (base + offset)
REG_GIE=$((AXI_BASE + 0x01C))
REG_ISR=$((AXI_BASE + 0x020))
REG_IER=$((AXI_BASE + 0x028))
REG_SOFTR=$((AXI_BASE + 0x040))
REG_CR=$((AXI_BASE + 0x100))
REG_SR=$((AXI_BASE + 0x104))
REG_TX_FIFO=$((AXI_BASE + 0x108))
REG_RX_FIFO=$((AXI_BASE + 0x10C))
REG_TSUSTA=$((AXI_BASE + 0x128))
REG_TSUSTO=$((AXI_BASE + 0x12C))
REG_THDSTA=$((AXI_BASE + 0x130))
REG_TSUDAT=$((AXI_BASE + 0x134))
REG_TBUF=$((AXI_BASE + 0x138))
REG_THIGH=$((AXI_BASE + 0x13C))
REG_TLOW=$((AXI_BASE + 0x140))
REG_THDDAT=$((AXI_BASE + 0x144))

# PIC I2C addresses
PIC_ADDRS="0x55 0x56 0x57"

# Output files
REG_CSV="/tmp/i2c_diag_regs.csv"
HB_CSV="/tmp/i2c_diag_hb.csv"
RB_CSV="/tmp/i2c_diag_readback.csv"
SUMMARY="/tmp/i2c_diag_summary.txt"

# ============================================================================
# HELPER: Read a 32-bit register via devmem
# ============================================================================
read_reg() {
    devmem "$1" 32 2>/dev/null || echo "0xDEAD"
}

# ============================================================================
# HELPER: Get uptime in centiseconds for high-resolution timestamps
# ============================================================================
get_ts() {
    # /proc/uptime gives seconds.centiseconds — more reliable than date on BusyBox
    read up idle < /proc/uptime
    echo "$up"
}

# ============================================================================
# HELPER: Decode SR register bits
# ============================================================================
decode_sr() {
    local sr=$((${1}))
    local bb=$(( (sr >> 2) & 1 ))
    local aas=$(( (sr >> 1) & 1 ))
    local rx_empty=$(( (sr >> 6) & 1 ))
    local tx_empty=$(( (sr >> 7) & 1 ))
    local srw=$(( (sr >> 3) & 1 ))
    local tx_full=$(( (sr >> 4) & 1 ))
    local rx_full=$(( (sr >> 5) & 1 ))
    echo "BB=${bb},AAS=${aas},SRW=${srw},TX_FULL=${tx_full},RX_FULL=${rx_full},RX_EMPTY=${rx_empty},TX_EMPTY=${tx_empty}"
}

# ============================================================================
# HELPER: Decode CR register bits
# ============================================================================
decode_cr() {
    local cr=$((${1}))
    local en=$(( cr & 1 ))
    local tx_fifo_rst=$(( (cr >> 1) & 1 ))
    local msms=$(( (cr >> 2) & 1 ))
    local tx=$(( (cr >> 3) & 1 ))
    local txak=$(( (cr >> 4) & 1 ))
    local rsta=$(( (cr >> 5) & 1 ))
    local gc_en=$(( (cr >> 6) & 1 ))
    echo "EN=${en},TX_RST=${tx_fifo_rst},MSMS=${msms},TX=${tx},TXAK=${txak},RSTA=${rsta},GC=${gc_en}"
}

# ============================================================================
# HELPER: Decode ISR register bits
# ============================================================================
decode_isr() {
    local isr=$((${1}))
    local arb=$(( isr & 1 ))
    local tx_err=$(( (isr >> 1) & 1 ))
    local tx_empty=$(( (isr >> 2) & 1 ))
    local rx_full=$(( (isr >> 3) & 1 ))
    local bus_not_busy=$(( (isr >> 4) & 1 ))
    local aas=$(( (isr >> 5) & 1 ))
    local naddr_slave=$(( (isr >> 6) & 1 ))
    local tx_half_empty=$(( (isr >> 7) & 1 ))
    echo "ARB=${arb},TX_ERR=${tx_err},TX_EMPTY=${tx_empty},RX_FULL=${rx_full},BNB=${bus_not_busy},AAS=${aas}"
}

# ============================================================================
# PHASE 1: Register Snapshot Capture (background)
# Captures AXI IIC registers every INTERVAL_US microseconds
# ============================================================================
capture_registers() {
    echo "tick,uptime,CR,SR,ISR,GIE,IER,THIGH,TLOW,TBUF,TSUSTA,TSUSTO,THDSTA,TSUDAT,THDDAT,SR_decode,CR_decode,ISR_decode" > "$REG_CSV"

    local tick=0
    local end_time=$(($(date +%s) + DURATION))

    while [ "$(date +%s)" -lt "$end_time" ]; do
        tick=$((tick + 1))
        local ts=$(get_ts)

        # Read all registers in a burst (minimize time skew between reads)
        local cr=$(read_reg $REG_CR)
        local sr=$(read_reg $REG_SR)
        local isr=$(read_reg $REG_ISR)
        local gie=$(read_reg $REG_GIE)
        local ier=$(read_reg $REG_IER)
        local thigh=$(read_reg $REG_THIGH)
        local tlow=$(read_reg $REG_TLOW)
        local tbuf=$(read_reg $REG_TBUF)
        local tsusta=$(read_reg $REG_TSUSTA)
        local tsusto=$(read_reg $REG_TSUSTO)
        local thdsta=$(read_reg $REG_THDSTA)
        local tsudat=$(read_reg $REG_TSUDAT)
        local thddat=$(read_reg $REG_THDDAT)

        # Decode bit fields
        local sr_dec=$(decode_sr "$sr")
        local cr_dec=$(decode_cr "$cr")
        local isr_dec=$(decode_isr "$isr")

        echo "${tick},${ts},${cr},${sr},${isr},${gie},${ier},${thigh},${tlow},${tbuf},${tsusta},${tsusto},${thdsta},${tsudat},${thddat},${sr_dec},${cr_dec},${isr_dec}" >> "$REG_CSV"

        # Detect anomalies in real-time and log to stderr
        # Check: timing registers zeroed (SOFTR happened without restore)
        if [ "$thigh" = "0x00000000" ] || [ "$tlow" = "0x00000000" ]; then
            echo "[ANOMALY @ ${ts}] THIGH/TLOW ZEROED: THIGH=${thigh} TLOW=${tlow} — SOFTR without timing restore!" >&2
        fi
        # Check: GIE toggled (kernel driver interference or devmem conflict)
        if [ "$gie" = "0x00000000" ]; then
            echo "[ANOMALY @ ${ts}] GIE=0 — kernel I2C interrupts DISABLED, heartbeats via kernel will timeout!" >&2
        fi
        # Check: bus stuck busy
        local sr_val=$((${sr}))
        if [ $((sr_val & 0x04)) -ne 0 ]; then
            echo "[ANOMALY @ ${ts}] SR bus-busy stuck: SR=${sr}" >&2
        fi
        # Check: TX_ERROR in ISR (NACK)
        local isr_val=$((${isr}))
        if [ $((isr_val & 0x02)) -ne 0 ]; then
            echo "[ANOMALY @ ${ts}] ISR TX_ERROR (NACK): ISR=${isr}" >&2
        fi
        # Check: CR not enabled
        local cr_val=$((${cr}))
        if [ $((cr_val & 0x01)) -eq 0 ]; then
            echo "[ANOMALY @ ${ts}] CR.EN=0 — IIC controller DISABLED: CR=${cr}" >&2
        fi

        usleep $INTERVAL_US 2>/dev/null || sleep 0
    done

    echo "[REG_CAPTURE] Done: ${tick} samples written to ${REG_CSV}" >&2
}

# ============================================================================
# PHASE 2: PIC Heartbeat Probe (background)
# Sends heartbeats via kernel I2C and records success/fail per PIC
# NOTE: This runs INDEPENDENTLY of dcentrald. It probes heartbeat capability
# by attempting I2C writes to each PIC and checking for NACK.
# ============================================================================
capture_heartbeats() {
    echo "tick,uptime,pic_addr,raw_read,hb_write_result,hb_write_errno,notes" > "$HB_CSV"

    local tick=0
    local end_time=$(($(date +%s) + DURATION))
    local first_fail_55=""
    local first_fail_56=""
    local first_fail_57=""

    while [ "$(date +%s)" -lt "$end_time" ]; do
        tick=$((tick + 1))
        local ts=$(get_ts)

        for addr in $PIC_ADDRS; do
            # Step 1: Raw read — what state is the PIC in?
            # 0x60 = app mode, 0xCC = bootloader, 0xFF = dead/no response
            local raw=$(i2cget -y 0 "$addr" 2>&1)
            local raw_rc=$?
            if [ $raw_rc -ne 0 ]; then
                raw="ERR:${raw_rc}"
            fi

            # Step 2: Attempt a heartbeat write (BraiinsOS: 0x55, 0xAA, 0x16)
            # Using i2cset byte-by-byte to match dcentrald's write_byte_by_byte pattern
            local hb_result="OK"
            local hb_errno=0

            # Byte 1: 0x55 (preamble byte 1)
            i2cset -y 0 "$addr" 0x55 b 2>/dev/null
            local rc1=$?
            if [ $rc1 -ne 0 ]; then
                hb_result="FAIL_BYTE1"
                hb_errno=$rc1
            else
                # Byte 2: 0xAA (preamble byte 2)
                i2cset -y 0 "$addr" 0xAA b 2>/dev/null
                local rc2=$?
                if [ $rc2 -ne 0 ]; then
                    hb_result="FAIL_BYTE2"
                    hb_errno=$rc2
                else
                    # Byte 3: 0x16 (BraiinsOS heartbeat command)
                    i2cset -y 0 "$addr" 0x16 b 2>/dev/null
                    local rc3=$?
                    if [ $rc3 -ne 0 ]; then
                        hb_result="FAIL_BYTE3"
                        hb_errno=$rc3
                    fi
                fi
            fi

            # Track first failure time per PIC
            local notes=""
            if [ "$hb_result" != "OK" ]; then
                case "$addr" in
                    0x55) [ -z "$first_fail_55" ] && first_fail_55="$ts" && notes="FIRST_FAIL" ;;
                    0x56) [ -z "$first_fail_56" ] && first_fail_56="$ts" && notes="FIRST_FAIL" ;;
                    0x57) [ -z "$first_fail_57" ] && first_fail_57="$ts" && notes="FIRST_FAIL" ;;
                esac
            fi

            echo "${tick},${ts},${addr},${raw},${hb_result},${hb_errno},${notes}" >> "$HB_CSV"
        done

        # 1-second interval matching dcentrald heartbeat period
        sleep 1
    done

    echo "[HB_CAPTURE] Done: ${tick} ticks. First failures: 0x55=${first_fail_55:-none} 0x56=${first_fail_56:-none} 0x57=${first_fail_57:-none}" >&2
}

# ============================================================================
# PHASE 3: PIC Read-Back Test
# After each heartbeat, READ BACK from the PIC to verify the bus actually works.
# If reads succeed but the PIC still dies, the data gets through but is ignored.
# If reads fail, the I2C bus is actually broken.
# ============================================================================
capture_readback() {
    echo "tick,uptime,pic_addr,hb_result,readback_result,readback_data,read_version_result,version_data,notes" > "$RB_CSV"

    local tick=0
    local end_time=$(($(date +%s) + DURATION))

    while [ "$(date +%s)" -lt "$end_time" ]; do
        tick=$((tick + 1))
        local ts=$(get_ts)

        for addr in $PIC_ADDRS; do
            # Send heartbeat (3 byte-by-byte writes)
            local hb_ok="OK"
            i2cset -y 0 "$addr" 0x55 b 2>/dev/null || hb_ok="FAIL"
            [ "$hb_ok" = "OK" ] && { i2cset -y 0 "$addr" 0xAA b 2>/dev/null || hb_ok="FAIL"; }
            [ "$hb_ok" = "OK" ] && { i2cset -y 0 "$addr" 0x16 b 2>/dev/null || hb_ok="FAIL"; }

            # Wait 5ms for PIC to process
            usleep 5000 2>/dev/null || sleep 0

            # Read back: plain I2C read (should return 0x60 if PIC is alive in app mode)
            local rb=$(i2cget -y 0 "$addr" 2>&1)
            local rb_rc=$?
            local rb_result="OK"
            if [ $rb_rc -ne 0 ]; then
                rb_result="FAIL"
                rb="ERR"
            fi

            # Read version: send GET_VERSION command (0x55 0xAA 0x04) then read
            # This is a deeper test — exercises the PIC's command parser
            local ver_result="SKIP"
            local ver_data="N/A"
            if [ "$rb_result" = "OK" ] && [ $((tick % 10)) -eq 0 ]; then
                # Only every 10th tick to avoid overwhelming the PIC
                i2cset -y 0 "$addr" 0x55 b 2>/dev/null
                i2cset -y 0 "$addr" 0xAA b 2>/dev/null
                i2cset -y 0 "$addr" 0x04 b 2>/dev/null
                usleep 5000 2>/dev/null || sleep 0
                ver_data=$(i2cget -y 0 "$addr" 2>&1)
                local ver_rc=$?
                if [ $ver_rc -eq 0 ]; then
                    ver_result="OK"
                else
                    ver_result="FAIL"
                    ver_data="ERR"
                fi
            fi

            local notes=""
            # Diagnose: heartbeat sent OK but readback fails = bus broke AFTER write
            if [ "$hb_ok" = "OK" ] && [ "$rb_result" = "FAIL" ]; then
                notes="HB_OK_BUT_READ_FAIL"
            fi
            # Diagnose: both fail = bus is broken
            if [ "$hb_ok" = "FAIL" ] && [ "$rb_result" = "FAIL" ]; then
                notes="BUS_DEAD"
            fi
            # Diagnose: heartbeat fails but read works = PIC alive, write path broken
            if [ "$hb_ok" = "FAIL" ] && [ "$rb_result" = "OK" ]; then
                notes="WRITE_BROKEN_READ_OK"
            fi

            echo "${tick},${ts},${addr},${hb_ok},${rb_result},${rb},${ver_result},${ver_data},${notes}" >> "$RB_CSV"
        done

        # 2-second interval (interleave with heartbeat capture, avoid bus contention)
        sleep 2
    done

    echo "[READBACK] Done: ${tick} ticks written to ${RB_CSV}" >&2
}

# ============================================================================
# MAIN
# ============================================================================
echo "=============================================="
echo " DCENT_OS I2C Heartbeat Diagnostic Capture"
echo " Duration: ${DURATION}s  Interval: ${INTERVAL_US}us"
echo " Target: AXI IIC @ 0x${AXI_BASE}"
echo " PICs: ${PIC_ADDRS}"
echo " Output: /tmp/i2c_diag_*.csv"
echo "=============================================="
echo ""

# Pre-flight: check devmem and i2c tools exist
if ! command -v devmem >/dev/null 2>&1; then
    echo "ERROR: devmem not found. Install busybox-devmem." >&2
    exit 1
fi
if ! command -v i2cget >/dev/null 2>&1; then
    echo "ERROR: i2cget not found. Install i2c-tools." >&2
    exit 1
fi

# Pre-flight: snapshot baseline register state
echo "=== BASELINE (before capture) ==="
echo "CR:     $(read_reg $REG_CR)   $(decode_cr $(read_reg $REG_CR))"
echo "SR:     $(read_reg $REG_SR)   $(decode_sr $(read_reg $REG_SR))"
echo "ISR:    $(read_reg $REG_ISR)  $(decode_isr $(read_reg $REG_ISR))"
echo "GIE:    $(read_reg $REG_GIE)"
echo "IER:    $(read_reg $REG_IER)"
echo "THIGH:  $(read_reg $REG_THIGH)"
echo "TLOW:   $(read_reg $REG_TLOW)"
echo "TBUF:   $(read_reg $REG_TBUF)"
echo "TSUSTA: $(read_reg $REG_TSUSTA)"
echo "TSUSTO: $(read_reg $REG_TSUSTO)"
echo "THDSTA: $(read_reg $REG_THDSTA)"
echo "TSUDAT: $(read_reg $REG_TSUDAT)"
echo "THDDAT: $(read_reg $REG_THDDAT)"
echo ""

# Pre-flight: check PIC state
echo "=== PIC STATE ==="
for addr in $PIC_ADDRS; do
    raw=$(i2cget -y 0 "$addr" 2>&1)
    rc=$?
    if [ $rc -eq 0 ]; then
        echo "PIC $addr: $raw (rc=$rc)"
        case "$raw" in
            0x60) echo "  -> APP MODE (good)" ;;
            0xcc|0xCC) echo "  -> BOOTLOADER (needs JUMP)" ;;
            0xff|0xFF) echo "  -> NO RESPONSE (dead or powered off)" ;;
            *) echo "  -> UNKNOWN STATE" ;;
        esac
    else
        echo "PIC $addr: FAILED (rc=$rc) — $raw"
    fi
done
echo ""

# Check if dcentrald is running (if so, we're capturing alongside it)
if pidof dcentrald >/dev/null 2>&1; then
    echo "NOTE: dcentrald is RUNNING (PID $(pidof dcentrald)) — capturing alongside daemon heartbeats"
    echo "      This script's heartbeat writes may INTERFERE with daemon's heartbeat thread."
    echo "      For cleanest results, stop dcentrald first: /etc/init.d/dcentrald stop"
    echo ""
fi

# Record start conditions
echo "I2C Heartbeat Diagnostic — $(date)" > "$SUMMARY"
echo "Duration: ${DURATION}s" >> "$SUMMARY"
echo "dcentrald PID: $(pidof dcentrald 2>/dev/null || echo none)" >> "$SUMMARY"
echo "" >> "$SUMMARY"

echo "Starting 3 capture threads..."
echo "  [1] Register snapshots (every ${INTERVAL_US}us) -> ${REG_CSV}"
echo "  [2] Heartbeat probe (every 1s) -> ${HB_CSV}"
echo "  [3] Read-back test (every 2s) -> ${RB_CSV}"
echo ""

# Launch all three captures in parallel
capture_registers 2>/tmp/i2c_diag_anomalies.log &
REG_PID=$!

capture_heartbeats 2>/tmp/i2c_diag_hb_status.log &
HB_PID=$!

capture_readback 2>/tmp/i2c_diag_rb_status.log &
RB_PID=$!

echo "Capture running: REG_PID=$REG_PID HB_PID=$HB_PID RB_PID=$RB_PID"
echo "Waiting ${DURATION} seconds..."
echo "  Real-time anomalies: tail -f /tmp/i2c_diag_anomalies.log"
echo "  Heartbeat status:    tail -f /tmp/i2c_diag_hb_status.log"
echo ""

# Wait for all to finish
wait $REG_PID 2>/dev/null
wait $HB_PID 2>/dev/null
wait $RB_PID 2>/dev/null

echo ""
echo "=============================================="
echo " CAPTURE COMPLETE"
echo "=============================================="
echo ""

# ============================================================================
# POST-CAPTURE ANALYSIS
# ============================================================================

echo "=== REGISTER ANOMALIES ===" >> "$SUMMARY"
if [ -s /tmp/i2c_diag_anomalies.log ]; then
    cat /tmp/i2c_diag_anomalies.log >> "$SUMMARY"
    echo "" >> "$SUMMARY"
    ANOMALY_COUNT=$(wc -l < /tmp/i2c_diag_anomalies.log)
    echo "  $ANOMALY_COUNT anomalies detected (see /tmp/i2c_diag_anomalies.log)"
else
    echo "  None detected." >> "$SUMMARY"
    echo "  No register anomalies detected."
fi
echo ""

echo "=== HEARTBEAT FAILURES ===" >> "$SUMMARY"
if [ -f "$HB_CSV" ]; then
    FAIL_55=$(grep -c "0x55.*FAIL" "$HB_CSV" 2>/dev/null || echo 0)
    FAIL_56=$(grep -c "0x56.*FAIL" "$HB_CSV" 2>/dev/null || echo 0)
    FAIL_57=$(grep -c "0x57.*FAIL" "$HB_CSV" 2>/dev/null || echo 0)
    TOTAL_55=$(grep -c "0x55" "$HB_CSV" 2>/dev/null || echo 0)
    TOTAL_56=$(grep -c "0x56" "$HB_CSV" 2>/dev/null || echo 0)
    TOTAL_57=$(grep -c "0x57" "$HB_CSV" 2>/dev/null || echo 0)
    echo "  PIC 0x55: ${FAIL_55}/${TOTAL_55} failed" | tee -a "$SUMMARY"
    echo "  PIC 0x56: ${FAIL_56}/${TOTAL_56} failed" | tee -a "$SUMMARY"
    echo "  PIC 0x57: ${FAIL_57}/${TOTAL_57} failed" | tee -a "$SUMMARY"

    # Find first failure time for each PIC
    for addr in 0x55 0x56 0x57; do
        FIRST=$(grep "${addr}.*FIRST_FAIL" "$HB_CSV" 2>/dev/null | head -1)
        if [ -n "$FIRST" ]; then
            echo "  FIRST FAIL $addr: $FIRST" | tee -a "$SUMMARY"
        fi
    done
fi
echo "" >> "$SUMMARY"

echo "=== READBACK DIAGNOSIS ===" >> "$SUMMARY"
if [ -f "$RB_CSV" ]; then
    HB_OK_READ_FAIL=$(grep -c "HB_OK_BUT_READ_FAIL" "$RB_CSV" 2>/dev/null || echo 0)
    BUS_DEAD=$(grep -c "BUS_DEAD" "$RB_CSV" 2>/dev/null || echo 0)
    WRITE_BROKEN=$(grep -c "WRITE_BROKEN_READ_OK" "$RB_CSV" 2>/dev/null || echo 0)
    echo "  Heartbeat OK but read fails:  $HB_OK_READ_FAIL (bus breaks AFTER write)" | tee -a "$SUMMARY"
    echo "  Both fail (bus dead):          $BUS_DEAD" | tee -a "$SUMMARY"
    echo "  Write fails but read OK:       $WRITE_BROKEN (write path broken)" | tee -a "$SUMMARY"

    if [ "$HB_OK_READ_FAIL" -gt 0 ]; then
        echo "" | tee -a "$SUMMARY"
        echo "  >>> VERDICT: I2C write completes but bus state corrupted after." | tee -a "$SUMMARY"
        echo "      PIC likely receives garbled data. Check timing registers." | tee -a "$SUMMARY"
    elif [ "$BUS_DEAD" -gt 0 ]; then
        echo "" | tee -a "$SUMMARY"
        echo "  >>> VERDICT: I2C bus goes FULLY DEAD. AXI IIC controller stuck." | tee -a "$SUMMARY"
        echo "      Check SR for bus-busy stuck, CR for disabled, timing for zeroed." | tee -a "$SUMMARY"
    elif [ "$WRITE_BROKEN" -gt 0 ]; then
        echo "" | tee -a "$SUMMARY"
        echo "  >>> VERDICT: Write path broken but read works. TX FIFO or START gen issue." | tee -a "$SUMMARY"
    else
        echo "" | tee -a "$SUMMARY"
        echo "  >>> VERDICT: No readback anomalies. PIC communication appears healthy." | tee -a "$SUMMARY"
    fi
fi
echo "" >> "$SUMMARY"

# Final register state
echo "=== FINAL REGISTER STATE ===" >> "$SUMMARY"
echo "CR:     $(read_reg $REG_CR)" >> "$SUMMARY"
echo "SR:     $(read_reg $REG_SR)" >> "$SUMMARY"
echo "ISR:    $(read_reg $REG_ISR)" >> "$SUMMARY"
echo "GIE:    $(read_reg $REG_GIE)" >> "$SUMMARY"
echo "IER:    $(read_reg $REG_IER)" >> "$SUMMARY"
echo "THIGH:  $(read_reg $REG_THIGH)" >> "$SUMMARY"
echo "TLOW:   $(read_reg $REG_TLOW)" >> "$SUMMARY"
echo "TBUF:   $(read_reg $REG_TBUF)" >> "$SUMMARY"

echo ""
echo "=== FILES ==="
echo "  Registers:  ${REG_CSV} ($(wc -l < "$REG_CSV" 2>/dev/null || echo 0) lines)"
echo "  Heartbeats: ${HB_CSV} ($(wc -l < "$HB_CSV" 2>/dev/null || echo 0) lines)"
echo "  Readback:   ${RB_CSV} ($(wc -l < "$RB_CSV" 2>/dev/null || echo 0) lines)"
echo "  Anomalies:  /tmp/i2c_diag_anomalies.log"
echo "  Summary:    ${SUMMARY}"
echo ""

cat "$SUMMARY"
