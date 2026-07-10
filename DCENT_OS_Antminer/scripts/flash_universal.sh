#!/bin/bash
#
# flash_universal.sh — Legacy installer wrapper shim (auto-detect firmware)
# D-Central Technologies, 2026
#
# Historical auto-detect wrapper for AM1/S9 engineering workflows.
# It is retained to route users away from unsafe direct-flash paths and toward
# safe SD/sysupgrade/runtime-validation flows.
#
# Auto-detects source firmware and then refuses unsafe legacy flash routes.
#
# Usage:
#   ./flash_universal.sh <miner_ip> [OPTIONS]
#
# Examples:
#   ./flash_universal.sh 203.0.113.97           # Auto-detect, default creds
#   ./flash_universal.sh 203.0.113.50 --force vnish
#   ./flash_universal.sh 203.0.113.100 --password admin
#
# Options:
#   --password <pass>     SSH password to try first
#   --force <firmware>    Skip detection: braiinsos, vnish, stock, dcentos
#   --images-dir <dir>    Path to DCENTos build images
#   --help                Show this help
#

set -e

# =============================================================================
# Configuration
# =============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MINER_IP=""
PASSWORD=""
FORCE_FW=""
IMAGES_DIR=""
EXTRA_ARGS=""
CONFIG_FILE=""

# Credential sets to try (in order)
# Format: user:password:description
CRED_LIST="
root::BraiinsOS (empty password)
root:dcentral:DCENTos
root:admin:VNish
root:root:VNish (alt)
miner:miner:Stock Bitmain
"

# Colors
RED='\033[1;31m'
GREEN='\033[1;32m'
YELLOW='\033[1;33m'
CYAN='\033[1;36m'
BOLD='\033[1m'
NC='\033[0m'

info()   { echo -e "${GREEN}[INFO]${NC} $*"; }
warn()   { echo -e "${YELLOW}[WARN]${NC} $*"; }
error()  { echo -e "${RED}[ERROR]${NC} $*"; exit 1; }
header() { echo -e "\n${CYAN}${BOLD}=== $* ===${NC}"; }

# SSH with legacy crypto support
SSH_BASE_OPTS="-o StrictHostKeyChecking=no -o ConnectTimeout=5"
SSH_LEGACY_OPTS="$SSH_BASE_OPTS -o KexAlgorithms=+diffie-hellman-group14-sha1,diffie-hellman-group1-sha1"
SSH_LEGACY_OPTS="$SSH_LEGACY_OPTS -o HostKeyAlgorithms=+ssh-rsa,ssh-dss"
SSH_LEGACY_OPTS="$SSH_LEGACY_OPTS -o PubkeyAcceptedAlgorithms=+ssh-rsa"
SSH_LEGACY_OPTS="$SSH_LEGACY_OPTS -o Ciphers=+aes128-ctr,aes128-cbc,3des-cbc"

NODE_SSH="$(dirname "$SCRIPT_DIR")/../tools/ssh_cmd.js"

# =============================================================================
# Parse Arguments
# =============================================================================

usage() {
    echo "Usage: $(basename "$0") <miner_ip> [OPTIONS]"
    echo ""
    echo "Legacy AM1/S9 wrapper — auto-detects current firmware and routes to safe paths."
    echo ""
    echo "Options:"
    echo "  --password <pass>     SSH password to try first"
    echo "  --force <firmware>    Skip detection: braiinsos, vnish, stock, dcentos"
    echo "  --images-dir <dir>    Path to build images directory"
    echo "  --config <file>       dcentrald config for AM2/Amlogic runtime routing"
    echo "  --help                Show this help"
    echo ""
    echo "Supported firmware auto-detection:"
    echo "  AM1/S9     → signed install/update paths"
    echo "  AM2        → runtime-only validation path"
    echo "  Amlogic    → runtime-only validation path"
}

while [ $# -gt 0 ]; do
    case "$1" in
        --password)    PASSWORD="$2"; shift 2 ;;
        --force)       FORCE_FW="$2"; shift 2 ;;
        --images-dir)  IMAGES_DIR="$2"; EXTRA_ARGS="$EXTRA_ARGS --images-dir $2"; shift 2 ;;
        --config)      CONFIG_FILE="$2"; shift 2 ;;
        --help|-h)     usage; exit 0 ;;
        -*)            error "Unknown option: $1" ;;
        *)
            if [ -z "$MINER_IP" ]; then
                MINER_IP="$1"
            else
                error "Unexpected argument: $1"
            fi
            shift ;;
    esac
done

[ -n "$MINER_IP" ] || { usage; exit 1; }

# =============================================================================
# SSH Helper
# =============================================================================

try_ssh() {
    local user="$1"
    local pass="$2"
    local cmd="$3"

    if [ -z "$pass" ]; then
        # Empty password
        ssh $SSH_BASE_OPTS "$user@$MINER_IP" "$cmd" 2>/dev/null
    elif command -v sshpass >/dev/null 2>&1; then
        sshpass -p "$pass" ssh $SSH_LEGACY_OPTS "$user@$MINER_IP" "$cmd" 2>/dev/null
    elif [ -f "$NODE_SSH" ]; then
        node "$NODE_SSH" "$MINER_IP" "$user" "$pass" "$cmd" 2>/dev/null
    else
        # Try native ssh (will prompt for password — won't work in scripts)
        ssh $SSH_LEGACY_OPTS "$user@$MINER_IP" "$cmd" 2>/dev/null
    fi
}

# =============================================================================
# Network Check
# =============================================================================

header "DCENTos Universal Installer"

info "Target: $MINER_IP"

# Ping check
if ping -c 1 -W 2 "$MINER_IP" >/dev/null 2>&1; then
    pass() { echo -e "${GREEN}[OK]${NC}   $*"; }
    pass "Miner is reachable"
else
    warn "Miner does not respond to ping (may still be accessible via SSH)"
fi

# =============================================================================
# Detect Firmware
# =============================================================================

header "Detecting Firmware"

DETECTED_FW=""
DETECTED_USER=""
DETECTED_PASS=""
DETECTED_VER=""

if [ -n "$FORCE_FW" ]; then
    DETECTED_FW="$FORCE_FW"
    info "Forced firmware type: $FORCE_FW"

    # Set default credentials for forced mode
    case "$FORCE_FW" in
        braiinsos)  DETECTED_USER="root"; DETECTED_PASS="" ;;
        vnish)      DETECTED_USER="root"; DETECTED_PASS="${PASSWORD:-admin}" ;;
        stock)      DETECTED_USER="miner"; DETECTED_PASS="${PASSWORD:-miner}" ;;
        dcentos)    DETECTED_USER="root"; DETECTED_PASS="${PASSWORD:-dcentral}" ;;
        *)          error "Unknown firmware type: $FORCE_FW (use: braiinsos, vnish, stock, dcentos)" ;;
    esac
else
    # Try each credential set and detect firmware
    info "Trying SSH credentials..."

    # If user specified a password, try it first
    if [ -n "$PASSWORD" ]; then
        CRED_LIST="root:$PASSWORD:User-specified
$CRED_LIST"
    fi

    echo "$CRED_LIST" | while IFS=: read -r user pass desc; do
        [ -n "$user" ] || continue
        printf "  Trying %s (%s)... " "$user:${pass:-(empty)}" "$desc"

        RESULT=$(try_ssh "$user" "$pass" '
            if [ -f /etc/bos_version ]; then
                echo "FW=braiinsos"
                echo "VER=$(cat /etc/bos_version | head -1)"
            elif pidof dashd >/dev/null 2>&1; then
                echo "FW=vnish"
                echo "VER=$(cat /etc/anthill_version 2>/dev/null || echo unknown)"
            elif [ -f /etc/dcentos-version ]; then
                echo "FW=dcentos"
                echo "VER=$(cat /etc/dcentos-version)"
            elif pidof bmminer >/dev/null 2>&1; then
                echo "FW=stock"
                echo "VER=stock"
            else
                echo "FW=unknown"
                echo "VER=unknown"
            fi
        ' 2>/dev/null) || true

        if [ -n "$RESULT" ]; then
            FW=$(echo "$RESULT" | grep "^FW=" | head -1 | cut -d= -f2)
            VER=$(echo "$RESULT" | grep "^VER=" | head -1 | cut -d= -f2)
            if [ -n "$FW" ]; then
                echo -e "${GREEN}CONNECTED${NC}"
                # Write to temp file since we're in a subshell
                echo "$FW" > /tmp/_dcentos_fw_detect
                echo "$user" > /tmp/_dcentos_fw_user
                echo "$pass" > /tmp/_dcentos_fw_pass
                echo "$VER" > /tmp/_dcentos_fw_ver
                break
            fi
        fi
        echo "no"
    done

    # Read detection results from temp files
    if [ -f /tmp/_dcentos_fw_detect ]; then
        DETECTED_FW=$(cat /tmp/_dcentos_fw_detect)
        DETECTED_USER=$(cat /tmp/_dcentos_fw_user)
        DETECTED_PASS=$(cat /tmp/_dcentos_fw_pass)
        DETECTED_VER=$(cat /tmp/_dcentos_fw_ver)
        rm -f /tmp/_dcentos_fw_detect /tmp/_dcentos_fw_user /tmp/_dcentos_fw_pass /tmp/_dcentos_fw_ver
    fi
fi

if [ -z "$DETECTED_FW" ]; then
    echo ""
    error "Could not connect to $MINER_IP via SSH.\n\nPossible issues:\n  1. Miner is offline\n  2. SSH is disabled (VNish: enable via web → ssh.cgi)\n  3. Wrong credentials\n  4. Firewall blocking port 22\n\nSafe installation alternatives:\n  - Standalone SD: ./build_sd_image.sh + ./write_sd_card.sh\n  - Sysupgrade package: ./package_sysupgrade.sh"
fi

echo ""
info "Detected firmware: $DETECTED_FW ($DETECTED_VER)"
info "Credentials: $DETECTED_USER / ${DETECTED_PASS:-(empty)}"

header "Validating Board Family"
PLATFORM_INFO=$(try_ssh "$DETECTED_USER" "$DETECTED_PASS" '
    echo "ARCH=$(uname -m 2>/dev/null || echo unknown)"
    MODEL="$(cat /config/CONF_MINER_TYPE 2>/dev/null || cat /proc/device-tree/model 2>/dev/null || echo)"
    HWID="$(cat /config/CONF_HARDWARE_ID 2>/dev/null || echo)"
    if [ -f /sys/devices/soc0/soc_id ]; then
        echo "SOC=$(cat /sys/devices/soc0/soc_id 2>/dev/null)"
    elif grep -q zynq /proc/cpuinfo 2>/dev/null; then
        echo "SOC=zynq"
    else
        echo "SOC=unknown"
    fi
    echo "UIO_COUNT=$(find /sys/class/uio -maxdepth 1 -name "uio*" 2>/dev/null | wc -l)"
' 2>/dev/null) || true
eval "$PLATFORM_INFO" 2>/dev/null || true
ARCH_LC=$(printf '%s' "${ARCH:-unknown}" | tr '[:upper:]' '[:lower:]')
MODEL_LC=$(printf '%s' "${MODEL:-}" | tr '[:upper:]' '[:lower:]')
HWID_LC=$(printf '%s' "${HWID:-}" | tr '[:upper:]' '[:lower:]')
SOC_LC=$(printf '%s' "${SOC:-unknown}" | tr '[:upper:]' '[:lower:]')

BOARD_FAMILY="am1"
if [[ "$ARCH_LC" == *"aarch64"* ]] || [[ "$SOC_LC" == *"amlogic"* ]] || [[ "$MODEL_LC" == *"amlogic"* ]]; then
    BOARD_FAMILY="amlogic"
elif [[ "$MODEL_LC" == *"s17"* ]] || [[ "$MODEL_LC" == *"s19"* ]] || [[ "$HWID_LC" == *"am2"* ]] || [ "${UIO_COUNT:-0}" -ge 19 ]; then
    BOARD_FAMILY="am2"
fi

if [ "$BOARD_FAMILY" = "am1" ]; then
    info "Target validated as AM1/S9-style Zynq board"
else
    warn "Detected experimental board family: $BOARD_FAMILY"
fi

# =============================================================================
# Route to Appropriate Installer
# =============================================================================

header "Installing DCENTos"

if [ "$BOARD_FAMILY" != "am1" ]; then
    if [ "$DETECTED_USER" != "root" ]; then
        error "Runtime-only $BOARD_FAMILY workflow requires root SSH. Connected as '$DETECTED_USER' instead. Unlock/enable root SSH first."
    fi

    info "Routing $BOARD_FAMILY to runtime-only validation via dev_deploy.sh"
    if [ -n "$CONFIG_FILE" ]; then
        exec "$SCRIPT_DIR/dev_deploy.sh" "$MINER_IP" --config "$CONFIG_FILE" --verify
    else
        exec "$SCRIPT_DIR/dev_deploy.sh" "$MINER_IP" --verify
    fi
fi

case "$DETECTED_FW" in
    braiinsos)
        error "Unsafe BraiinsOS active-rootfs flashing is disabled. Use package_sysupgrade.sh or a safe SD/NAND installation path."
        ;;

    vnish)
        error "Unsafe VNish flashing is disabled. Migrate through a safe sysupgrade or SD workflow instead of writing active firmware paths over SSH."
        ;;

    stock)
        error "Unsafe stock Bitmain active-NAND flashing is disabled. Use SD boot or migrate to a supported inactive-slot sysupgrade workflow first."
        ;;

    dcentos)
        error "Legacy flash_update.sh is disabled because it bypasses A/B safety. Use the supported inactive-slot sysupgrade flow instead."
        ;;

    unknown)
        error "Unknown firmware will not be flashed through the legacy active-rootfs path. Use SD boot or a verified sysupgrade workflow first."
        ;;

    *)
        error "Unsupported firmware type: $DETECTED_FW"
        ;;
esac
