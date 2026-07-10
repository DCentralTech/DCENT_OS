#!/bin/sh
#
# test_suite.sh -- DCENTos Comprehensive Diagnostic Report Generator
# ====================================================================
# Orchestrates ALL diagnostic operations on an Antminer S9 (Zynq 7010)
# and generates a comprehensive, timestamped report directory that can
# be SCP'd back to a desktop for analysis.
#
# Part of DCENTos Hacker Shell firmware research tools.
#
# Hardware Target:
#   - Antminer S9 (Zynq 7010, BM1387 x 63 x 3 boards)
#   - UART: /dev/ttyPS1 (hash boards via FPGA)
#   - I2C:  /dev/i2c-0, /dev/i2c-1
#   - FPGA: UIO devices (/dev/uio0-uio13) or devmem at 0x43C00000
#
# Output:
#   /tmp/dcentos-report-YYYYMMDD-HHMMSS/
#   ├── summary.txt           (human-readable summary)
#   ├── system_info.txt       (kernel, memory, uptime, etc.)
#   ├── i2c_scan.txt          (all I2C buses)
#   ├── fpga_registers.txt    (all 352 bytes)
#   ├── asic_chain.json       (chip enumeration results)
#   ├── register_dump.json    (register scan of chip 0)
#   ├── psu_data.json         (PSU readings)
#   ├── temperature.json      (temperature register candidates)
#   ├── assumptions.json      (assumption verification results)
#   └── dmesg.txt             (kernel log)
#
# Usage:
#   test_suite.sh              # Run full diagnostic suite
#   test_suite.sh --quick      # Quick mode (skip slow scans)
#   test_suite.sh --test       # Dry-run (show what would be done)
#   test_suite.sh --help       # Show usage
#

set -e

# ============================================================================
# Configuration
# ============================================================================

TOOLS_DIR="/root/tools"
TIMESTAMP=$(date +"%Y%m%d-%H%M%S")
REPORT_DIR="/tmp/dcentos-report-${TIMESTAMP}"
FPGA_BASE="0x43C00000"
FPGA_SIZE_WORDS=88  # 352 bytes / 4
UART_DEV="/dev/ttyPS1"

# Colors (if terminal supports it)
RED=""
GREEN=""
YELLOW=""
BLUE=""
BOLD=""
RESET=""

if [ -t 1 ]; then
    RED="\033[31m"
    GREEN="\033[32m"
    YELLOW="\033[33m"
    BLUE="\033[34m"
    BOLD="\033[1m"
    RESET="\033[0m"
fi

# Counters
PASS_COUNT=0
FAIL_COUNT=0
SKIP_COUNT=0
TOTAL_STEPS=0

# ============================================================================
# Helper Functions
# ============================================================================

log_header() {
    printf "\n${BOLD}${BLUE}=== %s ===${RESET}\n" "$1"
}

log_step() {
    TOTAL_STEPS=$((TOTAL_STEPS + 1))
    printf "  [%02d] %s... " "$TOTAL_STEPS" "$1"
}

log_pass() {
    PASS_COUNT=$((PASS_COUNT + 1))
    printf "${GREEN}OK${RESET}"
    if [ -n "$1" ]; then
        printf " (%s)" "$1"
    fi
    printf "\n"
}

log_fail() {
    FAIL_COUNT=$((FAIL_COUNT + 1))
    printf "${RED}FAIL${RESET}"
    if [ -n "$1" ]; then
        printf " (%s)" "$1"
    fi
    printf "\n"
}

log_skip() {
    SKIP_COUNT=$((SKIP_COUNT + 1))
    printf "${YELLOW}SKIP${RESET}"
    if [ -n "$1" ]; then
        printf " (%s)" "$1"
    fi
    printf "\n"
}

log_info() {
    printf "  ${BLUE}INFO:${RESET} %s\n" "$1"
}

file_size() {
    if [ -f "$1" ]; then
        wc -c < "$1" | tr -d ' '
    else
        echo "0"
    fi
}

# Check if a command exists
has_cmd() {
    command -v "$1" >/dev/null 2>&1
}

# Safe devmem read (returns empty string on failure)
safe_devmem() {
    if has_cmd devmem; then
        devmem "$1" 2>/dev/null || true
    fi
}

# ============================================================================
# Usage
# ============================================================================

show_help() {
    cat <<'HELP'
test_suite.sh -- DCENTos Comprehensive Diagnostic Report Generator

Usage:
  test_suite.sh [OPTIONS]

Options:
  --help       Show this help message
  --test       Dry-run mode (show what would be done, no hardware access)
  --quick      Quick mode (skip slow scans like full register dump)
  --verbose    Show more detail during execution
  --no-asic    Skip ASIC chain tests (if chain is not powered)
  --no-i2c     Skip I2C tests (if no hash boards connected)
  --no-fpga    Skip FPGA register dump (if devmem not available)
  --outdir DIR Custom output directory (default: /tmp/dcentos-report-YYYYMMDD-HHMMSS)

Output:
  Creates a timestamped report directory under /tmp/ containing:
    summary.txt, system_info.txt, i2c_scan.txt, fpga_registers.txt,
    asic_chain.json, register_dump.json, psu_data.json, temperature.json,
    assumptions.json, dmesg.txt

  The directory can be transferred with:
    scp -r root@<miner-ip>:/tmp/dcentos-report-*/ ./

Examples:
  # Full diagnostic suite:
  test_suite.sh

  # Quick mode (skip slow scans):
  test_suite.sh --quick

  # Skip ASIC tests (boards not powered):
  test_suite.sh --no-asic

  # Custom output directory:
  test_suite.sh --outdir /mnt/usb/report
HELP
}

# ============================================================================
# Parse Arguments
# ============================================================================

DRY_RUN=0
QUICK_MODE=0
VERBOSE=0
SKIP_ASIC=0
SKIP_I2C=0
SKIP_FPGA=0

while [ $# -gt 0 ]; do
    case "$1" in
        --help|-h)
            show_help
            exit 0
            ;;
        --test)
            DRY_RUN=1
            ;;
        --quick)
            QUICK_MODE=1
            ;;
        --verbose)
            VERBOSE=1
            ;;
        --no-asic)
            SKIP_ASIC=1
            ;;
        --no-i2c)
            SKIP_I2C=1
            ;;
        --no-fpga)
            SKIP_FPGA=1
            ;;
        --outdir)
            shift
            REPORT_DIR="$1"
            ;;
        *)
            printf "Unknown option: %s\n" "$1"
            show_help
            exit 1
            ;;
    esac
    shift
done

# ============================================================================
# Banner
# ============================================================================

printf "\n"
printf "${BOLD}╔══════════════════════════════════════════════════════════════╗${RESET}\n"
printf "${BOLD}║       DCENTos Comprehensive Diagnostic Suite v1.0          ║${RESET}\n"
printf "${BOLD}║       Antminer S9 / Zynq 7010 / BM1387                    ║${RESET}\n"
printf "${BOLD}╚══════════════════════════════════════════════════════════════╝${RESET}\n"
printf "\n"
printf "  Timestamp: %s\n" "$TIMESTAMP"
printf "  Report:    %s\n" "$REPORT_DIR"
printf "  Mode:      %s\n" "$([ $DRY_RUN -eq 1 ] && echo 'DRY RUN' || echo 'LIVE')"
printf "\n"

if [ $DRY_RUN -eq 1 ]; then
    printf "${YELLOW}DRY RUN MODE: No hardware access, showing planned steps.${RESET}\n\n"
    printf "Would create: %s/\n" "$REPORT_DIR"
    printf "Would run:\n"
    printf "  1. System information collection\n"
    printf "  2. I2C bus scan (all buses)\n"
    printf "  3. FPGA register dump (352 bytes)\n"
    printf "  4. ASIC chain enumeration\n"
    printf "  5. Register scan on chip 0\n"
    printf "  6. PSU probe\n"
    printf "  7. Temperature register scan\n"
    printf "  8. Assumption verification (--all --safe --json)\n"
    printf "  9. Kernel log capture\n"
    printf " 10. Summary generation\n"
    exit 0
fi

# ============================================================================
# Create Report Directory
# ============================================================================

mkdir -p "$REPORT_DIR"
printf "Report directory: %s\n" "$REPORT_DIR"

# ============================================================================
# Phase 1: System Reconnaissance
# ============================================================================

log_header "Phase 1: System Reconnaissance"

# --- System Info ---
log_step "Collecting system information"
{
    echo "=== DCENTos System Information ==="
    echo "Timestamp: $(date)"
    echo "Hostname: $(hostname 2>/dev/null || echo 'unknown')"
    echo ""

    echo "--- Kernel ---"
    uname -a 2>/dev/null || echo "(uname not available)"
    echo ""

    echo "--- Uptime ---"
    uptime 2>/dev/null || echo "(uptime not available)"
    echo ""

    echo "--- Memory ---"
    cat /proc/meminfo 2>/dev/null | head -5 || echo "(meminfo not available)"
    echo ""

    echo "--- CPU ---"
    cat /proc/cpuinfo 2>/dev/null | head -20 || echo "(cpuinfo not available)"
    echo ""

    echo "--- Disk/Storage ---"
    df -h 2>/dev/null || echo "(df not available)"
    echo ""

    echo "--- Mount Points ---"
    mount 2>/dev/null || echo "(mount not available)"
    echo ""

    echo "--- NAND Partitions ---"
    cat /proc/mtd 2>/dev/null || echo "(no /proc/mtd)"
    echo ""

    echo "--- Device Nodes ---"
    echo "Serial:"
    ls -la /dev/ttyPS* 2>/dev/null || echo "  (no ttyPS devices)"
    echo "I2C:"
    ls -la /dev/i2c-* 2>/dev/null || echo "  (no i2c devices)"
    echo "FPGA (UIO):"
    ls -d /sys/class/uio/uio* 2>/dev/null | while read u; do
        echo "  $(basename $u): $(cat $u/name 2>/dev/null) @ $(cat $u/maps/map0/addr 2>/dev/null)"
    done
    [ ! -d /sys/class/uio/uio0 ] && echo "  (no UIO devices)"
    echo ""

    echo "--- Loaded Modules ---"
    lsmod 2>/dev/null || cat /proc/modules 2>/dev/null || echo "(modules not available)"
    echo ""

    echo "--- Network ---"
    ip addr 2>/dev/null || ifconfig 2>/dev/null || echo "(network info not available)"
    echo ""

    echo "--- Processes ---"
    ps aux 2>/dev/null || ps 2>/dev/null || echo "(ps not available)"

} > "$REPORT_DIR/system_info.txt" 2>&1
SIZE=$(file_size "$REPORT_DIR/system_info.txt")
log_pass "${SIZE} bytes"

# --- Kernel Log ---
log_step "Capturing kernel log (dmesg)"
{
    dmesg 2>/dev/null || echo "(dmesg not available)"
} > "$REPORT_DIR/dmesg.txt" 2>&1
SIZE=$(file_size "$REPORT_DIR/dmesg.txt")
log_pass "${SIZE} bytes"

# ============================================================================
# Phase 2: I2C Bus Scan
# ============================================================================

log_header "Phase 2: I2C Bus Scan"

if [ $SKIP_I2C -eq 1 ]; then
    log_step "I2C scan"
    log_skip "disabled via --no-i2c"
    echo "I2C scan skipped (--no-i2c)" > "$REPORT_DIR/i2c_scan.txt"
else
    log_step "Scanning all I2C buses"
    {
        echo "=== I2C Bus Scan ==="
        echo "Timestamp: $(date)"
        echo ""

        for bus in 0 1 2 3 4 5 6 7; do
            if [ -e "/dev/i2c-${bus}" ]; then
                echo "--- Bus ${bus} (/dev/i2c-${bus}) ---"
                if has_cmd i2cdetect; then
                    i2cdetect -y "${bus}" 2>/dev/null || echo "  (scan failed)"
                else
                    echo "  (i2cdetect not available, probing manually)"
                    # Manual probe using Python tool if available
                    if [ -f "$TOOLS_DIR/i2c_scanner.py" ]; then
                        python3 "$TOOLS_DIR/i2c_scanner.py" --bus "${bus}" 2>/dev/null || true
                    fi
                fi
                echo ""
            fi
        done

        # Run our comprehensive I2C scanner if available
        if [ -f "$TOOLS_DIR/i2c_scanner.py" ]; then
            echo "--- Comprehensive I2C Scanner ---"
            python3 "$TOOLS_DIR/i2c_scanner.py" 2>/dev/null || true
        fi

    } > "$REPORT_DIR/i2c_scan.txt" 2>&1
    SIZE=$(file_size "$REPORT_DIR/i2c_scan.txt")
    log_pass "${SIZE} bytes"
fi

# ============================================================================
# Phase 3: FPGA Register Dump
# ============================================================================

log_header "Phase 3: FPGA Register Dump"

if [ $SKIP_FPGA -eq 1 ]; then
    log_step "FPGA register dump"
    log_skip "disabled via --no-fpga"
    echo "FPGA scan skipped (--no-fpga)" > "$REPORT_DIR/fpga_registers.txt"
else
    log_step "Reading FPGA registers (352 bytes)"
    {
        echo "=== FPGA Register Dump ==="
        echo "Base Address: ${FPGA_BASE}"
        echo "Size: ${FPGA_SIZE_WORDS} words (352 bytes)"
        echo "Timestamp: $(date)"
        echo ""

        if has_cmd devmem; then
            echo "--- Register Values ---"
            printf "%-12s %-12s %-12s %s\n" "WORD_OFF" "BYTE_OFF" "VALUE" "NOTES"
            printf "%-12s %-12s %-12s %s\n" "--------" "--------" "----------" "-----"

            OFFSET=0
            READABLE=0
            FAILED=0
            while [ $OFFSET -lt $FPGA_SIZE_WORDS ]; do
                BYTE_OFFSET=$((OFFSET * 4))
                ADDR=$(printf "0x%08X" $((0x43C00000 + BYTE_OFFSET)))
                VALUE=$(devmem "$ADDR" 2>/dev/null) || VALUE="ERROR"

                if [ "$VALUE" != "ERROR" ]; then
                    READABLE=$((READABLE + 1))
                    # Annotate known registers
                    NOTE=""
                    case $OFFSET in
                        0)  NOTE="HARDWARE_VERSION" ;;
                        1)  NOTE="FAN_SPEED" ;;
                        2)  NOTE="HASH_ON_PLUG" ;;
                        3)  NOTE="BUFFER_SPACE" ;;
                        4)  NOTE="RETURN_NONCE_LO" ;;
                        5)  NOTE="RETURN_NONCE_HI" ;;
                        6)  NOTE="NONCE_FIFO_COUNT" ;;
                        7)  NOTE="NONCE_FIFO_IRQ" ;;
                        8)  NOTE="TEMP_0_3" ;;
                        9)  NOTE="TEMP_4_7" ;;
                        10) NOTE="TEMP_8_11" ;;
                        11) NOTE="TEMP_12_15" ;;
                        12) NOTE="IIC_COMMAND" ;;
                        13) NOTE="RESET_HASHBOARD" ;;
                        33) NOTE="FAN_CONTROL" ;;
                        34) NOTE="TIMEOUT_CTRL" ;;
                        35) NOTE="TICKET_MASK" ;;
                    esac
                    printf "0x%02X        0x%03X       %s  %s\n" "$OFFSET" "$BYTE_OFFSET" "$VALUE" "$NOTE"
                else
                    FAILED=$((FAILED + 1))
                fi

                OFFSET=$((OFFSET + 1))
            done

            echo ""
            echo "--- Summary ---"
            echo "Readable: ${READABLE}/${FPGA_SIZE_WORDS} words"
            echo "Failed: ${FAILED}/${FPGA_SIZE_WORDS} words"
        else
            echo "(devmem not available)"

            # Try reading via device node
            if [ -e "$FPGA_DEV" ]; then
                echo "Reading via ${FPGA_DEV}..."
                dd if="$FPGA_DEV" bs=352 count=1 2>/dev/null | xxd 2>/dev/null || echo "(read failed)"
            else
                echo "(no access method available for FPGA registers)"
            fi
        fi

    } > "$REPORT_DIR/fpga_registers.txt" 2>&1
    SIZE=$(file_size "$REPORT_DIR/fpga_registers.txt")
    log_pass "${SIZE} bytes"
fi

# ============================================================================
# Phase 4: ASIC Chain Enumeration
# ============================================================================

log_header "Phase 4: ASIC Chain Operations"

if [ $SKIP_ASIC -eq 1 ]; then
    log_step "ASIC chain enumeration"
    log_skip "disabled via --no-asic"
    echo '{"skipped": true, "reason": "--no-asic"}' > "$REPORT_DIR/asic_chain.json"
    echo '{"skipped": true, "reason": "--no-asic"}' > "$REPORT_DIR/register_dump.json"
    echo '{"skipped": true, "reason": "--no-asic"}' > "$REPORT_DIR/temperature.json"
else
    # ASIC Chain Enumeration
    log_step "ASIC chain enumeration (passive scan)"
    if [ -f "$TOOLS_DIR/asic_enumerator.py" ]; then
        python3 "$TOOLS_DIR/asic_enumerator.py" --passive --json \
            > "$REPORT_DIR/asic_chain.json" 2>/dev/null
        if [ $? -eq 0 ]; then
            # Extract chip count from JSON
            CHIP_COUNT=$(python3 -c "import json; d=json.load(open('$REPORT_DIR/asic_chain.json')); print(d.get('chip_count', 0))" 2>/dev/null || echo "?")
            log_pass "${CHIP_COUNT} chips found"
        else
            log_fail "enumeration error"
            echo '{"error": "enumeration failed"}' > "$REPORT_DIR/asic_chain.json"
        fi
    elif [ -e "$UART_DEV" ]; then
        log_skip "asic_enumerator.py not found"
        echo '{"skipped": true, "reason": "tool not found"}' > "$REPORT_DIR/asic_chain.json"
    else
        log_skip "no UART device"
        echo '{"skipped": true, "reason": "no UART device"}' > "$REPORT_DIR/asic_chain.json"
    fi

    # Register Scan
    if [ $QUICK_MODE -eq 1 ]; then
        log_step "Register scan on chip 0 (known registers only)"
        SCAN_OPTS="--known-only"
    else
        log_step "Register scan on chip 0 (full 0x00-0xFF)"
        SCAN_OPTS=""
    fi

    if [ -f "$TOOLS_DIR/register_scanner.py" ]; then
        python3 "$TOOLS_DIR/register_scanner.py" --chip 0x00 --json $SCAN_OPTS \
            > "$REPORT_DIR/register_dump.json" 2>/dev/null
        if [ $? -eq 0 ]; then
            SIZE=$(file_size "$REPORT_DIR/register_dump.json")
            log_pass "${SIZE} bytes"
        else
            log_fail "scan error"
            echo '{"error": "register scan failed"}' > "$REPORT_DIR/register_dump.json"
        fi
    else
        log_skip "register_scanner.py not found"
        echo '{"skipped": true, "reason": "tool not found"}' > "$REPORT_DIR/register_dump.json"
    fi

    # Temperature Discovery
    if [ $QUICK_MODE -eq 1 ]; then
        log_step "Temperature scan (quick, 2 rounds)"
        TEMP_OPTS="--rounds 2 --delay 1"
    else
        log_step "Temperature scan (5 rounds, 2s delay)"
        TEMP_OPTS="--rounds 5 --delay 2"
    fi

    if [ -f "$TOOLS_DIR/temp_finder.py" ]; then
        python3 "$TOOLS_DIR/temp_finder.py" --json $TEMP_OPTS \
            > "$REPORT_DIR/temperature.json" 2>/dev/null
        if [ $? -eq 0 ]; then
            SIZE=$(file_size "$REPORT_DIR/temperature.json")
            log_pass "${SIZE} bytes"
        else
            log_fail "temperature scan error"
            echo '{"error": "temperature scan failed"}' > "$REPORT_DIR/temperature.json"
        fi
    else
        log_skip "temp_finder.py not found"
        echo '{"skipped": true, "reason": "tool not found"}' > "$REPORT_DIR/temperature.json"
    fi
fi

# ============================================================================
# Phase 5: PSU Probe
# ============================================================================

log_header "Phase 5: PSU Probe"

log_step "Probing PSU via I2C/PMBus"
if [ -f "$TOOLS_DIR/psu_probe.py" ]; then
    python3 "$TOOLS_DIR/psu_probe.py" --scan --all --json \
        > "$REPORT_DIR/psu_data.json" 2>/dev/null
    if [ $? -eq 0 ]; then
        SIZE=$(file_size "$REPORT_DIR/psu_data.json")
        log_pass "${SIZE} bytes"
    else
        log_fail "PSU probe error"
        echo '{"error": "PSU probe failed"}' > "$REPORT_DIR/psu_data.json"
    fi
else
    log_skip "psu_probe.py not found"
    echo '{"skipped": true, "reason": "tool not found"}' > "$REPORT_DIR/psu_data.json"
fi

# ============================================================================
# Phase 6: Assumption Verification
# ============================================================================

log_header "Phase 6: Assumption Verification"

log_step "Running assumption verifier (--all --safe --json)"
if [ -f "$TOOLS_DIR/assumption_verifier.py" ]; then
    python3 "$TOOLS_DIR/assumption_verifier.py" --all --safe --json \
        > "$REPORT_DIR/assumptions.json" 2>/dev/null
    if [ $? -eq 0 ]; then
        # Extract pass/fail counts from JSON
        COUNTS=$(python3 -c "
import json, sys
try:
    d = json.load(open('$REPORT_DIR/assumptions.json'))
    s = d.get('summary', {})
    print('{}/{} passed, {} failed'.format(s.get('passed',0), s.get('total',0), s.get('failed',0)))
except:
    print('parse error')
" 2>/dev/null || echo "?")
        log_pass "$COUNTS"
    else
        # Non-zero exit = some tests failed, but we still have results
        SIZE=$(file_size "$REPORT_DIR/assumptions.json")
        if [ "$SIZE" -gt 10 ]; then
            log_pass "completed with failures (${SIZE} bytes)"
        else
            log_fail "verifier error"
            echo '{"error": "assumption verifier failed"}' > "$REPORT_DIR/assumptions.json"
        fi
    fi
else
    log_skip "assumption_verifier.py not found"
    echo '{"skipped": true, "reason": "tool not found"}' > "$REPORT_DIR/assumptions.json"
fi

# ============================================================================
# Phase 7: Generate Summary
# ============================================================================

log_header "Phase 7: Generating Summary"

log_step "Writing summary report"
{
    echo "==============================================================="
    echo "  DCENTos Comprehensive Diagnostic Report"
    echo "  Generated: $(date)"
    echo "  Report ID: ${TIMESTAMP}"
    echo "==============================================================="
    echo ""

    # System overview
    echo "--- System Overview ---"
    KERNEL=$(uname -r 2>/dev/null || echo "unknown")
    HOSTNAME=$(hostname 2>/dev/null || echo "unknown")
    UPTIME=$(uptime 2>/dev/null | sed 's/^ *//' || echo "unknown")
    MEM_TOTAL=$(grep MemTotal /proc/meminfo 2>/dev/null | awk '{print $2}' || echo "?")
    MEM_MB=$((${MEM_TOTAL:-0} / 1024))

    echo "  Hostname: ${HOSTNAME}"
    echo "  Kernel:   ${KERNEL}"
    echo "  Uptime:   ${UPTIME}"
    echo "  Memory:   ${MEM_MB} MB"
    echo ""

    # Device node check
    echo "--- Device Nodes ---"
    printf "  %-25s %s\n" "/dev/ttyPS0 (console):" "$([ -e /dev/ttyPS0 ] && echo 'PRESENT' || echo 'MISSING')"
    printf "  %-25s %s\n" "/dev/ttyPS1 (hash UART):" "$([ -e /dev/ttyPS1 ] && echo 'PRESENT' || echo 'MISSING')"
    UIO_COUNT=$(ls -d /sys/class/uio/uio* 2>/dev/null | wc -l)
    printf "  %-25s %s\n" "UIO devices:" "$UIO_COUNT found"
    printf "  %-25s %s\n" "/dev/mem:" "$([ -e /dev/mem ] && echo 'PRESENT' || echo 'MISSING')"

    I2C_COUNT=0
    for bus in 0 1 2 3 4 5 6 7; do
        if [ -e "/dev/i2c-${bus}" ]; then
            I2C_COUNT=$((I2C_COUNT + 1))
        fi
    done
    echo "  I2C buses found: ${I2C_COUNT}"
    echo ""

    # NAND partitions
    echo "--- NAND Partitions ---"
    if [ -f /proc/mtd ]; then
        PART_COUNT=$(grep -c '^mtd' /proc/mtd 2>/dev/null || echo 0)
        echo "  Partitions: ${PART_COUNT}"
        cat /proc/mtd 2>/dev/null | tail -n +2 | while read line; do
            echo "    $line"
        done
    else
        echo "  /proc/mtd not available"
    fi
    echo ""

    # File manifest
    echo "--- Report Files ---"
    for f in "$REPORT_DIR"/*; do
        if [ -f "$f" ]; then
            FNAME=$(basename "$f")
            FSIZE=$(file_size "$f")
            printf "  %-30s %8s bytes\n" "$FNAME" "$FSIZE"
        fi
    done
    echo ""

    # Assumption verification summary (if available)
    if [ -f "$REPORT_DIR/assumptions.json" ]; then
        echo "--- Assumption Verification Summary ---"
        python3 -c "
import json, sys
try:
    d = json.load(open('$REPORT_DIR/assumptions.json'))
    s = d.get('summary', {})
    print('  Total:   {}'.format(s.get('total', 0)))
    print('  Passed:  {}'.format(s.get('passed', 0)))
    print('  Failed:  {}'.format(s.get('failed', 0)))
    print('  Skipped: {}'.format(s.get('skipped', 0)))
    tests = d.get('tests', [])
    failed = [t for t in tests if t.get('status') == 'FAIL']
    if failed:
        print('')
        print('  FAILED TESTS:')
        for t in failed:
            print('    {} : {}'.format(t.get('test_id','?'), t.get('description','?')))
except Exception as e:
    print('  (could not parse assumptions.json: {})'.format(e))
" 2>/dev/null || echo "  (could not parse results)"
        echo ""
    fi

    # ASIC chain summary (if available)
    if [ -f "$REPORT_DIR/asic_chain.json" ]; then
        echo "--- ASIC Chain Summary ---"
        python3 -c "
import json, sys
try:
    d = json.load(open('$REPORT_DIR/asic_chain.json'))
    if d.get('skipped'):
        print('  Skipped: {}'.format(d.get('reason', 'unknown')))
    else:
        print('  Chips found:      {}'.format(d.get('chip_count', 0)))
        print('  Total cores:      {}'.format(d.get('total_cores', 0)))
        print('  Est. hashrate:    {} GH/s ({} TH/s)'.format(
            d.get('estimated_hashrate_ghs', 0),
            d.get('estimated_hashrate_ths', 0)))
except Exception as e:
    print('  (could not parse asic_chain.json: {})'.format(e))
" 2>/dev/null || echo "  (could not parse results)"
        echo ""
    fi

    # Diagnostic suite execution stats
    echo "--- Diagnostic Suite Stats ---"
    echo "  Steps completed: ${TOTAL_STEPS}"
    echo "  Passed:          ${PASS_COUNT}"
    echo "  Failed:          ${FAIL_COUNT}"
    echo "  Skipped:         ${SKIP_COUNT}"
    echo ""

    # Transfer instructions
    echo "==============================================================="
    echo "  To copy this report to your computer:"
    echo ""
    echo "  scp -r root@<miner-ip>:${REPORT_DIR}/ ./"
    echo ""
    echo "  Or via USB (if mounted at /mnt/usb):"
    echo "  cp -r ${REPORT_DIR}/ /mnt/usb/"
    echo "==============================================================="

} > "$REPORT_DIR/summary.txt" 2>&1
SIZE=$(file_size "$REPORT_DIR/summary.txt")
log_pass "${SIZE} bytes"

# ============================================================================
# Final Summary
# ============================================================================

printf "\n"
printf "${BOLD}╔══════════════════════════════════════════════════════════════╗${RESET}\n"
printf "${BOLD}║                    REPORT COMPLETE                          ║${RESET}\n"
printf "${BOLD}╚══════════════════════════════════════════════════════════════╝${RESET}\n"
printf "\n"
printf "  Report directory: ${BOLD}%s${RESET}\n" "$REPORT_DIR"
printf "\n"

# List all files with sizes
printf "  %-30s %10s\n" "FILE" "SIZE"
printf "  %-30s %10s\n" "------------------------------" "----------"
TOTAL_SIZE=0
for f in "$REPORT_DIR"/*; do
    if [ -f "$f" ]; then
        FNAME=$(basename "$f")
        FSIZE=$(file_size "$f")
        TOTAL_SIZE=$((TOTAL_SIZE + FSIZE))
        printf "  %-30s %10s bytes\n" "$FNAME" "$FSIZE"
    fi
done
printf "  %-30s %10s\n" "------------------------------" "----------"
printf "  %-30s %10s bytes\n" "TOTAL" "$TOTAL_SIZE"
printf "\n"

printf "  Steps: %d  |  " "$TOTAL_STEPS"
printf "${GREEN}Passed: %d${RESET}  |  " "$PASS_COUNT"
printf "${RED}Failed: %d${RESET}  |  " "$FAIL_COUNT"
printf "${YELLOW}Skipped: %d${RESET}\n" "$SKIP_COUNT"
printf "\n"

printf "  Transfer with:\n"
printf "  ${BOLD}scp -r root@<ip>:%s/ ./report/${RESET}\n" "$REPORT_DIR"
printf "\n"

# Exit with failure if any steps failed
if [ $FAIL_COUNT -gt 0 ]; then
    exit 1
fi
exit 0
