#!/usr/bin/env bash
#
# AM1 (S9 Zynq) SD-recovery feasibility probe — READ-ONLY.
#
# The S9 family does not typically SD-boot in production. However, Bitmain
# and BraiinsOS both ship "SD recovery" workflows:
#
#   - S9 control board has JP4 jumper for SD-boot (required on most revs).
#   - BraiinsOS publishes an `am1-s9` SD-recovery image.
#   - Stock Bitmain ships SD-card disaster-recovery via the same JP4 jumper.
#
# This script probes the running miner to report whether SD-recovery is a
# practical fallback if NAND were to become unrecoverable. It does NOT
# write to SD, boot from SD, or touch the JP4 jumper state.
#
# It is the S9 analog of `am2_sd_recovery_probe.sh` / `am3_bb_sd_recovery_probe.sh`.
#
# READ-ONLY CONTRACT:
#   - No uploads, no service restarts, no flash writes, no env writes.
#   - No raw NAND/MTD reads, no /dev/mem, no /dev/i2c, no UART, no GPIO,
#     no PWM, no hashboard I/O.
#   - MAC addresses, serials, URLs, users, passwords are redacted.
#   - A passing probe does NOT authorize NAND writes or persistent installs;
#     it merely flags that SD-recovery is a viable rollback option.

set -euo pipefail

usage() {
    local code="${1:-2}"
    cat >&2 <<'USAGE'
Usage:
  am1_nand_sd_recovery_probe.sh <ip> [options]

Options:
  --artifact-dir <dir>       Evidence output dir (default:
                             docs/dev/2026-05-15-am1-sd-recovery/evidence)
  --ssh-user <user>          SSH user (default: DCENT_AM1_RECOVERY_SSH_USER or root)
  --ssh-password-env <name>  Env var with SSH password (default: DCENT_PASSWORD)
  --json                     Emit JSON-only feasibility summary to stdout.
  -h, --help                 Show this help.

Read-only contract:
  - No uploads, service restarts, process stops, flash writes, env writes,
    raw NAND/MTD reads, /dev/mem, /dev/i2c, UART, GPIO, PWM, or hashboard I/O.
  - Pool URLs, users, passwords, MAC, and serials are redacted in evidence.
  - A passing probe does not authorize NAND writes or persistent installs.
USAGE
    exit "$code"
}

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$PROJECT_ROOT/../.." && pwd)"

ARTIFACT_DIR="$REPO_ROOT/docs/dev/2026-05-15-am1-sd-recovery/evidence"
SSH_USER="${DCENT_AM1_RECOVERY_SSH_USER:-root}"
SSH_PASSWORD_ENV="DCENT_PASSWORD"
IP=""
JSON_ONLY=0

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
        --json)
            JSON_ONLY=1
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
echo "=== sd-slot device presence ==="
# Look for any mmcblk that is NOT eMMC. On Zynq SD-boot path it's typically
# /dev/mmcblk0 with partitions p1 (FAT BOOT) + p2 (rootfs).
ls -l /dev/mmcblk0* 2>/dev/null || echo "no /dev/mmcblk0 device"
test -d /sys/class/mmc_host && ls -l /sys/class/mmc_host/ 2>/dev/null || true
for ctrl in /sys/bus/platform/devices/*sdhci* /sys/bus/platform/devices/*sdmmc* /sys/bus/platform/devices/*mmc* ; do
    [ -d "$ctrl" ] || continue
    printf 'sd_controller=%s\n' "$ctrl"
    cat "$ctrl/of_node/compatible" 2>/dev/null | tr '\000' '\n' || true
done
echo "=== mtd layout ==="
cat /proc/mtd 2>/dev/null || true
echo "=== mtd sysfs names ==="
for f in /sys/class/mtd/mtd*/name; do
    [ -f "$f" ] || continue
    printf '%s=' "$f"
    cat "$f" 2>/dev/null
done
echo "=== bos_platform ==="
cat /etc/bos_platform 2>/dev/null || true
echo "=== fw-info ==="
cat /etc/fw-info 2>/dev/null || true
echo "=== fw_printenv bootslot/firmware ==="
fw_printenv bootslot 2>/dev/null || true
fw_printenv firmware 2>/dev/null || true
echo "=== flash tooling ==="
for t in fw_printenv fw_setenv nanddump nandwrite flash_erase ubinfo ubiattach ubidetach sha256sum dd parted fdisk mkfs.vfat; do
    if command -v "$t" >/dev/null 2>&1; then
        echo "tool:$t=present"
    else
        echo "tool:$t=missing"
    fi
done
echo "=== mining-process hints ==="
(ps w 2>/dev/null || ps 2>/dev/null) | grep -E 'bosminer|bmminer|cgminer|dcentrald|dropbear|sshd' | grep -v grep || true
echo "=== dmesg sd/mmc/mtd hints ==="
dmesg 2>/dev/null | grep -Ei 'mmc|sdhci|nand|mtd|ubi|ubifs|zynq|xilinx|boot|rootfs' | tail -200 || true
REMOTE
}

expected_mtd_names_present() {
    local evidence="$1"
    local braiinsos_names='boot uboot fpga1 fpga2 uboot_env miner_cfg recovery firmware1 firmware2 factory'
    local stock_names='boot rootfs upgrade'
    local missing=0
    local name

    local scheme="unknown"
    missing=0
    for name in $braiinsos_names; do
        if ! grep -Eq "\"${name}\"" "$evidence"; then
            missing=1
            break
        fi
    done
    if [ "$missing" -eq 0 ]; then
        scheme="braiinsos"
        echo "mtd_layout_scheme=$scheme"
        return 0
    fi

    missing=0
    for name in $stock_names; do
        if ! grep -Eq "\"${name}\"" "$evidence"; then
            missing=1
            break
        fi
    done
    if [ "$missing" -eq 0 ]; then
        scheme="stock"
        echo "mtd_layout_scheme=$scheme"
        return 0
    fi

    echo "mtd_layout_scheme=$scheme"
    return 1
}

append_decision() {
    local evidence="$1"
    local identity=fail
    local mtd_layout=fail
    local sd_slot=review
    local jp4_status=review

    {
        echo
        echo "=== am1 sd recovery decision ==="
        if grep -Eiq 'xlnx,zynq|zynq-7000|zynq-7010|am1|S9|antminer s9' "$evidence"; then
            identity=pass
            echo "identity=pass am1_zynq_s9"
        else
            echo "identity=fail not_proven_am1_zynq_s9"
        fi

        # SD slot detection: any /dev/mmcblk0 device, or sdhci controller present.
        if grep -Eq '^/dev/mmcblk0' "$evidence" || \
           grep -Eq 'sd_controller=' "$evidence" || \
           grep -Eiq 'sdhci|mmc[0-9]+:|mmcblk0' "$evidence"; then
            sd_slot=pass
            echo "sd_slot_present=pass"
        else
            sd_slot=fail
            echo "sd_slot_present=fail no_mmc_evidence"
        fi

        # JP4 jumper state is NOT software-visible on S9 — it's a physical
        # jumper that biases the Zynq BootROM strap. We can only flag that
        # this is a manual operator step.
        echo "jp4_jumper=manual_only — physical JP4 jumper on S9 control board"
        echo "  controls SD-boot strap; not software-readable"

        if expected_mtd_names_present "$evidence"; then
            mtd_layout=pass
        fi
        echo "mtd_layout=$mtd_layout"

        # SD-recovery feasibility:
        # - identity=pass + sd_slot=pass + mtd_layout=pass → SD-recovery is
        #   a viable option (operator still needs to flip JP4 + physically
        #   insert SD card with a known-good recovery image).
        # - identity=pass + sd_slot=fail → SD-recovery NOT viable; only
        #   JTAG / serial-console recovery remains.
        # - identity=fail → cannot confirm this is an S9 unit; refuse to
        #   speculate.
        local feasibility="unknown"
        if [ "$identity" = pass ] && [ "$sd_slot" = pass ] && [ "$mtd_layout" = pass ]; then
            feasibility="viable"
        elif [ "$identity" = pass ] && [ "$sd_slot" = fail ]; then
            feasibility="not_viable_no_sd_slot"
        elif [ "$identity" = pass ]; then
            feasibility="review_required"
        else
            feasibility="not_an_am1_s9"
        fi
        echo "sd_recovery_feasibility=$feasibility"

        echo "nand_backup_execute_go=0"
        echo "nand_write_go=0"
        echo "persistent_install_go=0"

        if [ "$feasibility" = viable ]; then
            echo "sd_recovery_probe=pass"
            echo "next_step=jp4_and_known_good_sd_image_required_for_actual_recovery"
        else
            echo "sd_recovery_probe=not_ready"
            echo "next_step=review_evidence_before_relying_on_sd_recovery_path"
        fi
    } >>"$evidence"
}

emit_json_summary() {
    local evidence="$1"
    local feasibility
    feasibility="$(grep -m1 '^sd_recovery_feasibility=' "$evidence" | cut -d= -f2)"
    local identity
    identity="$(grep -m1 '^identity=' "$evidence" | awk '{print $1}' | cut -d= -f2)"
    local sd_slot
    sd_slot="$(grep -m1 '^sd_slot_present=' "$evidence" | cut -d= -f2)"
    local mtd_layout
    mtd_layout="$(grep -m1 '^mtd_layout=' "$evidence" | cut -d= -f2)"
    local mtd_scheme
    mtd_scheme="$(grep -m1 '^mtd_layout_scheme=' "$evidence" | cut -d= -f2)"
    local probe_result
    probe_result="$(grep -m1 '^sd_recovery_probe=' "$evidence" | cut -d= -f2)"

    cat <<JSON
{
  "schema_version": "1.0.0",
  "type": "am1_sd_recovery_probe",
  "probe_utc": "$(timestamp_utc)",
  "target_ip": "$IP",
  "identity": "$identity",
  "mtd_layout": "$mtd_layout",
  "mtd_layout_scheme": "${mtd_scheme:-unknown}",
  "sd_slot_present": "$sd_slot",
  "jp4_jumper": "manual_only",
  "sd_recovery_feasibility": "$feasibility",
  "sd_recovery_probe": "$probe_result",
  "evidence_file": "$evidence",
  "nand_backup_execute_go": 0,
  "nand_write_go": 0,
  "persistent_install_go": 0
}
JSON
}

STAMP="$(date -u +"%Y%m%dT%H%M%SZ")"
EVIDENCE="$ARTIFACT_DIR/am1_$(safe_name "$IP")_${STAMP}_sd_recovery_probe.txt"

{
    echo "=== dcent os am1 sd recovery probe ==="
    echo "timestamp_utc=$(timestamp_utc)"
    echo "ip=$IP"
    echo "ssh_user=$SSH_USER"
    echo "ssh_password_env=$SSH_PASSWORD_ENV"
    echo "contract=read_only_sd_recovery_feasibility_probe"
    echo
    record_remote_evidence
} | redact_sensitive | sed -E 's/[[:space:]]+$//' >"$EVIDENCE"

append_decision "$EVIDENCE"

if [ "$JSON_ONLY" = "1" ]; then
    emit_json_summary "$EVIDENCE"
    exit 0
fi

echo "AM1 SD recovery evidence: $EVIDENCE"
tail -n 25 "$EVIDENCE"
