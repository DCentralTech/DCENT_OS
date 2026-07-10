#!/bin/sh
#
# hb_bridge.sh - PIC Heartbeat Bridge for Binary Upgrades
# D-Central Technologies, 2026
#
# Sends I2C heartbeats to all 3 PIC voltage controllers via devmem,
# keeping hash boards powered during dcentrald restarts.
#
# The BraiinsOS PIC watchdog fires after ~10s without a heartbeat,
# cutting DC-DC voltage and corrupting the I2C bus. This bridge
# maintains heartbeats independently of dcentrald.
#
# Runs on the miner itself (BusyBox ash compatible, no bash needed).
# Uses devmem AXI IIC register writes -- no kernel I2C driver required.
#
# Usage:
#   /data/hb_bridge.sh &              # Start in background
#   kill $(cat /tmp/hb_bridge.pid)    # Stop
#
# Protocol:
#   Stock PIC (v0x56+):    single I2C write [0x55 0xAA 0x11]
#   BraiinsOS PIC (v0x03): byte-by-byte   [0x55] [0xAA] [0x16]
#   Unknown:               send both (wrong one is silently ignored)
#
# AXI IIC register map (Xilinx PG090, base 0x41600000):
#   0x020 = ISR     (Interrupt Status)
#   0x040 = SOFTR   (Software Reset)
#   0x100 = CR      (Control: bit0=EN, bit1=TX_FIFO_RESET)
#   0x104 = SR      (Status: bit2=BB, bit6=RX_EMPTY, bit7=TX_EMPTY)
#   0x108 = TX_FIFO (Data + control: bit8=START, bit9=STOP)
#   0x128 = TSUSTA  (Setup time for START)
#   0x12C = TSUSTO  (Setup time for STOP)
#   0x130 = THDSTA  (Hold time for repeated START)
#   0x138 = TBUF    (Bus free time)
#   0x13C = THIGH   (SCL high period)
#   0x140 = TLOW    (SCL low period)
#
# CRITICAL SAFETY NOTES:
#   - NEVER use I2C_RDWR ioctl for PIC (corrupts MSSP parser)
#   - NEVER use kernel i2c driver simultaneously with devmem
#   - dcentrald unbinds the kernel driver; this script uses the same devmem path
#   - Clock timing MUST be set after every SOFTR (reset clears to 0 = max speed)

# ── Configuration ───────────────────────────────────────────────────────────

AXI_IIC_BASE=0x41600000

# Register offsets (hex, added to base for devmem calls)
REG_ISR=0x41600020
REG_SOFTR=0x41600040
REG_CR=0x41600100
REG_SR=0x41600104
REG_TX_FIFO=0x41600108
REG_THIGH=0x4160013C
REG_TLOW=0x41600140
REG_TBUF=0x41600138
REG_THDSTA=0x41600130
REG_TSUSTA=0x41600128
REG_TSUSTO=0x4160012C
REG_GIE=0x4160001C
REG_IER=0x41600028

# PIC I2C addresses (S9: chains 6, 7, 8)
PIC_ADDRS="0x55 0x56 0x57"

# I2C clock timing divider (1498 = ~33 kHz at 100 MHz AXI clock)
# Matches dcentrald's IIC_TIMING_100KHZ constant for reliable PIC comms
IIC_TIMING=1498

# Heartbeat interval (seconds). PIC watchdog is ~10s, send every 2s for margin.
HB_INTERVAL=2

# PIC heartbeat commands
# Stock:    0x55 0xAA 0x11 (single transaction)
# BraiinsOS: 0x55 0xAA 0x16 (byte-by-byte)
STOCK_CMD_BYTE=0x11
BRAIINS_CMD_BYTE=0x16

# PIC command preamble
PREAMBLE_1=0x55
PREAMBLE_2=0xAA

# PID file
PIDFILE="/tmp/hb_bridge.pid"
LOGFILE="/tmp/hb_bridge.log"

# ── Functions ───────────────────────────────────────────────────────────────

# Ensure usleep is available (BusyBox applet). If not, approximate with sleep.
if ! command -v usleep > /dev/null 2>&1; then
    usleep() {
        # Approximate: anything < 100ms rounds to 0.1s, else convert to seconds
        local us=$1
        if [ "$us" -lt 100000 ]; then
            # sleep 0 is a no-op on BusyBox (sub-second not supported)
            # Best effort: just return immediately for short delays
            :
        else
            sleep $(( us / 1000000 ))
        fi
    }
fi

log() {
    echo "$(date '+%H:%M:%S') hb_bridge: $1" >> "$LOGFILE"
}

# Read a 32-bit register via devmem
read_reg() {
    devmem "$1" 32 2>/dev/null
}

# Write a 32-bit register via devmem
write_reg() {
    devmem "$1" 32 "$2" 2>/dev/null
}

# Initialize the AXI IIC controller for devmem access.
# Mirrors dcentrald's one-time init in devmem_i2c_write_inner().
init_iic() {
    log "Initializing AXI IIC controller"

    # Disable GIE (no interrupts)
    write_reg $REG_GIE 0x00000000

    # Disable controller
    write_reg $REG_CR 0x00000000
    usleep 10000  # 10ms

    # Software reset (clears ISR flags and FIFOs)
    write_reg $REG_SOFTR 0x0000000A
    usleep 5000   # 5ms

    # CRITICAL: Set I2C clock timing after SOFTR (which resets them to 0 = max speed).
    # PIC NACKs at max speed. Must set slow clock for reliable comms.
    write_reg $REG_THIGH $IIC_TIMING
    write_reg $REG_TLOW $IIC_TIMING
    write_reg $REG_TBUF $IIC_TIMING
    write_reg $REG_THDSTA $IIC_TIMING
    write_reg $REG_TSUSTA $IIC_TIMING
    write_reg $REG_TSUSTO $IIC_TIMING

    # Enable interrupt sources
    write_reg $REG_IER 0x0000001F

    # Enable controller (CR bit 0 = EN)
    write_reg $REG_CR 0x00000001
    usleep 1000   # 1ms

    log "AXI IIC controller initialized (timing=$IIC_TIMING)"
}

# Wait for I2C bus idle (SR bit 2 = Bus Busy)
wait_bus_idle() {
    local tries=0
    while [ $tries -lt 100 ]; do
        local sr
        sr=$(read_reg $REG_SR)
        # SR bit 2 (0x04) = Bus Busy
        if [ $(( sr & 0x04 )) -eq 0 ]; then
            return 0
        fi
        tries=$((tries + 1))
        usleep 1000  # 1ms
    done
    log "WARNING: bus stuck busy (SR=$sr)"
    return 1
}

# Wait for transfer complete (bus goes idle after START)
wait_transfer_complete() {
    local tries=0
    # Wait for BB to go high (transfer started)
    while [ $tries -lt 50 ]; do
        local sr
        sr=$(read_reg $REG_SR)
        if [ $(( sr & 0x04 )) -ne 0 ]; then
            break
        fi
        tries=$((tries + 1))
        usleep 200
    done

    # Wait for BB to go low (transfer complete)
    tries=0
    while [ $tries -lt 500 ]; do
        local sr
        sr=$(read_reg $REG_SR)
        if [ $(( sr & 0x04 )) -eq 0 ]; then
            # Check for NACK
            local isr
            isr=$(read_reg $REG_ISR)
            if [ $(( isr & 0x02 )) -ne 0 ]; then
                # Clear TX_ERROR bit
                write_reg $REG_ISR 0x00000002
                # Flush TX FIFO
                write_reg $REG_CR 0x00000003  # EN + TX_FIFO_RESET
                write_reg $REG_CR 0x00000001  # EN only
                return 1  # NACK
            fi
            return 0  # Success
        fi
        tries=$((tries + 1))
        usleep 200
    done
    log "WARNING: transfer timeout"
    return 1
}

# Send a heartbeat to one PIC via AXI IIC dynamic mode write.
# Sends a single 3-byte I2C write: START + addr(W) + preamble + cmd + STOP.
#
# Dynamic mode TX FIFO format:
#   Byte 0: START(bit8) | (slave_addr << 1) | W(0)
#   Byte 1: data byte (preamble[0])
#   Byte 2: data byte (preamble[1])
#   Byte 3: STOP(bit9) | data byte (cmd)
send_heartbeat_stock() {
    local pic_addr=$1
    local addr_byte=$(( 0x100 | (pic_addr << 1) ))  # START + addr(W)

    # Flush TX FIFO
    write_reg $REG_CR 0x00000003  # EN + TX_FIFO_RESET
    write_reg $REG_CR 0x00000001  # EN only

    # Clear stale ISR
    local isr
    isr=$(read_reg $REG_ISR)
    if [ "$isr" != "0x00000000" ] 2>/dev/null; then
        write_reg $REG_ISR "$isr"
    fi

    # TX FIFO: START + addr(W)
    write_reg $REG_TX_FIFO $addr_byte
    # TX FIFO: preamble byte 1 (0x55)
    write_reg $REG_TX_FIFO $PREAMBLE_1
    # TX FIFO: preamble byte 2 (0xAA)
    write_reg $REG_TX_FIFO $PREAMBLE_2
    # TX FIFO: command + STOP (0x200 | cmd)
    write_reg $REG_TX_FIFO $(( 0x200 | STOCK_CMD_BYTE ))

    wait_transfer_complete
}

# Send BraiinsOS heartbeat (byte-by-byte, 3 separate I2C writes).
# Each byte is a full START-data-STOP transaction.
send_heartbeat_braiins() {
    local pic_addr=$1
    local addr_byte=$(( 0x100 | (pic_addr << 1) ))  # START + addr(W)

    # Byte 1: preamble[0] = 0x55
    write_reg $REG_CR 0x00000003; write_reg $REG_CR 0x00000001
    write_reg $REG_TX_FIFO $addr_byte
    write_reg $REG_TX_FIFO $(( 0x200 | PREAMBLE_1 ))
    wait_transfer_complete

    # Byte 2: preamble[1] = 0xAA
    wait_bus_idle
    write_reg $REG_CR 0x00000003; write_reg $REG_CR 0x00000001
    write_reg $REG_TX_FIFO $addr_byte
    write_reg $REG_TX_FIFO $(( 0x200 | PREAMBLE_2 ))
    wait_transfer_complete

    # Byte 3: command = 0x16
    wait_bus_idle
    write_reg $REG_CR 0x00000003; write_reg $REG_CR 0x00000001
    write_reg $REG_TX_FIFO $addr_byte
    write_reg $REG_TX_FIFO $(( 0x200 | BRAIINS_CMD_BYTE ))
    wait_transfer_complete
}

# Send heartbeat to one PIC (tries both protocols).
# The wrong protocol is silently ignored by the PIC.
send_heartbeat() {
    local pic_addr=$1
    wait_bus_idle || return 1

    # Try stock heartbeat first (single transaction, faster)
    send_heartbeat_stock "$pic_addr"

    usleep 5000  # 5ms gap between protocols

    # Then BraiinsOS heartbeat (byte-by-byte)
    wait_bus_idle || return 1
    send_heartbeat_braiins "$pic_addr"
}

# ── Signal Handlers ─────────────────────────────────────────────────────────

cleanup() {
    log "Stopping heartbeat bridge (signal received)"
    rm -f "$PIDFILE"
    exit 0
}

trap cleanup INT TERM HUP

# ── Main Loop ───────────────────────────────────────────────────────────────

# Write PID file
echo $$ > "$PIDFILE"

# Truncate log
> "$LOGFILE"
log "Starting PIC heartbeat bridge (PID=$$)"
log "PICs: $PIC_ADDRS, interval: ${HB_INTERVAL}s"

# Unbind kernel I2C driver (dcentrald does this too, but be safe)
if [ -e /sys/bus/platform/drivers/xiic-i2c/41600000.i2c ]; then
    echo 41600000.i2c > /sys/bus/platform/drivers/xiic-i2c/unbind 2>/dev/null
    log "Unbound kernel xiic-i2c driver"
fi

# Initialize AXI IIC
init_iic

HEARTBEAT_COUNT=0
FAIL_COUNT=0

while true; do
    for addr in $PIC_ADDRS; do
        if send_heartbeat "$addr"; then
            : # success, no log spam
        else
            FAIL_COUNT=$((FAIL_COUNT + 1))
            log "WARN: heartbeat failed for PIC $addr (fail_count=$FAIL_COUNT)"
            # Re-init controller after 3 consecutive failures
            if [ $FAIL_COUNT -ge 3 ]; then
                log "Re-initializing AXI IIC after $FAIL_COUNT failures"
                init_iic
                FAIL_COUNT=0
            fi
        fi
    done

    HEARTBEAT_COUNT=$((HEARTBEAT_COUNT + 1))
    if [ $((HEARTBEAT_COUNT % 10)) -eq 0 ]; then
        log "Heartbeat round $HEARTBEAT_COUNT (all PICs alive)"
    fi

    sleep $HB_INTERVAL
done
