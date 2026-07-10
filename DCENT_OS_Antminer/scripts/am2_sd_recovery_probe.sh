#!/usr/bin/env bash
#
# Read-only external SD recovery probe for am2 Zynq (S19 Pro / S19j Pro)
# units. Run this AFTER booting a known-good DCENT_OS SD-card recovery image
# (the one produced by `build_am2_s19jpro_sd_disk_image.sh`).
#
# This probe collects identity, root-device, MTD, and mount evidence only.
# It does NOT upload files, stop services, write flash or bootenv, read raw
# MTD contents, touch hashboard buses, or alter pool configuration.
#
# Mirrors `am3_bb_sd_recovery_probe.sh` for the am2 platform: a passing
# probe is a precondition for `am2_nand_backup_execute.sh`, NOT for any
# write operation.

set -euo pipefail

usage() {
    local code="${1:-2}"
    cat >&2 <<'USAGE'
Usage:
  am2_sd_recovery_probe.sh <ip> [options]

Options:
  --artifact-dir <dir>       Evidence output dir (default: docs/dev/2026-05-15-am2-sd-recovery/evidence)
  --ssh-user <user>          SSH user (default: DCENT_AM2_RECOVERY_SSH_USER or root)
  --ssh-password-env <name>  Env var with SSH password (default: DCENT_PASSWORD)
  -h, --help                 Show this help.

Read-only contract:
  - No uploads, service restarts, process stops, flash writes, env writes,
    raw NAND/MTD reads, /dev/mem, /dev/i2c, UART, GPIO, PWM, or hashboard I/O.
  - MAC addresses, serials, URLs, users, passwords are redacted in evidence.
  - A passing probe does not authorize NAND writes or persistent installs.
USAGE
    exit "$code"
}

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$PROJECT_ROOT/../.." && pwd)"

ARTIFACT_DIR="$REPO_ROOT/docs/dev/2026-05-15-am2-sd-recovery/evidence"
SSH_USER="${DCENT_AM2_RECOVERY_SSH_USER:-root}"
SSH_PASSWORD_ENV="DCENT_PASSWORD"
IP=""

while [ $# -gt 0 ]; do
    case "$1" in
        --artifact-dir)
            ARTIFACT_DIR="${2:?--artifact-dir requires a path}"
            shift 2
            ;;
        --artifact-dir=*)
            ARTIFACT_DIR="${1#--artifact-dir=}"
            shift
            ;;
        --ssh-user)
            SSH_USER="${2:?--ssh-user requires a user}"
            shift 2
            ;;
        --ssh-user=*)
            SSH_USER="${1#--ssh-user=}"
            shift
            ;;
        --ssh-password-env)
            SSH_PASSWORD_ENV="${2:?--ssh-password-env requires a variable name}"
            shift 2
            ;;
        --ssh-password-env=*)
            SSH_PASSWORD_ENV="${1#--ssh-password-env=}"
            shift
            ;;
        -h|--help)
            usage 0
            ;;
        --*)
            echo "ERROR: unknown argument: $1" >&2
            usage
            ;;
        *)
            if [ -n "$IP" ]; then
                echo "ERROR: unexpected positional argument: $1" >&2
                usage
            fi
            IP="$1"
            shift
            ;;
    esac
done

[ -n "$IP" ] || usage

case "$IP" in
    ""|*[!A-Za-z0-9_.:-]*)
        echo "ERROR: unsafe host/IP token: $IP" >&2
        exit 2
        ;;
esac

mkdir -p "$ARTIFACT_DIR"

timestamp_utc() { date -u +"%Y-%m-%dT%H:%M:%SZ"; }
safe_name() { printf '%s' "$1" | tr -c 'A-Za-z0-9_.=-' '-'; }

redact_sensitive() {
    sed -E \
        -e 's#stratum\+tcp://[^[:space:]",|]+#stratum+tcp://<redacted>#g' \
        -e 's#stratum\+ssl://[^[:space:]",|]+#stratum+ssl://<redacted>#g' \
        -e 's#(URL=)[^,|]*#\1<redacted>#g' \
        -e 's#(User=)[^,|]*#\1<redacted>#g' \
        -e 's#(Password=)[^,|]*#\1<redacted>#g' \
        -e 's#(MACAddr=)[0-9A-Fa-f:]+#\1<redacted>#g' \
        -e 's#(SerialNumber=)[^,|]*#\1<redacted>#g' \
        -e 's#[0-9A-Fa-f]{2}(:[0-9A-Fa-f]{2}){5}#<mac:redacted>#g'
}

ssh_readonly() {
    local password="${!SSH_PASSWORD_ENV:-}"
    local ssh_opts=(
        -o StrictHostKeyChecking=no
        -o UserKnownHostsFile=/dev/null
        -o ConnectTimeout=8
        -o ServerAliveInterval=5
        -o ServerAliveCountMax=1
    )
    if [ -z "$password" ]; then
        ssh_opts+=(-o BatchMode=yes)
        ssh "${ssh_opts[@]}" "${SSH_USER}@${IP}" 'sh -s'
        return
    fi
    if ! command -v sshpass >/dev/null 2>&1; then
        echo "sshpass not installed; cannot use password env ${SSH_PASSWORD_ENV}" >&2
        return 127
    fi
    SSHPASS="$password" sshpass -e ssh "${ssh_opts[@]}" "${SSH_USER}@${IP}" 'sh -s'
}

record_remote_evidence() {
    ssh_readonly <<'REMOTE'
echo "=== remote timestamp ==="
date -u +"%Y-%m-%dT%H:%M:%SZ" 2>/dev/null || date 2>/dev/null || true
echo "=== safety contract ==="
echo "remote_script=read_only_no_upload_no_service_change_no_flash_write_no_raw_mtd_read"
echo "=== uname ==="
uname -a 2>/dev/null || true
echo "=== release ==="
cat /etc/os-release /etc/*release 2>/dev/null | sed -n '1,100p' || true
echo "=== proc cmdline ==="
cat /proc/cmdline 2>/dev/null || true
echo "=== device-tree model ==="
tr '\000' '\n' < /proc/device-tree/model 2>/dev/null || true
echo "=== device-tree compatible ==="
tr '\000' '\n' < /proc/device-tree/compatible 2>/dev/null || true
echo "=== cpu hints ==="
grep -E 'Hardware|Revision|model name|Processor' /proc/cpuinfo 2>/dev/null | sed -n '1,100p' || true
echo "=== root mount evidence ==="
awk '$2 == "/" { print }' /proc/mounts 2>/dev/null || true
findmnt / 2>/dev/null || true
echo "=== mounts ==="
cat /proc/mounts 2>/dev/null | sed -n '1,180p' || mount 2>/dev/null | sed -n '1,180p' || true
echo "=== df ==="
df -Pk / /boot /config /etc /tmp /mnt /data 2>/dev/null || true
echo "=== partitions ==="
cat /proc/partitions 2>/dev/null || true
echo "=== block devices ==="
ls -l /dev/mmcblk* /dev/sd* /dev/mtd* /dev/mtdblock* 2>/dev/null || true
echo "=== mtd layout ==="
cat /proc/mtd 2>/dev/null || true
echo "=== mtd sysfs names ==="
for f in /sys/class/mtd/mtd*/name; do
    [ -f "$f" ] || continue
    printf '%s=' "$f"
    cat "$f" 2>/dev/null
done
echo "=== fw-info (xil identification) ==="
cat /etc/fw-info 2>/dev/null || true
echo "=== bos_platform ==="
cat /etc/bos_platform 2>/dev/null || true
echo "=== flash tooling ==="
for t in fw_printenv fw_setenv nanddump nandwrite flash_erase ubinfo ubiattach ubidetach sha256sum; do
    if command -v "$t" >/dev/null 2>&1; then
        echo "tool:$t=present"
    else
        echo "tool:$t=missing"
    fi
done
echo "=== process hints ==="
(ps w 2>/dev/null || ps 2>/dev/null) | grep -E 'bosminer|bmminer|cgminer|dcentrald|dropbear|sshd' | grep -v grep || true
echo "=== listener hints ==="
(netstat -ltnp 2>/dev/null || netstat -ltn 2>/dev/null || ss -ltnp 2>/dev/null || ss -ltn 2>/dev/null) | sed -n '1,140p' || true
echo "=== dmesg sd/mtd hints ==="
dmesg 2>/dev/null | grep -Ei 'mmc|sdhci|nand|mtd|ubi|ubifs|zynq|xilinx|boot|rootfs' | tail -200 || true
REMOTE
}

expected_mtd_names_present() {
    local evidence="$1"
    local missing=0
    local name
    for name in boot boot-failover fpga1 fpga2 uboot_env miner_cfg recovery firmware1 firmware2 factory; do
        if ! grep -Eq "\"${name}\"|=${name}\$" "$evidence"; then
            echo "mtd_expected_name_${name}=missing"
            missing=1
        fi
    done
    if [ "$missing" -eq 0 ]; then
        echo "mtd_expected_names=pass"
        return 0
    fi
    echo "mtd_expected_names=fail"
    return 1
}

append_decision() {
    local evidence="$1"
    local identity=fail
    local external_boot=review
    local mtd_layout=fail
    local stock_xil=0

    {
        echo
        echo "=== am2 sd recovery decision ==="
        if grep -Eiq 'xlnx,zynq|zynq-7000|zynq.*am2|S19j|S19 Pro|am2-s19' "$evidence"; then
            identity=pass
            echo "identity=pass am2_zynq"
        else
            echo "identity=fail not_proven_am2_zynq"
        fi

        # Detect stock-XIL platform identity. The am2 SD recovery probe is
        # NOT a stock-XIL recovery contract — flag and refuse for that case.
        if grep -Eiq '^platform=xil|fw_info_platform=xil' "$evidence"; then
            stock_xil=1
            echo "stock_xil_detected=1"
            echo "warning=stock_xil_single_slot_layout_not_supported_by_this_probe"
        else
            echo "stock_xil_detected=0"
        fi

        if grep -Eq 'root=/dev/mmcblk|root=PARTUUID=|root=UUID=' "$evidence" ||
            grep -Eq '^/dev/mmcblk[0-9]p?[0-9]*[[:space:]]+/[[:space:]]' "$evidence"; then
            external_boot=pass
            echo "external_boot=pass root_appears_sd_or_partition_uuid"
        elif grep -Eq 'root=/dev/mtdblock|root=/dev/ubiblock' "$evidence"; then
            external_boot=fail
            echo "external_boot=fail root_appears_nand"
        else
            echo "external_boot=review root_source_not_classified"
        fi

        if expected_mtd_names_present "$evidence"; then
            mtd_layout=pass
        fi

        echo "nand_backup_execute_go=0"
        echo "nand_write_go=0"
        echo "persistent_install_go=0"

        if [ "$identity" = pass ] && [ "$external_boot" = pass ] && \
           [ "$mtd_layout" = pass ] && [ "$stock_xil" = 0 ]; then
            echo "sd_recovery_probe=pass"
            echo "next_step=run_am2_nand_backup_execute_with_restore_verified_artifact"
        else
            echo "sd_recovery_probe=not_ready"
            if [ "$stock_xil" = 1 ]; then
                echo "next_step=convert_unit_to_braiinsos_first_OR_use_stock_xil_recovery_path"
            else
                echo "next_step=review_evidence_before_any_backup_or_install_work"
            fi
        fi
    } >>"$evidence"
}

STAMP="$(date -u +"%Y%m%dT%H%M%SZ")"
EVIDENCE="$ARTIFACT_DIR/am2_$(safe_name "$IP")_${STAMP}_sd_recovery_probe.txt"

{
    echo "=== dcent os am2 sd recovery probe ==="
    echo "timestamp_utc=$(timestamp_utc)"
    echo "ip=$IP"
    echo "ssh_user=$SSH_USER"
    echo "ssh_password_env=$SSH_PASSWORD_ENV"
    echo "contract=read_only_external_boot_recovery_probe"
    echo
    record_remote_evidence
} | redact_sensitive | sed -E 's/[[:space:]]+$//' >"$EVIDENCE"

append_decision "$EVIDENCE"

echo "AM2 SD recovery evidence: $EVIDENCE"
tail -n 20 "$EVIDENCE"
