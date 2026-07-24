#!/bin/sh
# Prove that capture-first SKU rows cannot enter a live acceptance, deployment,
# install-guidance, or OTA path. The boot-log parser remains available because
# it is the offline evidence-ingestion route that can retire NOT-IMPLEMENTED.
set -u

here=$(CDPATH= cd "$(dirname "$0")" && pwd)
harness="$here/dcent-accept.sh"
producer_manifest="$here/../../docs/architecture/artifact_producers.json"

fails=0
bad() {
    printf 'FAIL: %s\n' "$*" >&2
    fails=$((fails + 1))
}

for phase in detect backup firstlight enum shares soak bench all ota install-hint; do
    output=$(sh "$harness" "$phase" S15 192.0.2.1 2>&1)
    rc=$?
    if [ "$rc" -ne 1 ]; then
        bad "$phase returned $rc instead of refusal rc=1"
    fi
    case "$output" in
        *"$phase is refused for NOT-IMPLEMENTED route S15 (am1-s15)"*) : ;;
        *) bad "$phase did not report the release-state refusal" ;;
    esac
    case "$output" in
        *"only offline bootlog diagnosis"*) : ;;
        *) bad "$phase did not point to the capture-first evidence route" ;;
    esac
done

boot_output=$(printf '%s\n' 'accepted share' | sh "$harness" bootlog S15 - 2>&1)
boot_rc=$?
if [ "$boot_rc" -ne 0 ]; then
    bad "bootlog returned $boot_rc instead of accepting offline evidence"
fi
case "$boot_output" in
    *"BOOT PASS: the capture reached mining"*) : ;;
    *) bad "bootlog did not process the capture-first evidence" ;;
esac

for route in \
    "S17Plus am2-s17plus" \
    "T17 am2-t17" \
    "T17Plus am2-t17plus" \
    "T19 am2-t19" \
    "S19XP am3-s19xp"
do
    set -- $route
    output=$(sh "$harness" install-hint "$1" 192.0.2.1 2>&1)
    rc=$?
    if [ "$rc" -ne 1 ]; then
        bad "$1 install-hint returned $rc instead of policy refusal rc=1"
    fi
    case "$output" in
        *"typed hardware matrix denies install and declares no artifact for $2"*) : ;;
        *) bad "$1 install-hint did not report its typed no-artifact policy" ;;
    esac
    case "$output" in
        *"dcent install "*) bad "$1 install-hint printed an install command" ;;
    esac
done

if grep -Fq 'artifact_package_target()' "$harness"; then
    bad "install-hint still carries a second hardcoded artifact alias table"
fi

for route in \
    "S9 am1-s9 dcentos-sysupgrade-118.tar managed_s9_install" \
    "S17 am2-s17p dcentos-sysupgrade-am2-s17pro.tar guarded_am2_self_update" \
    "S17Pro am2-s17p dcentos-sysupgrade-am2-s17pro.tar guarded_am2_self_update" \
    "S19 am2-s19pro dcentos-sysupgrade-am2-s19pro.tar guarded_am2_self_update" \
    "S19Pro am2-s19pro dcentos-sysupgrade-am2-s19pro.tar guarded_am2_self_update" \
    "S19jPro am2-s19j dcentos-sysupgrade-am2-s19jpro.tar guarded_am2_self_update" \
    "S19kPro am3-s19k dcentos-sysupgrade-am3-s19kpro.tar guarded_amlogic_rootfs_window" \
    "S21 am3-s21 dcentos-sysupgrade-am3-s21.tar guarded_amlogic_rootfs_window" \
    "T21 am3-t21 dcentos-sysupgrade-am3-t21.tar guarded_amlogic_rootfs_window" \
    "S21Pro am3-s21pro dcentos-sysupgrade-am3-s21pro.tar guarded_amlogic_rootfs_window" \
    "S21XP am3-s21xp dcentos-sysupgrade-am3-s21xp.tar guarded_amlogic_rootfs_window"
do
    set -- $route
    output=$(sh "$harness" install-hint "$1" 192.0.2.1 2>&1)
    rc=$?
    if [ "$rc" -ne 0 ]; then
        bad "$1 install-hint returned $rc despite its declared artifact lane"
    fi
    case "$output" in
        *"dcent install "*) : ;;
        *) bad "$1 install-hint omitted the admitted install command" ;;
    esac
    case "$output" in
        *"output/$3"*) : ;;
        *) bad "$1 install-hint did not print typed artifact filename $3" ;;
    esac
    producer_row=$(grep -F "\"board_target\":\"$2\"" "$producer_manifest")
    case "$producer_row" in
        *"\"artifact_filename\":\"$3\""*) : ;;
        *) bad "$1 expected filename is not bound to $2 in the producer manifest" ;;
    esac
    case "$producer_row" in
        *"\"install_contract\":\"$4\""*) : ;;
        *) bad "$1 expected install contract is not bound to $2 in the producer manifest" ;;
    esac
    case "$4" in
        managed_s9_install)
            case "$output" in
                *"--artifact-dir"*|*"--accept-am2-persistent"*|*"--accept-vnish-aml-rootfs-window"*)
                    bad "$1 managed S9 hint inherited a guarded-family argument"
                    ;;
            esac
            ;;
        guarded_am2_self_update)
            for required in \
                "--artifact-dir <restore_verified_dir>" \
                "--accept-am2-persistent-lab" \
                "--i-have-recovery" \
                "Vendor-source first install remains evidence-gap" \
                "does not auto-reboot"
            do
                case "$output" in
                    *"$required"*) : ;;
                    *) bad "$1 guarded AM2 hint omitted: $required" ;;
                esac
            done
            ;;
        guarded_amlogic_rootfs_window)
            for required in \
                "--artifact-dir <restore_verified_dir>" \
                "Stock source:" \
                "VNish source (additional source-specific acknowledgement):" \
                "--accept-vnish-aml-rootfs-window" \
                "There is no A/B rollback slot and no automatic reboot"
            do
                case "$output" in
                    *"$required"*) : ;;
                    *) bad "$1 guarded Amlogic hint omitted: $required" ;;
                esac
            done
            ;;
        *)
            bad "$1 test carries unknown install contract $4"
            ;;
    esac
    case "$output" in
        *"atomic bootslot flip"*|*"fw_setenv bootcmd \"run storeboot\""*)
            bad "$1 hint still advertises an unproven generic rollback"
            ;;
    esac
done

if [ "$fails" -eq 0 ]; then
    echo "PASS: release-state routes fail closed and install hints use typed producer contracts"
    exit 0
fi
echo "FAIL: $fails release-state route error(s)"
exit 1
