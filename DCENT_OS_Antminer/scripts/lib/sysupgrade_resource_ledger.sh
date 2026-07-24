#!/bin/sh
# Offline ownership-ledger core for Zynq sysupgrade external resources.
#
# This v2 format is deliberately incompatible with v1.  There is no migration
# path: encountering a legacy binding or resource receipt fails closed and
# leaves it in place for explicit recovery tooling.
#
# Public owner API:
#   dcent_sysupgrade_ledger_create DIR TX BOOT PID START MNT_NS LOCK LOCK_DEVINO
#   dcent_sysupgrade_ledger_open_owned DIR TX BOOT PID START MNT_NS LOCK LOCK_DEVINO
#   dcent_sysupgrade_ledger_resource_pending KIND ID created|borrowed A B C
#   dcent_sysupgrade_ledger_resource_active KIND ID EVIDENCE_SHA256
#   dcent_sysupgrade_ledger_resource_release_pending KIND ID EVIDENCE_SHA256
#   dcent_sysupgrade_ledger_resource_released KIND ID EVIDENCE_SHA256
#   dcent_sysupgrade_ledger_resource_absent_released KIND ID EVIDENCE_SHA256
#   dcent_sysupgrade_ledger_resource_conflict KIND ID EVIDENCE_SHA256
#   dcent_sysupgrade_ledger_resource_expect KIND ID PROVENANCE PHASE EVIDENCE A B C
#
# Public reconciler API:
#   dcent_sysupgrade_ledger_reconcile_claim DIR TX CLAIM BOOT PID START MNT_NS \
#       OWNER_DEATH_SHA256 MAINTENANCE_LOCK MAINTENANCE_LOCK_DEVINO
#   dcent_sysupgrade_ledger_reconcile_open DIR TX CLAIM BOOT PID START MNT_NS
#   dcent_sysupgrade_ledger_reconcile_quiescent CLAIM QUIESCENCE_SHA256
#   dcent_sysupgrade_ledger_reconcile_begin CLAIM QUIESCENCE_SHA256
#   dcent_sysupgrade_ledger_reconcile_complete CLAIM QUIESCENCE_SHA256 RESULT_SHA256
#   dcent_sysupgrade_ledger_reconcile_block CLAIM BLOCK_EVIDENCE_SHA256
#
# Intent and status are physically separate.  Each resource has one immutable
# `intent` receipt and an append-only, digest-chained status sequence.  Logical
# status is mutable only by appending the next legal revision; an interrupted
# append or rollback leaves a malformed resource that blocks every operation.
#
# Evidence digests authenticate caller-managed, byte-exact observation
# receipts.  This generic core deliberately does not interpret sysfs, devtmpfs,
# mountinfo, workspace, process-liveness, or maintenance-quiescence evidence.
# Resource-specific managers must do that before invoking a transition.
#
# DIR must be the exact `ledger` child of the transaction-lock path.  Production
# integration MUST obtain it from dcent_sysupgrade_lock_ledger_path.  This core
# validates the recorded lock directory identity but does not acquire either
# the transaction lock or maintenance lock itself.

DCENT_SYSUPGRADE_LEDGER_BOUND=0
DCENT_SYSUPGRADE_LEDGER_ACTOR=
DCENT_SYSUPGRADE_LEDGER_DIR=
DCENT_SYSUPGRADE_LEDGER_TRANSACTION_ID=
DCENT_SYSUPGRADE_LEDGER_BOOT_ID=
DCENT_SYSUPGRADE_LEDGER_OWNER_PID=
DCENT_SYSUPGRADE_LEDGER_OWNER_STARTTIME=
DCENT_SYSUPGRADE_LEDGER_OWNER_MOUNT_NAMESPACE=
DCENT_SYSUPGRADE_LEDGER_LOCK_PATH=
DCENT_SYSUPGRADE_LEDGER_LOCK_DEVICE_INODE=
DCENT_SYSUPGRADE_LEDGER_BINDING_SHA256=
DCENT_SYSUPGRADE_LEDGER_CLAIM_ID=
DCENT_SYSUPGRADE_LEDGER_RECONCILER_BOOT_ID=
DCENT_SYSUPGRADE_LEDGER_RECONCILER_PID=
DCENT_SYSUPGRADE_LEDGER_RECONCILER_STARTTIME=
DCENT_SYSUPGRADE_LEDGER_RECONCILER_MOUNT_NAMESPACE=

dcent_sysupgrade_ledger_fail()
{
    printf '%s\n' "sysupgrade-resource-ledger: ERROR: $*" >&2
    return 1
}

# Pure policy boundary for unprivileged host tests.  Production leaves this at
# uid 0; focused tests may override it after sourcing the helper.
dcent_sysupgrade_ledger_expected_uid()
{
    printf '%s\n' 0
}

dcent_sysupgrade_ledger_stat()
{
    stat -c "$1" -- "$2" 2>/dev/null
}

dcent_sysupgrade_ledger_safe_id()
{
    [ "$#" -eq 1 ] || return 1
    case "$1" in
        ''|.*|*[!A-Za-z0-9._-]*) return 1 ;;
    esac
}

dcent_sysupgrade_ledger_boot_id_valid()
{
    [ "$#" -eq 1 ] || return 1
    case "$1" in
        ''|*[!0-9A-Fa-f-]*) return 1 ;;
    esac
    [ "${#1}" -eq 36 ] || return 1
    case "$1" in
        ????????-????-????-????-????????????) ;;
        *) return 1 ;;
    esac
}

dcent_sysupgrade_ledger_uint_valid()
{
    [ "$#" -eq 1 ] || return 1
    case "$1" in
        ''|*[!0-9]*|0[0-9]*) return 1 ;;
    esac
}

dcent_sysupgrade_ledger_devino_valid()
{
    [ "$#" -eq 1 ] || return 1
    case "$1" in
        *:*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_dev=${1%%:*}
    _dcent_ledger_ino=${1#*:}
    [ "$_dcent_ledger_ino" = "${1##*:}" ] || return 1
    dcent_sysupgrade_ledger_uint_valid "$_dcent_ledger_dev" &&
        dcent_sysupgrade_ledger_uint_valid "$_dcent_ledger_ino"
}

dcent_sysupgrade_ledger_sha256_valid()
{
    [ "$#" -eq 1 ] || return 1
    [ "${#1}" -eq 64 ] || return 1
    case "$1" in
        *[!0-9a-f]*) return 1 ;;
    esac
}

dcent_sysupgrade_ledger_sha256_or_dash_valid()
{
    [ "$#" -eq 1 ] || return 1
    [ "$1" = - ] || dcent_sysupgrade_ledger_sha256_valid "$1"
}

dcent_sysupgrade_ledger_digest_file()
{
    [ "$#" -eq 1 ] || return 1
    _dcent_ledger_digest_output=$(sha256sum "$1" 2>/dev/null) || return 1
    _dcent_ledger_digest=${_dcent_ledger_digest_output%% *}
    dcent_sysupgrade_ledger_sha256_valid "$_dcent_ledger_digest" || return 1
    printf '%s\n' "$_dcent_ledger_digest"
}

dcent_sysupgrade_ledger_absolute_path_valid()
{
    [ "$#" -eq 1 ] || return 1
    case "$1" in
        /*) ;;
        *) return 1 ;;
    esac
    case "$1" in
        /|*//*|*/./*|*/../*|*/.|*/..|*[!A-Za-z0-9._/@:+-]*) return 1 ;;
    esac
}

dcent_sysupgrade_ledger_identity_valid()
{
    [ "$#" -eq 4 ] || return 1
    _dcent_ledger_kind=$1
    _dcent_ledger_a=$2
    _dcent_ledger_b=$3
    _dcent_ledger_c=$4
    case "$_dcent_ledger_kind" in
        attachment)
            dcent_sysupgrade_ledger_uint_valid "$_dcent_ledger_a" &&
                dcent_sysupgrade_ledger_uint_valid "$_dcent_ledger_b" &&
                [ "$_dcent_ledger_c" = - ]
            ;;
        node)
            dcent_sysupgrade_ledger_absolute_path_valid "$_dcent_ledger_a" &&
                dcent_sysupgrade_ledger_devino_valid "$_dcent_ledger_b" &&
                [ "$_dcent_ledger_c" = - ]
            ;;
        mount)
            dcent_sysupgrade_ledger_absolute_path_valid "$_dcent_ledger_a" &&
                dcent_sysupgrade_ledger_absolute_path_valid "$_dcent_ledger_b" || return 1
            case "$_dcent_ledger_c" in
                ro|rw) ;;
                *) return 1 ;;
            esac
            ;;
        workspace)
            dcent_sysupgrade_ledger_absolute_path_valid "$_dcent_ledger_a" &&
                dcent_sysupgrade_ledger_devino_valid "$_dcent_ledger_b" &&
                [ "$_dcent_ledger_c" = - ]
            ;;
        *) return 1 ;;
    esac
}

dcent_sysupgrade_ledger_secure_dir()
{
    [ "$#" -eq 1 ] || return 1
    [ -d "$1" ] && [ ! -L "$1" ] || return 1
    [ "$(dcent_sysupgrade_ledger_stat %u "$1")" = \
        "$(dcent_sysupgrade_ledger_expected_uid)" ] || return 1
    [ "$(dcent_sysupgrade_ledger_stat %a "$1")" = 700 ] || return 1
}

dcent_sysupgrade_ledger_secure_receipt()
{
    [ "$#" -eq 1 ] || return 1
    [ -r "$1" ] && [ -f "$1" ] && [ ! -L "$1" ] || return 1
    [ "$(dcent_sysupgrade_ledger_stat %u "$1")" = \
        "$(dcent_sysupgrade_ledger_expected_uid)" ] || return 1
    [ "$(dcent_sysupgrade_ledger_stat %a "$1")" = 600 ] || return 1
    [ "$(dcent_sysupgrade_ledger_stat %h "$1")" = 1 ] || return 1
    _dcent_ledger_bad_bytes=$(LC_ALL=C tr -d '\012\040-\176' <"$1" 2>/dev/null |
        wc -c | tr -d '[:space:]') || return 1
    [ "$_dcent_ledger_bad_bytes" = 0 ] || return 1
    _dcent_ledger_final_newline=$(tail -c 1 "$1" 2>/dev/null | wc -l |
        tr -d '[:space:]') || return 1
    [ "$_dcent_ledger_final_newline" = 1 ]
}

dcent_sysupgrade_ledger_parent_valid()
{
    [ "$#" -eq 1 ] || return 1
    _dcent_ledger_dir=$1
    dcent_sysupgrade_ledger_absolute_path_valid "$_dcent_ledger_dir" || return 1
    _dcent_ledger_parent=${_dcent_ledger_dir%/*}
    [ -n "$_dcent_ledger_parent" ] || _dcent_ledger_parent=/
    [ -d "$_dcent_ledger_parent" ] && [ ! -L "$_dcent_ledger_parent" ] || return 1
    _dcent_ledger_real_parent=$(CDPATH='' cd -P -- "$_dcent_ledger_parent" 2>/dev/null && pwd -P) || return 1
    [ "$_dcent_ledger_real_parent" = "$_dcent_ledger_parent" ] || return 1
    [ "$(dcent_sysupgrade_ledger_stat %u "$_dcent_ledger_parent")" = \
        "$(dcent_sysupgrade_ledger_expected_uid)" ]
}

dcent_sysupgrade_ledger_read_binding()
{
    [ "$#" -eq 1 ] || return 1
    _dcent_ledger_receipt=$1
    dcent_sysupgrade_ledger_secure_receipt "$_dcent_ledger_receipt" || return 1
    [ "$(wc -l <"$_dcent_ledger_receipt" 2>/dev/null)" = 10 ] || return 1

    _dcent_ledger_line=$(sed -n '1p' "$_dcent_ledger_receipt") || return 1
    [ "$_dcent_ledger_line" = 'schema=dcentos-sysupgrade-resource-ledger-v2' ] || return 1
    _dcent_ledger_line=$(sed -n '2p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in transaction_id=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_parsed_transaction_id=${_dcent_ledger_line#transaction_id=}
    _dcent_ledger_line=$(sed -n '3p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in boot_id=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_parsed_boot_id=${_dcent_ledger_line#boot_id=}
    _dcent_ledger_line=$(sed -n '4p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in owner_pid=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_parsed_owner_pid=${_dcent_ledger_line#owner_pid=}
    _dcent_ledger_line=$(sed -n '5p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in owner_starttime=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_parsed_owner_starttime=${_dcent_ledger_line#owner_starttime=}
    _dcent_ledger_line=$(sed -n '6p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in owner_mount_namespace=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_parsed_owner_mount_namespace=${_dcent_ledger_line#owner_mount_namespace=}
    _dcent_ledger_line=$(sed -n '7p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in transaction_lock_path=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_parsed_lock_path=${_dcent_ledger_line#transaction_lock_path=}
    _dcent_ledger_line=$(sed -n '8p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in transaction_lock_device_inode=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_parsed_lock_device_inode=${_dcent_ledger_line#transaction_lock_device_inode=}
    _dcent_ledger_line=$(sed -n '9p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in ledger_path=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_parsed_ledger_path=${_dcent_ledger_line#ledger_path=}
    _dcent_ledger_line=$(sed -n '10p' "$_dcent_ledger_receipt") || return 1
    [ "$_dcent_ledger_line" = 'owner=zynq-sysupgrade' ] || return 1

    dcent_sysupgrade_ledger_safe_id "$_dcent_ledger_parsed_transaction_id" &&
        dcent_sysupgrade_ledger_boot_id_valid "$_dcent_ledger_parsed_boot_id" &&
        dcent_sysupgrade_ledger_uint_valid "$_dcent_ledger_parsed_owner_pid" &&
        dcent_sysupgrade_ledger_uint_valid "$_dcent_ledger_parsed_owner_starttime" &&
        dcent_sysupgrade_ledger_devino_valid "$_dcent_ledger_parsed_owner_mount_namespace" &&
        dcent_sysupgrade_ledger_absolute_path_valid "$_dcent_ledger_parsed_lock_path" &&
        dcent_sysupgrade_ledger_devino_valid "$_dcent_ledger_parsed_lock_device_inode" &&
        dcent_sysupgrade_ledger_absolute_path_valid "$_dcent_ledger_parsed_ledger_path" || return 1
    [ "$_dcent_ledger_parsed_ledger_path" = "$_dcent_ledger_parsed_lock_path/ledger" ] || return 1
    [ "$_dcent_ledger_parsed_ledger_path/binding" = "$_dcent_ledger_receipt" ] || return 1
    dcent_sysupgrade_ledger_secure_dir "$_dcent_ledger_parsed_lock_path" || return 1
    [ "$(dcent_sysupgrade_ledger_stat '%d:%i' "$_dcent_ledger_parsed_lock_path")" = \
        "$_dcent_ledger_parsed_lock_device_inode" ] || return 1
    _dcent_ledger_parsed_binding_sha256=$(dcent_sysupgrade_ledger_digest_file \
        "$_dcent_ledger_receipt")
}

dcent_sysupgrade_ledger_read_resource_intent()
{
    [ "$#" -eq 1 ] || return 1
    _dcent_ledger_receipt=$1
    dcent_sysupgrade_ledger_secure_receipt "$_dcent_ledger_receipt" || return 1
    [ "$(wc -l <"$_dcent_ledger_receipt" 2>/dev/null)" = 9 ] || return 1
    _dcent_ledger_line=$(sed -n '1p' "$_dcent_ledger_receipt") || return 1
    [ "$_dcent_ledger_line" = 'schema=dcentos-sysupgrade-resource-intent-v2' ] || return 1
    _dcent_ledger_line=$(sed -n '2p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in binding_sha256=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_intent_binding_sha256=${_dcent_ledger_line#binding_sha256=}
    _dcent_ledger_line=$(sed -n '3p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in transaction_id=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_intent_transaction_id=${_dcent_ledger_line#transaction_id=}
    _dcent_ledger_line=$(sed -n '4p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in kind=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_intent_kind=${_dcent_ledger_line#kind=}
    _dcent_ledger_line=$(sed -n '5p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in resource_id=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_intent_id=${_dcent_ledger_line#resource_id=}
    _dcent_ledger_line=$(sed -n '6p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in provenance=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_intent_provenance=${_dcent_ledger_line#provenance=}
    _dcent_ledger_line=$(sed -n '7p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in identity_a=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_intent_a=${_dcent_ledger_line#identity_a=}
    _dcent_ledger_line=$(sed -n '8p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in identity_b=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_intent_b=${_dcent_ledger_line#identity_b=}
    _dcent_ledger_line=$(sed -n '9p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in identity_c=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_intent_c=${_dcent_ledger_line#identity_c=}

    dcent_sysupgrade_ledger_sha256_valid "$_dcent_ledger_intent_binding_sha256" &&
        dcent_sysupgrade_ledger_safe_id "$_dcent_ledger_intent_transaction_id" &&
        dcent_sysupgrade_ledger_safe_id "$_dcent_ledger_intent_id" || return 1
    case "$_dcent_ledger_intent_provenance" in created|borrowed) ;;
        *) return 1 ;;
    esac
    dcent_sysupgrade_ledger_identity_valid "$_dcent_ledger_intent_kind" \
        "$_dcent_ledger_intent_a" "$_dcent_ledger_intent_b" \
        "$_dcent_ledger_intent_c" || return 1
    _dcent_ledger_intent_sha256=$(dcent_sysupgrade_ledger_digest_file \
        "$_dcent_ledger_receipt")
}

dcent_sysupgrade_ledger_read_resource_status()
{
    [ "$#" -eq 1 ] || return 1
    _dcent_ledger_receipt=$1
    dcent_sysupgrade_ledger_secure_receipt "$_dcent_ledger_receipt" || return 1
    [ "$(wc -l <"$_dcent_ledger_receipt" 2>/dev/null)" = 12 ] || return 1
    _dcent_ledger_line=$(sed -n '1p' "$_dcent_ledger_receipt") || return 1
    [ "$_dcent_ledger_line" = 'schema=dcentos-sysupgrade-resource-status-v2' ] || return 1
    _dcent_ledger_line=$(sed -n '2p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in binding_sha256=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_status_binding_sha256=${_dcent_ledger_line#binding_sha256=}
    _dcent_ledger_line=$(sed -n '3p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in transaction_id=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_status_transaction_id=${_dcent_ledger_line#transaction_id=}
    _dcent_ledger_line=$(sed -n '4p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in kind=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_status_kind=${_dcent_ledger_line#kind=}
    _dcent_ledger_line=$(sed -n '5p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in resource_id=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_status_id=${_dcent_ledger_line#resource_id=}
    _dcent_ledger_line=$(sed -n '6p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in intent_sha256=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_status_intent_sha256=${_dcent_ledger_line#intent_sha256=}
    _dcent_ledger_line=$(sed -n '7p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in phase=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_status_phase=${_dcent_ledger_line#phase=}
    _dcent_ledger_line=$(sed -n '8p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in revision=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_status_revision=${_dcent_ledger_line#revision=}
    _dcent_ledger_line=$(sed -n '9p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in evidence_sha256=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_status_evidence_sha256=${_dcent_ledger_line#evidence_sha256=}
    _dcent_ledger_line=$(sed -n '10p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in previous_status_sha256=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_status_previous_sha256=${_dcent_ledger_line#previous_status_sha256=}
    _dcent_ledger_line=$(sed -n '11p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in actor_kind=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_status_actor_kind=${_dcent_ledger_line#actor_kind=}
    _dcent_ledger_line=$(sed -n '12p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in actor_id=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_status_actor_id=${_dcent_ledger_line#actor_id=}

    dcent_sysupgrade_ledger_sha256_valid "$_dcent_ledger_status_binding_sha256" &&
        dcent_sysupgrade_ledger_safe_id "$_dcent_ledger_status_transaction_id" &&
        dcent_sysupgrade_ledger_safe_id "$_dcent_ledger_status_id" &&
        dcent_sysupgrade_ledger_sha256_valid "$_dcent_ledger_status_intent_sha256" &&
        dcent_sysupgrade_ledger_uint_valid "$_dcent_ledger_status_revision" &&
        dcent_sysupgrade_ledger_sha256_valid "$_dcent_ledger_status_evidence_sha256" &&
        dcent_sysupgrade_ledger_sha256_or_dash_valid \
            "$_dcent_ledger_status_previous_sha256" &&
        dcent_sysupgrade_ledger_safe_id "$_dcent_ledger_status_actor_id" || return 1
    case "$_dcent_ledger_status_phase" in
        pending|active|release-pending|released|conflict) ;;
        *) return 1 ;;
    esac
    case "$_dcent_ledger_status_actor_kind" in owner|reconciler) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_status_sha256=$(dcent_sysupgrade_ledger_digest_file \
        "$_dcent_ledger_receipt")
}

dcent_sysupgrade_ledger_resource_transition_valid()
{
    [ "$#" -eq 2 ] || return 1
    case "$1:$2" in
        pending:active|pending:released|pending:conflict|\
        active:release-pending|active:conflict|\
        release-pending:released|release-pending:conflict) ;;
        *) return 1 ;;
    esac
}

dcent_sysupgrade_ledger_read_resource_dir()
{
    [ "$#" -eq 4 ] || return 1
    _dcent_ledger_resource_dir=$1
    _dcent_ledger_expected_kind=$2
    _dcent_ledger_expected_id=$3
    _dcent_ledger_layout_kind=$4
    dcent_sysupgrade_ledger_secure_dir "$_dcent_ledger_resource_dir" || return 1
    dcent_sysupgrade_ledger_read_resource_intent \
        "$_dcent_ledger_resource_dir/intent" || return 1
    [ "$_dcent_ledger_intent_binding_sha256" = \
        "$_dcent_ledger_parsed_binding_sha256" ] &&
        [ "$_dcent_ledger_intent_transaction_id" = \
            "$_dcent_ledger_parsed_transaction_id" ] &&
        [ "$_dcent_ledger_intent_kind" = "$_dcent_ledger_expected_kind" ] &&
        [ "$_dcent_ledger_intent_id" = "$_dcent_ledger_expected_id" ] || return 1

    _dcent_ledger_entry_count=0
    for _dcent_ledger_entry in "$_dcent_ledger_resource_dir"/* \
        "$_dcent_ledger_resource_dir"/.[!.]* \
        "$_dcent_ledger_resource_dir"/..?*; do
        [ -e "$_dcent_ledger_entry" ] || [ -L "$_dcent_ledger_entry" ] || continue
        _dcent_ledger_entry_count=$((_dcent_ledger_entry_count + 1))
        case "${_dcent_ledger_entry##*/}" in
            intent|status.1|status.2|status.3|status.4) ;;
            *) return 1 ;;
        esac
    done

    _dcent_ledger_revision=1
    _dcent_ledger_previous_phase=
    _dcent_ledger_previous_sha=-
    _dcent_ledger_status_count=0
    _dcent_ledger_gap=0
    while [ "$_dcent_ledger_revision" -le 4 ]; do
        _dcent_ledger_status_path=$_dcent_ledger_resource_dir/status.$_dcent_ledger_revision
        if [ ! -e "$_dcent_ledger_status_path" ] && [ ! -L "$_dcent_ledger_status_path" ]; then
            _dcent_ledger_gap=1
            _dcent_ledger_revision=$((_dcent_ledger_revision + 1))
            continue
        fi
        [ "$_dcent_ledger_gap" = 0 ] || return 1
        dcent_sysupgrade_ledger_read_resource_status \
            "$_dcent_ledger_status_path" || return 1
        [ "$_dcent_ledger_status_binding_sha256" = \
            "$_dcent_ledger_parsed_binding_sha256" ] &&
            [ "$_dcent_ledger_status_transaction_id" = \
                "$_dcent_ledger_parsed_transaction_id" ] &&
            [ "$_dcent_ledger_status_kind" = "$_dcent_ledger_expected_kind" ] &&
            [ "$_dcent_ledger_status_id" = "$_dcent_ledger_expected_id" ] &&
            [ "$_dcent_ledger_status_intent_sha256" = \
                "$_dcent_ledger_intent_sha256" ] &&
            [ "$_dcent_ledger_status_revision" = "$_dcent_ledger_revision" ] &&
            [ "$_dcent_ledger_status_previous_sha256" = \
                "$_dcent_ledger_previous_sha" ] || return 1
        if [ "$_dcent_ledger_revision" = 1 ]; then
            [ "$_dcent_ledger_status_phase" = pending ] &&
                [ "$_dcent_ledger_status_actor_kind" = owner ] &&
                [ "$_dcent_ledger_status_actor_id" = \
                    "$_dcent_ledger_parsed_transaction_id" ] &&
                [ "$_dcent_ledger_status_evidence_sha256" = \
                    "$_dcent_ledger_intent_sha256" ] || return 1
        else
            dcent_sysupgrade_ledger_resource_transition_valid \
                "$_dcent_ledger_previous_phase" \
                "$_dcent_ledger_status_phase" || return 1
            case "$_dcent_ledger_status_actor_kind" in
                owner)
                    [ "$_dcent_ledger_status_actor_id" = \
                        "$_dcent_ledger_parsed_transaction_id" ] || return 1
                    ;;
                reconciler)
                    [ "$_dcent_ledger_layout_kind" = claimed ] &&
                        [ "$_dcent_ledger_status_actor_id" = \
                            "$_dcent_ledger_claim_id" ] || return 1
                    ;;
                *) return 1 ;;
            esac
        fi
        _dcent_ledger_status_count=$((_dcent_ledger_status_count + 1))
        _dcent_ledger_previous_phase=$_dcent_ledger_status_phase
        _dcent_ledger_previous_sha=$_dcent_ledger_status_sha256
        _dcent_ledger_revision=$((_dcent_ledger_revision + 1))
    done
    [ "$_dcent_ledger_status_count" -ge 1 ] || return 1
    [ "$_dcent_ledger_entry_count" -eq $((_dcent_ledger_status_count + 1)) ] || return 1
    _dcent_ledger_resource_latest_phase=$_dcent_ledger_previous_phase
    _dcent_ledger_resource_latest_revision=$_dcent_ledger_status_count
    _dcent_ledger_resource_latest_status_sha256=$_dcent_ledger_previous_sha
    _dcent_ledger_resource_latest_evidence_sha256=$_dcent_ledger_status_evidence_sha256
}

dcent_sysupgrade_ledger_read_claim_intent()
{
    [ "$#" -eq 1 ] || return 1
    _dcent_ledger_receipt=$1
    dcent_sysupgrade_ledger_secure_receipt "$_dcent_ledger_receipt" || return 1
    [ "$(wc -l <"$_dcent_ledger_receipt" 2>/dev/null)" = 12 ] || return 1
    _dcent_ledger_line=$(sed -n '1p' "$_dcent_ledger_receipt") || return 1
    [ "$_dcent_ledger_line" = 'schema=dcentos-sysupgrade-reconcile-intent-v2' ] || return 1
    _dcent_ledger_line=$(sed -n '2p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in binding_sha256=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_claim_binding_sha256=${_dcent_ledger_line#binding_sha256=}
    _dcent_ledger_line=$(sed -n '3p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in transaction_id=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_claim_transaction_id=${_dcent_ledger_line#transaction_id=}
    _dcent_ledger_line=$(sed -n '4p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in claim_id=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_claim_id=${_dcent_ledger_line#claim_id=}
    _dcent_ledger_line=$(sed -n '5p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in reconciler_boot_id=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_claim_boot_id=${_dcent_ledger_line#reconciler_boot_id=}
    _dcent_ledger_line=$(sed -n '6p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in reconciler_pid=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_claim_pid=${_dcent_ledger_line#reconciler_pid=}
    _dcent_ledger_line=$(sed -n '7p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in reconciler_starttime=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_claim_starttime=${_dcent_ledger_line#reconciler_starttime=}
    _dcent_ledger_line=$(sed -n '8p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in reconciler_mount_namespace=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_claim_mount_namespace=${_dcent_ledger_line#reconciler_mount_namespace=}
    _dcent_ledger_line=$(sed -n '9p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in owner_death_evidence_sha256=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_claim_owner_death_sha256=${_dcent_ledger_line#owner_death_evidence_sha256=}
    _dcent_ledger_line=$(sed -n '10p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in maintenance_lock_path=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_claim_maintenance_lock=${_dcent_ledger_line#maintenance_lock_path=}
    _dcent_ledger_line=$(sed -n '11p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in maintenance_lock_device_inode=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_claim_maintenance_lock_devino=${_dcent_ledger_line#maintenance_lock_device_inode=}
    _dcent_ledger_line=$(sed -n '12p' "$_dcent_ledger_receipt") || return 1
    [ "$_dcent_ledger_line" = 'owner=zynq-sysupgrade-reconciler' ] || return 1

    dcent_sysupgrade_ledger_sha256_valid "$_dcent_ledger_claim_binding_sha256" &&
        dcent_sysupgrade_ledger_safe_id "$_dcent_ledger_claim_transaction_id" &&
        dcent_sysupgrade_ledger_safe_id "$_dcent_ledger_claim_id" &&
        dcent_sysupgrade_ledger_boot_id_valid "$_dcent_ledger_claim_boot_id" &&
        dcent_sysupgrade_ledger_uint_valid "$_dcent_ledger_claim_pid" &&
        dcent_sysupgrade_ledger_uint_valid "$_dcent_ledger_claim_starttime" &&
        dcent_sysupgrade_ledger_devino_valid \
            "$_dcent_ledger_claim_mount_namespace" &&
        dcent_sysupgrade_ledger_sha256_valid \
            "$_dcent_ledger_claim_owner_death_sha256" &&
        dcent_sysupgrade_ledger_absolute_path_valid \
            "$_dcent_ledger_claim_maintenance_lock" &&
        dcent_sysupgrade_ledger_devino_valid \
            "$_dcent_ledger_claim_maintenance_lock_devino" || return 1
    dcent_sysupgrade_ledger_secure_dir \
        "$_dcent_ledger_claim_maintenance_lock" || return 1
    [ "$(dcent_sysupgrade_ledger_stat '%d:%i' \
        "$_dcent_ledger_claim_maintenance_lock")" = \
        "$_dcent_ledger_claim_maintenance_lock_devino" ] || return 1
    _dcent_ledger_claim_intent_sha256=$(dcent_sysupgrade_ledger_digest_file \
        "$_dcent_ledger_receipt")
}

dcent_sysupgrade_ledger_read_claim_status()
{
    [ "$#" -eq 1 ] || return 1
    _dcent_ledger_receipt=$1
    dcent_sysupgrade_ledger_secure_receipt "$_dcent_ledger_receipt" || return 1
    [ "$(wc -l <"$_dcent_ledger_receipt" 2>/dev/null)" = 8 ] || return 1
    _dcent_ledger_line=$(sed -n '1p' "$_dcent_ledger_receipt") || return 1
    [ "$_dcent_ledger_line" = 'schema=dcentos-sysupgrade-reconcile-status-v2' ] || return 1
    _dcent_ledger_line=$(sed -n '2p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in claim_intent_sha256=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_claim_status_intent_sha256=${_dcent_ledger_line#claim_intent_sha256=}
    _dcent_ledger_line=$(sed -n '3p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in phase=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_claim_status_phase=${_dcent_ledger_line#phase=}
    _dcent_ledger_line=$(sed -n '4p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in revision=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_claim_status_revision=${_dcent_ledger_line#revision=}
    _dcent_ledger_line=$(sed -n '5p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in quiescence_sha256=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_claim_status_quiescence=${_dcent_ledger_line#quiescence_sha256=}
    _dcent_ledger_line=$(sed -n '6p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in outcome_sha256=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_claim_status_outcome=${_dcent_ledger_line#outcome_sha256=}
    _dcent_ledger_line=$(sed -n '7p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in previous_status_sha256=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_claim_status_previous_sha256=${_dcent_ledger_line#previous_status_sha256=}
    _dcent_ledger_line=$(sed -n '8p' "$_dcent_ledger_receipt") || return 1
    case "$_dcent_ledger_line" in actor_id=*) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_claim_status_actor_id=${_dcent_ledger_line#actor_id=}

    dcent_sysupgrade_ledger_sha256_valid \
        "$_dcent_ledger_claim_status_intent_sha256" &&
        dcent_sysupgrade_ledger_uint_valid \
            "$_dcent_ledger_claim_status_revision" &&
        dcent_sysupgrade_ledger_sha256_or_dash_valid \
            "$_dcent_ledger_claim_status_quiescence" &&
        dcent_sysupgrade_ledger_sha256_or_dash_valid \
            "$_dcent_ledger_claim_status_outcome" &&
        dcent_sysupgrade_ledger_sha256_or_dash_valid \
            "$_dcent_ledger_claim_status_previous_sha256" &&
        dcent_sysupgrade_ledger_safe_id \
            "$_dcent_ledger_claim_status_actor_id" || return 1
    case "$_dcent_ledger_claim_status_phase" in
        claimed|quiescent|reconciling|complete|blocked) ;;
        *) return 1 ;;
    esac
    _dcent_ledger_claim_status_sha256=$(dcent_sysupgrade_ledger_digest_file \
        "$_dcent_ledger_receipt")
}

dcent_sysupgrade_ledger_claim_transition_valid()
{
    [ "$#" -eq 2 ] || return 1
    case "$1:$2" in
        claimed:quiescent|claimed:blocked|quiescent:reconciling|\
        quiescent:blocked|reconciling:complete|reconciling:blocked) ;;
        *) return 1 ;;
    esac
}

dcent_sysupgrade_ledger_read_claim_dir()
{
    [ "$#" -eq 1 ] || return 1
    _dcent_ledger_claim_dir=$1
    dcent_sysupgrade_ledger_secure_dir "$_dcent_ledger_claim_dir" || return 1
    dcent_sysupgrade_ledger_read_claim_intent \
        "$_dcent_ledger_claim_dir/intent" || return 1
    [ "$_dcent_ledger_claim_binding_sha256" = \
        "$_dcent_ledger_parsed_binding_sha256" ] &&
        [ "$_dcent_ledger_claim_transaction_id" = \
            "$_dcent_ledger_parsed_transaction_id" ] || return 1

    _dcent_ledger_entry_count=0
    for _dcent_ledger_entry in "$_dcent_ledger_claim_dir"/* \
        "$_dcent_ledger_claim_dir"/.[!.]* "$_dcent_ledger_claim_dir"/..?*; do
        [ -e "$_dcent_ledger_entry" ] || [ -L "$_dcent_ledger_entry" ] || continue
        _dcent_ledger_entry_count=$((_dcent_ledger_entry_count + 1))
        case "${_dcent_ledger_entry##*/}" in
            intent|status.1|status.2|status.3|status.4) ;;
            *) return 1 ;;
        esac
    done

    _dcent_ledger_revision=1
    _dcent_ledger_previous_phase=
    _dcent_ledger_previous_sha=-
    _dcent_ledger_previous_quiescence=-
    _dcent_ledger_status_count=0
    _dcent_ledger_gap=0
    while [ "$_dcent_ledger_revision" -le 4 ]; do
        _dcent_ledger_status_path=$_dcent_ledger_claim_dir/status.$_dcent_ledger_revision
        if [ ! -e "$_dcent_ledger_status_path" ] && [ ! -L "$_dcent_ledger_status_path" ]; then
            _dcent_ledger_gap=1
            _dcent_ledger_revision=$((_dcent_ledger_revision + 1))
            continue
        fi
        [ "$_dcent_ledger_gap" = 0 ] || return 1
        dcent_sysupgrade_ledger_read_claim_status \
            "$_dcent_ledger_status_path" || return 1
        [ "$_dcent_ledger_claim_status_intent_sha256" = \
            "$_dcent_ledger_claim_intent_sha256" ] &&
            [ "$_dcent_ledger_claim_status_revision" = \
                "$_dcent_ledger_revision" ] &&
            [ "$_dcent_ledger_claim_status_previous_sha256" = \
                "$_dcent_ledger_previous_sha" ] &&
            [ "$_dcent_ledger_claim_status_actor_id" = \
                "$_dcent_ledger_claim_id" ] || return 1
        if [ "$_dcent_ledger_revision" = 1 ]; then
            [ "$_dcent_ledger_claim_status_phase" = claimed ] &&
                [ "$_dcent_ledger_claim_status_quiescence" = - ] &&
                [ "$_dcent_ledger_claim_status_outcome" = - ] || return 1
        else
            dcent_sysupgrade_ledger_claim_transition_valid \
                "$_dcent_ledger_previous_phase" \
                "$_dcent_ledger_claim_status_phase" || return 1
            case "$_dcent_ledger_claim_status_phase" in
                quiescent)
                    dcent_sysupgrade_ledger_sha256_valid \
                        "$_dcent_ledger_claim_status_quiescence" &&
                        [ "$_dcent_ledger_claim_status_outcome" = - ] || return 1
                    ;;
                reconciling)
                    [ "$_dcent_ledger_claim_status_quiescence" = \
                        "$_dcent_ledger_previous_quiescence" ] &&
                        [ "$_dcent_ledger_claim_status_outcome" = - ] || return 1
                    ;;
                complete)
                    [ "$_dcent_ledger_claim_status_quiescence" = \
                        "$_dcent_ledger_previous_quiescence" ] &&
                        dcent_sysupgrade_ledger_sha256_valid \
                            "$_dcent_ledger_claim_status_outcome" || return 1
                    ;;
                blocked)
                    [ "$_dcent_ledger_claim_status_quiescence" = \
                        "$_dcent_ledger_previous_quiescence" ] &&
                        dcent_sysupgrade_ledger_sha256_valid \
                            "$_dcent_ledger_claim_status_outcome" || return 1
                    ;;
                *) return 1 ;;
            esac
        fi
        _dcent_ledger_status_count=$((_dcent_ledger_status_count + 1))
        _dcent_ledger_previous_phase=$_dcent_ledger_claim_status_phase
        _dcent_ledger_previous_sha=$_dcent_ledger_claim_status_sha256
        _dcent_ledger_previous_quiescence=$_dcent_ledger_claim_status_quiescence
        _dcent_ledger_revision=$((_dcent_ledger_revision + 1))
    done
    [ "$_dcent_ledger_status_count" -ge 1 ] || return 1
    [ "$_dcent_ledger_entry_count" -eq $((_dcent_ledger_status_count + 1)) ] || return 1
    _dcent_ledger_claim_latest_phase=$_dcent_ledger_previous_phase
    _dcent_ledger_claim_latest_revision=$_dcent_ledger_status_count
    _dcent_ledger_claim_latest_status_sha256=$_dcent_ledger_previous_sha
    _dcent_ledger_claim_latest_quiescence=$_dcent_ledger_previous_quiescence
}

dcent_sysupgrade_ledger_resources_clean()
{
    [ "$#" -eq 2 ] || return 1
    _dcent_ledger_resources=$1
    _dcent_ledger_layout_kind=$2
    dcent_sysupgrade_ledger_secure_dir "$_dcent_ledger_resources" || return 1
    for _dcent_ledger_entry in "$_dcent_ledger_resources"/* \
        "$_dcent_ledger_resources"/.[!.]* "$_dcent_ledger_resources"/..?*; do
        [ -e "$_dcent_ledger_entry" ] || [ -L "$_dcent_ledger_entry" ] || continue
        dcent_sysupgrade_ledger_secure_dir "$_dcent_ledger_entry" || return 1
        _dcent_ledger_base=${_dcent_ledger_entry##*/}
        case "$_dcent_ledger_base" in
            attachment--*|node--*|mount--*|workspace--*) ;;
            *) return 1 ;;
        esac
        _dcent_ledger_name_kind=${_dcent_ledger_base%%--*}
        _dcent_ledger_name_id=${_dcent_ledger_base#*--}
        dcent_sysupgrade_ledger_safe_id "$_dcent_ledger_name_id" || return 1
        dcent_sysupgrade_ledger_read_resource_dir "$_dcent_ledger_entry" \
            "$_dcent_ledger_name_kind" "$_dcent_ledger_name_id" \
            "$_dcent_ledger_layout_kind" || return 1
    done
}

dcent_sysupgrade_ledger_layout()
{
    [ "$#" -eq 3 ] || return 1
    _dcent_ledger_dir=$1
    _dcent_ledger_layout_kind=$2
    _dcent_ledger_reserved=$3
    dcent_sysupgrade_ledger_secure_dir "$_dcent_ledger_dir" || return 1
    _dcent_ledger_entry_count=0
    for _dcent_ledger_entry in "$_dcent_ledger_dir"/* \
        "$_dcent_ledger_dir"/.[!.]* "$_dcent_ledger_dir"/..?*; do
        [ -e "$_dcent_ledger_entry" ] || [ -L "$_dcent_ledger_entry" ] || continue
        _dcent_ledger_entry_count=$((_dcent_ledger_entry_count + 1))
        case "$_dcent_ledger_entry" in
            "$_dcent_ledger_dir/binding"|"$_dcent_ledger_dir/resources") ;;
            "$_dcent_ledger_dir/reconcile.claim")
                [ "$_dcent_ledger_layout_kind" = claimed ] || return 1
                ;;
            "$_dcent_ledger_dir/.operation")
                [ "$_dcent_ledger_reserved" = reserved ] || return 1
                ;;
            *) return 1 ;;
        esac
    done
    _dcent_ledger_expected_entries=2
    [ "$_dcent_ledger_layout_kind" = owner ] ||
        _dcent_ledger_expected_entries=$((_dcent_ledger_expected_entries + 1))
    [ "$_dcent_ledger_reserved" = clean ] ||
        _dcent_ledger_expected_entries=$((_dcent_ledger_expected_entries + 1))
    [ "$_dcent_ledger_entry_count" -eq "$_dcent_ledger_expected_entries" ] || return 1
    dcent_sysupgrade_ledger_read_binding "$_dcent_ledger_dir/binding" || return 1
    if [ "$_dcent_ledger_layout_kind" = claimed ]; then
        dcent_sysupgrade_ledger_read_claim_dir \
            "$_dcent_ledger_dir/reconcile.claim" || return 1
    fi
    dcent_sysupgrade_ledger_resources_clean "$_dcent_ledger_dir/resources" \
        "$_dcent_ledger_layout_kind" || return 1
    if [ "$_dcent_ledger_reserved" = reserved ]; then
        dcent_sysupgrade_ledger_secure_dir "$_dcent_ledger_dir/.operation" || return 1
    fi
}

dcent_sysupgrade_ledger_binding_matches_globals()
{
    [ "$_dcent_ledger_parsed_transaction_id" = \
        "$DCENT_SYSUPGRADE_LEDGER_TRANSACTION_ID" ] &&
        [ "$_dcent_ledger_parsed_boot_id" = \
            "$DCENT_SYSUPGRADE_LEDGER_BOOT_ID" ] &&
        [ "$_dcent_ledger_parsed_owner_pid" = \
            "$DCENT_SYSUPGRADE_LEDGER_OWNER_PID" ] &&
        [ "$_dcent_ledger_parsed_owner_starttime" = \
            "$DCENT_SYSUPGRADE_LEDGER_OWNER_STARTTIME" ] &&
        [ "$_dcent_ledger_parsed_owner_mount_namespace" = \
            "$DCENT_SYSUPGRADE_LEDGER_OWNER_MOUNT_NAMESPACE" ] &&
        [ "$_dcent_ledger_parsed_lock_path" = \
            "$DCENT_SYSUPGRADE_LEDGER_LOCK_PATH" ] &&
        [ "$_dcent_ledger_parsed_lock_device_inode" = \
            "$DCENT_SYSUPGRADE_LEDGER_LOCK_DEVICE_INODE" ] &&
        [ "$_dcent_ledger_parsed_binding_sha256" = \
            "$DCENT_SYSUPGRADE_LEDGER_BINDING_SHA256" ]
}

dcent_sysupgrade_ledger_bind_owner()
{
    DCENT_SYSUPGRADE_LEDGER_BOUND=1
    DCENT_SYSUPGRADE_LEDGER_ACTOR=owner
    DCENT_SYSUPGRADE_LEDGER_DIR=$1
    DCENT_SYSUPGRADE_LEDGER_TRANSACTION_ID=$2
    DCENT_SYSUPGRADE_LEDGER_BOOT_ID=$3
    DCENT_SYSUPGRADE_LEDGER_OWNER_PID=$4
    DCENT_SYSUPGRADE_LEDGER_OWNER_STARTTIME=$5
    DCENT_SYSUPGRADE_LEDGER_OWNER_MOUNT_NAMESPACE=$6
    DCENT_SYSUPGRADE_LEDGER_LOCK_PATH=$7
    DCENT_SYSUPGRADE_LEDGER_LOCK_DEVICE_INODE=$8
    DCENT_SYSUPGRADE_LEDGER_BINDING_SHA256=$_dcent_ledger_parsed_binding_sha256
}

dcent_sysupgrade_ledger_bind_reconciler()
{
    DCENT_SYSUPGRADE_LEDGER_BOUND=1
    DCENT_SYSUPGRADE_LEDGER_ACTOR=reconciler
    DCENT_SYSUPGRADE_LEDGER_DIR=$1
    DCENT_SYSUPGRADE_LEDGER_TRANSACTION_ID=$_dcent_ledger_parsed_transaction_id
    DCENT_SYSUPGRADE_LEDGER_BOOT_ID=$_dcent_ledger_parsed_boot_id
    DCENT_SYSUPGRADE_LEDGER_OWNER_PID=$_dcent_ledger_parsed_owner_pid
    DCENT_SYSUPGRADE_LEDGER_OWNER_STARTTIME=$_dcent_ledger_parsed_owner_starttime
    DCENT_SYSUPGRADE_LEDGER_OWNER_MOUNT_NAMESPACE=$_dcent_ledger_parsed_owner_mount_namespace
    DCENT_SYSUPGRADE_LEDGER_LOCK_PATH=$_dcent_ledger_parsed_lock_path
    DCENT_SYSUPGRADE_LEDGER_LOCK_DEVICE_INODE=$_dcent_ledger_parsed_lock_device_inode
    DCENT_SYSUPGRADE_LEDGER_BINDING_SHA256=$_dcent_ledger_parsed_binding_sha256
    DCENT_SYSUPGRADE_LEDGER_CLAIM_ID=$2
    DCENT_SYSUPGRADE_LEDGER_RECONCILER_BOOT_ID=$3
    DCENT_SYSUPGRADE_LEDGER_RECONCILER_PID=$4
    DCENT_SYSUPGRADE_LEDGER_RECONCILER_STARTTIME=$5
    DCENT_SYSUPGRADE_LEDGER_RECONCILER_MOUNT_NAMESPACE=$6
}

dcent_sysupgrade_ledger_create()
{
    [ "$#" -eq 8 ] || {
        dcent_sysupgrade_ledger_fail \
            "create requires DIR TX BOOT PID START MNT_NS LOCK LOCK_DEVINO"
        return 1
    }
    [ "$DCENT_SYSUPGRADE_LEDGER_BOUND" = 0 ] || return 1
    _dcent_ledger_dir=$1
    _dcent_ledger_tx=$2
    _dcent_ledger_boot=$3
    _dcent_ledger_pid=$4
    _dcent_ledger_start=$5
    _dcent_ledger_mntns=$6
    _dcent_ledger_lock=$7
    _dcent_ledger_lock_devino=$8
    dcent_sysupgrade_ledger_parent_valid "$_dcent_ledger_dir" &&
        dcent_sysupgrade_ledger_safe_id "$_dcent_ledger_tx" &&
        dcent_sysupgrade_ledger_boot_id_valid "$_dcent_ledger_boot" &&
        dcent_sysupgrade_ledger_uint_valid "$_dcent_ledger_pid" &&
        dcent_sysupgrade_ledger_uint_valid "$_dcent_ledger_start" &&
        dcent_sysupgrade_ledger_devino_valid "$_dcent_ledger_mntns" &&
        dcent_sysupgrade_ledger_absolute_path_valid "$_dcent_ledger_lock" &&
        dcent_sysupgrade_ledger_devino_valid "$_dcent_ledger_lock_devino" || return 1
    [ "$_dcent_ledger_dir" = "$_dcent_ledger_lock/ledger" ] || return 1
    dcent_sysupgrade_ledger_secure_dir "$_dcent_ledger_lock" || return 1
    [ "$(dcent_sysupgrade_ledger_stat '%d:%i' "$_dcent_ledger_lock")" = \
        "$_dcent_ledger_lock_devino" ] || return 1
    [ ! -e "$_dcent_ledger_dir" ] && [ ! -L "$_dcent_ledger_dir" ] || return 1
    (umask 077; mkdir "$_dcent_ledger_dir") || return 1
    chmod 700 "$_dcent_ledger_dir" || return 1
    (umask 077; mkdir "$_dcent_ledger_dir/resources") || return 1
    chmod 700 "$_dcent_ledger_dir/resources" || return 1
    _dcent_ledger_new=$_dcent_ledger_dir/.binding.new.$$
    if ! (umask 077; set -C; printf '%s\n' \
        'schema=dcentos-sysupgrade-resource-ledger-v2' \
        "transaction_id=$_dcent_ledger_tx" \
        "boot_id=$_dcent_ledger_boot" \
        "owner_pid=$_dcent_ledger_pid" \
        "owner_starttime=$_dcent_ledger_start" \
        "owner_mount_namespace=$_dcent_ledger_mntns" \
        "transaction_lock_path=$_dcent_ledger_lock" \
        "transaction_lock_device_inode=$_dcent_ledger_lock_devino" \
        "ledger_path=$_dcent_ledger_dir" \
        'owner=zynq-sysupgrade' >"$_dcent_ledger_new"); then
        return 1
    fi
    chmod 600 "$_dcent_ledger_new" || return 1
    mv "$_dcent_ledger_new" "$_dcent_ledger_dir/binding" || return 1
    dcent_sysupgrade_ledger_layout "$_dcent_ledger_dir" owner clean || return 1
    [ "$_dcent_ledger_parsed_transaction_id" = "$_dcent_ledger_tx" ] &&
        [ "$_dcent_ledger_parsed_boot_id" = "$_dcent_ledger_boot" ] &&
        [ "$_dcent_ledger_parsed_owner_pid" = "$_dcent_ledger_pid" ] &&
        [ "$_dcent_ledger_parsed_owner_starttime" = "$_dcent_ledger_start" ] &&
        [ "$_dcent_ledger_parsed_owner_mount_namespace" = "$_dcent_ledger_mntns" ] &&
        [ "$_dcent_ledger_parsed_lock_path" = "$_dcent_ledger_lock" ] &&
        [ "$_dcent_ledger_parsed_lock_device_inode" = \
            "$_dcent_ledger_lock_devino" ] || return 1
    dcent_sysupgrade_ledger_bind_owner "$_dcent_ledger_dir" "$_dcent_ledger_tx" \
        "$_dcent_ledger_boot" "$_dcent_ledger_pid" "$_dcent_ledger_start" \
        "$_dcent_ledger_mntns" "$_dcent_ledger_lock" "$_dcent_ledger_lock_devino"
}

dcent_sysupgrade_ledger_open_owned()
{
    [ "$#" -eq 8 ] || return 1
    [ "$DCENT_SYSUPGRADE_LEDGER_BOUND" = 0 ] || return 1
    dcent_sysupgrade_ledger_layout "$1" owner clean || return 1
    [ "$_dcent_ledger_parsed_transaction_id" = "$2" ] &&
        [ "$_dcent_ledger_parsed_boot_id" = "$3" ] &&
        [ "$_dcent_ledger_parsed_owner_pid" = "$4" ] &&
        [ "$_dcent_ledger_parsed_owner_starttime" = "$5" ] &&
        [ "$_dcent_ledger_parsed_owner_mount_namespace" = "$6" ] &&
        [ "$_dcent_ledger_parsed_lock_path" = "$7" ] &&
        [ "$_dcent_ledger_parsed_lock_device_inode" = "$8" ] || return 1
    dcent_sysupgrade_ledger_bind_owner "$1" "$2" "$3" "$4" "$5" "$6" "$7" "$8"
}

dcent_sysupgrade_ledger_verify_owned()
{
    [ "$DCENT_SYSUPGRADE_LEDGER_BOUND" = 1 ] &&
        [ "$DCENT_SYSUPGRADE_LEDGER_ACTOR" = owner ] || return 1
    dcent_sysupgrade_ledger_layout "$DCENT_SYSUPGRADE_LEDGER_DIR" owner clean &&
        dcent_sysupgrade_ledger_binding_matches_globals
}

dcent_sysupgrade_ledger_verify_reconciler()
{
    [ "$DCENT_SYSUPGRADE_LEDGER_BOUND" = 1 ] &&
        [ "$DCENT_SYSUPGRADE_LEDGER_ACTOR" = reconciler ] || return 1
    dcent_sysupgrade_ledger_layout "$DCENT_SYSUPGRADE_LEDGER_DIR" claimed clean &&
        dcent_sysupgrade_ledger_binding_matches_globals &&
        [ "$_dcent_ledger_claim_id" = "$DCENT_SYSUPGRADE_LEDGER_CLAIM_ID" ] &&
        [ "$_dcent_ledger_claim_boot_id" = \
            "$DCENT_SYSUPGRADE_LEDGER_RECONCILER_BOOT_ID" ] &&
        [ "$_dcent_ledger_claim_pid" = \
            "$DCENT_SYSUPGRADE_LEDGER_RECONCILER_PID" ] &&
        [ "$_dcent_ledger_claim_starttime" = \
            "$DCENT_SYSUPGRADE_LEDGER_RECONCILER_STARTTIME" ] &&
        [ "$_dcent_ledger_claim_mount_namespace" = \
            "$DCENT_SYSUPGRADE_LEDGER_RECONCILER_MOUNT_NAMESPACE" ]
}

dcent_sysupgrade_ledger_operation_reserve()
{
    [ "$#" -eq 2 ] || return 1
    _dcent_ledger_dir=$1
    _dcent_ledger_layout_kind=$2
    _dcent_ledger_operation=$_dcent_ledger_dir/.operation
    (umask 077; mkdir "$_dcent_ledger_operation") 2>/dev/null || return 1
    chmod 700 "$_dcent_ledger_operation" || return 1
    _dcent_ledger_operation_id=$(dcent_sysupgrade_ledger_stat '%d:%i' \
        "$_dcent_ledger_operation") || return 1
    dcent_sysupgrade_ledger_layout "$_dcent_ledger_dir" \
        "$_dcent_ledger_layout_kind" reserved || {
            if dcent_sysupgrade_ledger_secure_dir "$_dcent_ledger_operation" &&
               [ "$(dcent_sysupgrade_ledger_stat '%d:%i' \
                    "$_dcent_ledger_operation")" = "$_dcent_ledger_operation_id" ]; then
                rmdir "$_dcent_ledger_operation" 2>/dev/null || true
            fi
            return 1
        }
}

dcent_sysupgrade_ledger_operation_release()
{
    [ "$#" -eq 1 ] || return 1
    dcent_sysupgrade_ledger_secure_dir "$1/.operation" &&
        [ "$(dcent_sysupgrade_ledger_stat '%d:%i' "$1/.operation")" = \
            "$_dcent_ledger_operation_id" ] || return 1
    rmdir "$1/.operation"
}

dcent_sysupgrade_ledger_write_resource_status()
{
    [ "$#" -eq 10 ] || return 1
    _dcent_ledger_resource_dir=$1
    _dcent_ledger_kind=$2
    _dcent_ledger_id=$3
    _dcent_ledger_intent_digest=$4
    _dcent_ledger_phase=$5
    _dcent_ledger_revision=$6
    _dcent_ledger_evidence=$7
    _dcent_ledger_previous=$8
    _dcent_ledger_actor_kind=$9
    shift 9
    _dcent_ledger_actor_id=$1
    _dcent_ledger_target=$_dcent_ledger_resource_dir/status.$_dcent_ledger_revision
    [ ! -e "$_dcent_ledger_target" ] && [ ! -L "$_dcent_ledger_target" ] || return 1
    _dcent_ledger_new=$_dcent_ledger_resource_dir/.status.new.$_dcent_ledger_revision.$$
    if ! (umask 077; set -C; printf '%s\n' \
        'schema=dcentos-sysupgrade-resource-status-v2' \
        "binding_sha256=$DCENT_SYSUPGRADE_LEDGER_BINDING_SHA256" \
        "transaction_id=$DCENT_SYSUPGRADE_LEDGER_TRANSACTION_ID" \
        "kind=$_dcent_ledger_kind" \
        "resource_id=$_dcent_ledger_id" \
        "intent_sha256=$_dcent_ledger_intent_digest" \
        "phase=$_dcent_ledger_phase" \
        "revision=$_dcent_ledger_revision" \
        "evidence_sha256=$_dcent_ledger_evidence" \
        "previous_status_sha256=$_dcent_ledger_previous" \
        "actor_kind=$_dcent_ledger_actor_kind" \
        "actor_id=$_dcent_ledger_actor_id" >"$_dcent_ledger_new"); then
        return 1
    fi
    chmod 600 "$_dcent_ledger_new" || return 1
    mv "$_dcent_ledger_new" "$_dcent_ledger_target" || return 1
    dcent_sysupgrade_ledger_secure_receipt "$_dcent_ledger_target"
}

dcent_sysupgrade_ledger_resource_pending()
{
    [ "$#" -eq 6 ] || return 1
    # Keep operation inputs in names that none of the recursive layout parsers
    # use.  POSIX shell has no local variables; a verification walk must not
    # redirect publication to the last resource it inspected.
    _dcent_ledger_pending_kind=$1
    _dcent_ledger_pending_id=$2
    _dcent_ledger_pending_provenance=$3
    _dcent_ledger_pending_a=$4
    _dcent_ledger_pending_b=$5
    _dcent_ledger_pending_c=$6
    dcent_sysupgrade_ledger_safe_id "$_dcent_ledger_pending_id" || return 1
    case "$_dcent_ledger_pending_provenance" in created|borrowed) ;;
        *) return 1 ;;
    esac
    dcent_sysupgrade_ledger_identity_valid "$_dcent_ledger_pending_kind" \
        "$_dcent_ledger_pending_a" "$_dcent_ledger_pending_b" \
        "$_dcent_ledger_pending_c" &&
        dcent_sysupgrade_ledger_verify_owned || return 1
    _dcent_ledger_resource_dir=$DCENT_SYSUPGRADE_LEDGER_DIR/resources/\
$_dcent_ledger_pending_kind--$_dcent_ledger_pending_id
    [ ! -e "$_dcent_ledger_resource_dir" ] &&
        [ ! -L "$_dcent_ledger_resource_dir" ] || return 1
    dcent_sysupgrade_ledger_operation_reserve \
        "$DCENT_SYSUPGRADE_LEDGER_DIR" owner || return 1
    dcent_sysupgrade_ledger_binding_matches_globals || return 1
    _dcent_ledger_resource_dir=$DCENT_SYSUPGRADE_LEDGER_DIR/resources/\
$_dcent_ledger_pending_kind--$_dcent_ledger_pending_id
    [ ! -e "$_dcent_ledger_resource_dir" ] &&
        [ ! -L "$_dcent_ledger_resource_dir" ] || return 1
    (umask 077; mkdir "$_dcent_ledger_resource_dir") || return 1
    chmod 700 "$_dcent_ledger_resource_dir" || return 1
    _dcent_ledger_new=$_dcent_ledger_resource_dir/.intent.new.$$
    if ! (umask 077; set -C; printf '%s\n' \
        'schema=dcentos-sysupgrade-resource-intent-v2' \
        "binding_sha256=$DCENT_SYSUPGRADE_LEDGER_BINDING_SHA256" \
        "transaction_id=$DCENT_SYSUPGRADE_LEDGER_TRANSACTION_ID" \
        "kind=$_dcent_ledger_pending_kind" \
        "resource_id=$_dcent_ledger_pending_id" \
        "provenance=$_dcent_ledger_pending_provenance" \
        "identity_a=$_dcent_ledger_pending_a" \
        "identity_b=$_dcent_ledger_pending_b" \
        "identity_c=$_dcent_ledger_pending_c" >"$_dcent_ledger_new"); then
        return 1
    fi
    chmod 600 "$_dcent_ledger_new" || return 1
    mv "$_dcent_ledger_new" "$_dcent_ledger_resource_dir/intent" || return 1
    dcent_sysupgrade_ledger_read_resource_intent \
        "$_dcent_ledger_resource_dir/intent" || return 1
    [ "$_dcent_ledger_intent_binding_sha256" = \
        "$DCENT_SYSUPGRADE_LEDGER_BINDING_SHA256" ] || return 1
    dcent_sysupgrade_ledger_write_resource_status \
        "$_dcent_ledger_resource_dir" "$_dcent_ledger_pending_kind" \
        "$_dcent_ledger_pending_id" "$_dcent_ledger_intent_sha256" pending 1 \
        "$_dcent_ledger_intent_sha256" - owner \
        "$DCENT_SYSUPGRADE_LEDGER_TRANSACTION_ID" || return 1
    dcent_sysupgrade_ledger_operation_release "$DCENT_SYSUPGRADE_LEDGER_DIR"
}

dcent_sysupgrade_ledger_resource_transition()
{
    [ "$#" -eq 5 ] || return 1
    _dcent_ledger_transition_kind=$1
    _dcent_ledger_transition_id=$2
    _dcent_ledger_transition_expected_from=$3
    _dcent_ledger_transition_to=$4
    _dcent_ledger_transition_evidence=$5
    dcent_sysupgrade_ledger_safe_id "$_dcent_ledger_transition_id" &&
        dcent_sysupgrade_ledger_sha256_valid \
            "$_dcent_ledger_transition_evidence" || return 1
    case "$DCENT_SYSUPGRADE_LEDGER_ACTOR" in
        owner)
            dcent_sysupgrade_ledger_verify_owned || return 1
            _dcent_ledger_transition_layout_kind=owner
            _dcent_ledger_transition_actor_id=$DCENT_SYSUPGRADE_LEDGER_TRANSACTION_ID
            ;;
        reconciler)
            dcent_sysupgrade_ledger_verify_reconciler &&
                [ "$_dcent_ledger_claim_latest_phase" = reconciling ] || return 1
            _dcent_ledger_transition_layout_kind=claimed
            _dcent_ledger_transition_actor_id=$DCENT_SYSUPGRADE_LEDGER_CLAIM_ID
            ;;
        *) return 1 ;;
    esac
    _dcent_ledger_resource_dir=$DCENT_SYSUPGRADE_LEDGER_DIR/resources/\
$_dcent_ledger_transition_kind--$_dcent_ledger_transition_id
    dcent_sysupgrade_ledger_read_resource_dir "$_dcent_ledger_resource_dir" \
        "$_dcent_ledger_transition_kind" "$_dcent_ledger_transition_id" \
        "$_dcent_ledger_transition_layout_kind" || return 1
    _dcent_ledger_transition_from=$_dcent_ledger_resource_latest_phase
    [ "$_dcent_ledger_transition_expected_from" = any ] ||
        [ "$_dcent_ledger_transition_from" = \
            "$_dcent_ledger_transition_expected_from" ] || return 1
    _dcent_ledger_transition_previous=$_dcent_ledger_resource_latest_status_sha256
    _dcent_ledger_transition_intent_digest=$_dcent_ledger_intent_sha256
    _dcent_ledger_transition_revision=$((_dcent_ledger_resource_latest_revision + 1))
    dcent_sysupgrade_ledger_resource_transition_valid \
        "$_dcent_ledger_transition_from" "$_dcent_ledger_transition_to" || return 1
    dcent_sysupgrade_ledger_operation_reserve \
        "$DCENT_SYSUPGRADE_LEDGER_DIR" \
        "$_dcent_ledger_transition_layout_kind" || return 1
    dcent_sysupgrade_ledger_binding_matches_globals || return 1
    _dcent_ledger_resource_dir=$DCENT_SYSUPGRADE_LEDGER_DIR/resources/\
$_dcent_ledger_transition_kind--$_dcent_ledger_transition_id
    dcent_sysupgrade_ledger_read_resource_dir "$_dcent_ledger_resource_dir" \
        "$_dcent_ledger_transition_kind" "$_dcent_ledger_transition_id" \
        "$_dcent_ledger_transition_layout_kind" || return 1
    [ "$_dcent_ledger_resource_latest_phase" = \
        "$_dcent_ledger_transition_from" ] &&
        [ "$_dcent_ledger_resource_latest_status_sha256" = \
            "$_dcent_ledger_transition_previous" ] &&
        [ "$_dcent_ledger_intent_sha256" = \
            "$_dcent_ledger_transition_intent_digest" ] || return 1
    dcent_sysupgrade_ledger_write_resource_status \
        "$_dcent_ledger_resource_dir" "$_dcent_ledger_transition_kind" \
        "$_dcent_ledger_transition_id" \
        "$_dcent_ledger_transition_intent_digest" \
        "$_dcent_ledger_transition_to" "$_dcent_ledger_transition_revision" \
        "$_dcent_ledger_transition_evidence" \
        "$_dcent_ledger_transition_previous" \
        "$DCENT_SYSUPGRADE_LEDGER_ACTOR" \
        "$_dcent_ledger_transition_actor_id" || return 1
    dcent_sysupgrade_ledger_operation_release "$DCENT_SYSUPGRADE_LEDGER_DIR"
}

dcent_sysupgrade_ledger_resource_active()
{
    [ "$#" -eq 3 ] || return 1
    dcent_sysupgrade_ledger_resource_transition "$1" "$2" pending active "$3"
}

dcent_sysupgrade_ledger_resource_release_pending()
{
    [ "$#" -eq 3 ] || return 1
    dcent_sysupgrade_ledger_resource_transition "$1" "$2" active release-pending "$3"
}

dcent_sysupgrade_ledger_resource_released()
{
    [ "$#" -eq 3 ] || return 1
    dcent_sysupgrade_ledger_resource_transition "$1" "$2" \
        release-pending released "$3"
}

dcent_sysupgrade_ledger_resource_absent_released()
{
    [ "$#" -eq 3 ] || return 1
    dcent_sysupgrade_ledger_resource_transition "$1" "$2" pending released "$3"
}

dcent_sysupgrade_ledger_resource_conflict()
{
    [ "$#" -eq 3 ] || return 1
    dcent_sysupgrade_ledger_resource_transition "$1" "$2" any conflict "$3"
}

dcent_sysupgrade_ledger_resource_expect()
{
    [ "$#" -eq 8 ] || return 1
    case "$DCENT_SYSUPGRADE_LEDGER_ACTOR" in
        owner)
            dcent_sysupgrade_ledger_verify_owned || return 1
            _dcent_ledger_layout_kind=owner
            ;;
        reconciler)
            dcent_sysupgrade_ledger_verify_reconciler || return 1
            _dcent_ledger_layout_kind=claimed
            ;;
        *) return 1 ;;
    esac
    _dcent_ledger_resource_dir=$DCENT_SYSUPGRADE_LEDGER_DIR/resources/$1--$2
    dcent_sysupgrade_ledger_read_resource_dir "$_dcent_ledger_resource_dir" \
        "$1" "$2" "$_dcent_ledger_layout_kind" || return 1
    [ "$_dcent_ledger_intent_provenance" = "$3" ] &&
        [ "$_dcent_ledger_resource_latest_phase" = "$4" ] &&
        [ "$_dcent_ledger_resource_latest_evidence_sha256" = "$5" ] &&
        [ "$_dcent_ledger_intent_a" = "$6" ] &&
        [ "$_dcent_ledger_intent_b" = "$7" ] &&
        [ "$_dcent_ledger_intent_c" = "$8" ]
}

dcent_sysupgrade_ledger_write_claim_status()
{
    [ "$#" -eq 7 ] || return 1
    _dcent_ledger_claim_dir=$1
    _dcent_ledger_phase=$2
    _dcent_ledger_revision=$3
    _dcent_ledger_quiescence=$4
    _dcent_ledger_outcome=$5
    _dcent_ledger_previous=$6
    _dcent_ledger_claim_id_arg=$7
    case "$_dcent_ledger_phase:$_dcent_ledger_revision" in
        claimed:1|quiescent:2|reconciling:3|complete:4|\
        blocked:2|blocked:3|blocked:4) ;;
        *) return 1 ;;
    esac
    dcent_sysupgrade_ledger_sha256_or_dash_valid \
        "$_dcent_ledger_quiescence" &&
        dcent_sysupgrade_ledger_sha256_or_dash_valid \
            "$_dcent_ledger_outcome" &&
        dcent_sysupgrade_ledger_sha256_or_dash_valid \
            "$_dcent_ledger_previous" &&
        dcent_sysupgrade_ledger_safe_id "$_dcent_ledger_claim_id_arg" || return 1
    _dcent_ledger_target=$_dcent_ledger_claim_dir/status.$_dcent_ledger_revision
    [ ! -e "$_dcent_ledger_target" ] && [ ! -L "$_dcent_ledger_target" ] || return 1
    _dcent_ledger_new=$_dcent_ledger_claim_dir/.status.new.$_dcent_ledger_revision.$$
    if ! (umask 077; set -C; printf '%s\n' \
        'schema=dcentos-sysupgrade-reconcile-status-v2' \
        "claim_intent_sha256=$_dcent_ledger_claim_intent_sha256" \
        "phase=$_dcent_ledger_phase" \
        "revision=$_dcent_ledger_revision" \
        "quiescence_sha256=$_dcent_ledger_quiescence" \
        "outcome_sha256=$_dcent_ledger_outcome" \
        "previous_status_sha256=$_dcent_ledger_previous" \
        "actor_id=$_dcent_ledger_claim_id_arg" >"$_dcent_ledger_new"); then
        return 1
    fi
    chmod 600 "$_dcent_ledger_new" || return 1
    mv "$_dcent_ledger_new" "$_dcent_ledger_target" || return 1
    dcent_sysupgrade_ledger_secure_receipt "$_dcent_ledger_target"
}

dcent_sysupgrade_ledger_reconcile_claim()
{
    [ "$#" -eq 10 ] || return 1
    [ "$DCENT_SYSUPGRADE_LEDGER_BOUND" = 0 ] || return 1
    _dcent_ledger_dir=$1
    _dcent_ledger_tx=$2
    _dcent_ledger_claim_id_arg=$3
    _dcent_ledger_claim_boot_arg=$4
    _dcent_ledger_claim_pid_arg=$5
    _dcent_ledger_claim_start_arg=$6
    _dcent_ledger_claim_mntns_arg=$7
    _dcent_ledger_owner_death_arg=$8
    _dcent_ledger_maintenance_lock_arg=$9
    shift 9
    _dcent_ledger_maintenance_devino_arg=$1
    dcent_sysupgrade_ledger_safe_id "$_dcent_ledger_tx" &&
        dcent_sysupgrade_ledger_safe_id "$_dcent_ledger_claim_id_arg" &&
        dcent_sysupgrade_ledger_boot_id_valid "$_dcent_ledger_claim_boot_arg" &&
        dcent_sysupgrade_ledger_uint_valid "$_dcent_ledger_claim_pid_arg" &&
        dcent_sysupgrade_ledger_uint_valid "$_dcent_ledger_claim_start_arg" &&
        dcent_sysupgrade_ledger_devino_valid "$_dcent_ledger_claim_mntns_arg" &&
        dcent_sysupgrade_ledger_sha256_valid "$_dcent_ledger_owner_death_arg" &&
        dcent_sysupgrade_ledger_absolute_path_valid \
            "$_dcent_ledger_maintenance_lock_arg" &&
        dcent_sysupgrade_ledger_devino_valid \
            "$_dcent_ledger_maintenance_devino_arg" || return 1
    dcent_sysupgrade_ledger_secure_dir \
        "$_dcent_ledger_maintenance_lock_arg" || return 1
    [ "$(dcent_sysupgrade_ledger_stat '%d:%i' \
        "$_dcent_ledger_maintenance_lock_arg")" = \
        "$_dcent_ledger_maintenance_devino_arg" ] || return 1
    dcent_sysupgrade_ledger_layout "$_dcent_ledger_dir" owner clean || return 1
    [ "$_dcent_ledger_parsed_transaction_id" = "$_dcent_ledger_tx" ] || return 1
    _dcent_ledger_binding_digest=$_dcent_ledger_parsed_binding_sha256
    dcent_sysupgrade_ledger_operation_reserve "$_dcent_ledger_dir" owner || return 1
    [ "$_dcent_ledger_parsed_binding_sha256" = \
        "$_dcent_ledger_binding_digest" ] || return 1
    _dcent_ledger_claim_dir=$_dcent_ledger_dir/reconcile.claim
    (umask 077; mkdir "$_dcent_ledger_claim_dir") 2>/dev/null || return 1
    chmod 700 "$_dcent_ledger_claim_dir" || return 1
    _dcent_ledger_new=$_dcent_ledger_claim_dir/.intent.new.$$
    if ! (umask 077; set -C; printf '%s\n' \
        'schema=dcentos-sysupgrade-reconcile-intent-v2' \
        "binding_sha256=$_dcent_ledger_binding_digest" \
        "transaction_id=$_dcent_ledger_tx" \
        "claim_id=$_dcent_ledger_claim_id_arg" \
        "reconciler_boot_id=$_dcent_ledger_claim_boot_arg" \
        "reconciler_pid=$_dcent_ledger_claim_pid_arg" \
        "reconciler_starttime=$_dcent_ledger_claim_start_arg" \
        "reconciler_mount_namespace=$_dcent_ledger_claim_mntns_arg" \
        "owner_death_evidence_sha256=$_dcent_ledger_owner_death_arg" \
        "maintenance_lock_path=$_dcent_ledger_maintenance_lock_arg" \
        "maintenance_lock_device_inode=$_dcent_ledger_maintenance_devino_arg" \
        'owner=zynq-sysupgrade-reconciler' >"$_dcent_ledger_new"); then
        return 1
    fi
    chmod 600 "$_dcent_ledger_new" || return 1
    mv "$_dcent_ledger_new" "$_dcent_ledger_claim_dir/intent" || return 1
    dcent_sysupgrade_ledger_read_claim_intent \
        "$_dcent_ledger_claim_dir/intent" || return 1
    [ "$_dcent_ledger_claim_binding_sha256" = \
        "$_dcent_ledger_binding_digest" ] || return 1
    dcent_sysupgrade_ledger_bind_reconciler "$_dcent_ledger_dir" \
        "$_dcent_ledger_claim_id_arg" "$_dcent_ledger_claim_boot_arg" \
        "$_dcent_ledger_claim_pid_arg" "$_dcent_ledger_claim_start_arg" \
        "$_dcent_ledger_claim_mntns_arg"
    dcent_sysupgrade_ledger_write_claim_status "$_dcent_ledger_claim_dir" \
        claimed 1 - - - "$_dcent_ledger_claim_id_arg" || return 1
    dcent_sysupgrade_ledger_operation_release "$_dcent_ledger_dir" || return 1
    dcent_sysupgrade_ledger_verify_reconciler
}

dcent_sysupgrade_ledger_reconcile_open()
{
    [ "$#" -eq 7 ] || return 1
    [ "$DCENT_SYSUPGRADE_LEDGER_BOUND" = 0 ] || return 1
    dcent_sysupgrade_ledger_layout "$1" claimed clean || return 1
    [ "$_dcent_ledger_parsed_transaction_id" = "$2" ] &&
        [ "$_dcent_ledger_claim_id" = "$3" ] &&
        [ "$_dcent_ledger_claim_boot_id" = "$4" ] &&
        [ "$_dcent_ledger_claim_pid" = "$5" ] &&
        [ "$_dcent_ledger_claim_starttime" = "$6" ] &&
        [ "$_dcent_ledger_claim_mount_namespace" = "$7" ] || return 1
    dcent_sysupgrade_ledger_bind_reconciler "$1" "$3" "$4" "$5" "$6" "$7"
}

dcent_sysupgrade_ledger_claim_transition()
{
    [ "$#" -eq 5 ] || return 1
    _dcent_ledger_claim_transition_id=$1
    _dcent_ledger_claim_transition_from=$2
    _dcent_ledger_claim_transition_to=$3
    _dcent_ledger_claim_transition_quiescence=$4
    _dcent_ledger_claim_transition_outcome=$5
    dcent_sysupgrade_ledger_verify_reconciler || return 1
    [ "$_dcent_ledger_claim_id" = "$_dcent_ledger_claim_transition_id" ] &&
        [ "$_dcent_ledger_claim_latest_phase" = \
            "$_dcent_ledger_claim_transition_from" ] || return 1
    dcent_sysupgrade_ledger_claim_transition_valid \
        "$_dcent_ledger_claim_transition_from" \
        "$_dcent_ledger_claim_transition_to" || return 1
    case "$_dcent_ledger_claim_transition_to" in
        quiescent)
            dcent_sysupgrade_ledger_sha256_valid \
                "$_dcent_ledger_claim_transition_quiescence" &&
                [ "$_dcent_ledger_claim_transition_outcome" = - ] || return 1
            ;;
        reconciling)
            [ "$_dcent_ledger_claim_transition_quiescence" = \
                "$_dcent_ledger_claim_latest_quiescence" ] &&
                [ "$_dcent_ledger_claim_transition_outcome" = - ] || return 1
            ;;
        complete)
            [ "$_dcent_ledger_claim_transition_quiescence" = \
                "$_dcent_ledger_claim_latest_quiescence" ] &&
                dcent_sysupgrade_ledger_sha256_valid \
                    "$_dcent_ledger_claim_transition_outcome" || return 1
            dcent_sysupgrade_ledger_resources_all_released || return 1
            ;;
        blocked)
            [ "$_dcent_ledger_claim_transition_quiescence" = \
                "$_dcent_ledger_claim_latest_quiescence" ] &&
                dcent_sysupgrade_ledger_sha256_valid \
                    "$_dcent_ledger_claim_transition_outcome" || return 1
            ;;
        *) return 1 ;;
    esac
    _dcent_ledger_claim_transition_previous=$_dcent_ledger_claim_latest_status_sha256
    _dcent_ledger_claim_transition_revision=$((_dcent_ledger_claim_latest_revision + 1))
    dcent_sysupgrade_ledger_operation_reserve \
        "$DCENT_SYSUPGRADE_LEDGER_DIR" claimed || return 1
    dcent_sysupgrade_ledger_binding_matches_globals || return 1
    dcent_sysupgrade_ledger_read_claim_dir \
        "$DCENT_SYSUPGRADE_LEDGER_DIR/reconcile.claim" || return 1
    [ "$_dcent_ledger_claim_latest_phase" = \
        "$_dcent_ledger_claim_transition_from" ] &&
        [ "$_dcent_ledger_claim_latest_status_sha256" = \
            "$_dcent_ledger_claim_transition_previous" ] || return 1
    dcent_sysupgrade_ledger_write_claim_status \
        "$DCENT_SYSUPGRADE_LEDGER_DIR/reconcile.claim" \
        "$_dcent_ledger_claim_transition_to" \
        "$_dcent_ledger_claim_transition_revision" \
        "$_dcent_ledger_claim_transition_quiescence" \
        "$_dcent_ledger_claim_transition_outcome" \
        "$_dcent_ledger_claim_transition_previous" \
        "$_dcent_ledger_claim_transition_id" || return 1
    dcent_sysupgrade_ledger_operation_release "$DCENT_SYSUPGRADE_LEDGER_DIR"
}

dcent_sysupgrade_ledger_resources_all_released()
{
    dcent_sysupgrade_ledger_verify_reconciler || return 1
    for _dcent_ledger_entry in "$DCENT_SYSUPGRADE_LEDGER_DIR/resources"/* \
        "$DCENT_SYSUPGRADE_LEDGER_DIR/resources"/.[!.]* \
        "$DCENT_SYSUPGRADE_LEDGER_DIR/resources"/..?*; do
        [ -e "$_dcent_ledger_entry" ] || [ -L "$_dcent_ledger_entry" ] || continue
        _dcent_ledger_base=${_dcent_ledger_entry##*/}
        _dcent_ledger_kind=${_dcent_ledger_base%%--*}
        _dcent_ledger_id=${_dcent_ledger_base#*--}
        dcent_sysupgrade_ledger_read_resource_dir "$_dcent_ledger_entry" \
            "$_dcent_ledger_kind" "$_dcent_ledger_id" claimed || return 1
        [ "$_dcent_ledger_resource_latest_phase" = released ] || return 1
    done
}

dcent_sysupgrade_ledger_reconcile_quiescent()
{
    [ "$#" -eq 2 ] || return 1
    dcent_sysupgrade_ledger_claim_transition "$1" claimed quiescent "$2" -
}

dcent_sysupgrade_ledger_reconcile_begin()
{
    [ "$#" -eq 2 ] || return 1
    dcent_sysupgrade_ledger_claim_transition "$1" quiescent reconciling "$2" -
}

dcent_sysupgrade_ledger_reconcile_complete()
{
    [ "$#" -eq 3 ] || return 1
    dcent_sysupgrade_ledger_claim_transition "$1" reconciling complete "$2" "$3"
}

dcent_sysupgrade_ledger_reconcile_block()
{
    [ "$#" -eq 2 ] || return 1
    dcent_sysupgrade_ledger_verify_reconciler || return 1
    case "$_dcent_ledger_claim_latest_phase" in
        claimed|quiescent|reconciling) ;;
        *) return 1 ;;
    esac
    dcent_sysupgrade_ledger_claim_transition "$1" \
        "$_dcent_ledger_claim_latest_phase" blocked \
        "$_dcent_ledger_claim_latest_quiescence" "$2"
}
