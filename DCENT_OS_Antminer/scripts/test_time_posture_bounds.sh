#!/bin/sh
#
# Static time/NTP posture audit.
#
# Host-only source check. It never contacts devices or network services. This
# pins the no-RTC mitigations and the wall-clock contracts that matter before
# public beta:
#   - S41ntp restores a plausible /data last-good mark before network,
#   - SNTP is one-shot and never blocks rcS,
#   - last-good time is persisted atomically and never overwritten with 1970,
#   - auth idle/rate-limit timers use monotonic Instant while absolute TTL uses
#     wall clock,
#   - night/curtail schedules share a bounded UTC-offset helper.

set -eu

SCRIPT_DIR=$(CDPATH= cd "$(dirname "$0")" && pwd)
PROJECT_DIR=$(CDPATH= cd "$SCRIPT_DIR/.." && pwd)
cd "$PROJECT_DIR"

failures=0

pass() {
    printf 'PASS: %s\n' "$*"
}

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    failures=$((failures + 1))
}

require_file() {
    if [ -f "$1" ]; then
        pass "required file exists: $1"
    else
        fail "required file missing: $1"
    fi
}

require_pattern() {
    _rp_file=$1
    _rp_pattern=$2
    _rp_label=$3

    if [ ! -f "$_rp_file" ]; then
        fail "$_rp_label: missing file $_rp_file"
        return
    fi

    if grep -F -- "$_rp_pattern" "$_rp_file" >/dev/null 2>&1; then
        pass "$_rp_label"
    else
        fail "$_rp_label: missing pattern '$_rp_pattern' in $_rp_file"
    fi
}

require_ntp_script() {
    _ntp_file=$1
    _ntp_label=$2
    require_file "$_ntp_file"
    require_pattern "$_ntp_file" 'STAMP_FILE="/data/last-good-time"' \
        "$_ntp_label persists the last-good wall clock under /data"
    require_pattern "$_ntp_file" 'MIN_PLAUSIBLE_EPOCH=1735689600' \
        "$_ntp_label refuses 1970/too-old clock stamps"
    require_pattern "$_ntp_file" 'mv "$_tmp" "$STAMP_FILE"' \
        "$_ntp_label writes last-good time atomically"
    require_pattern "$_ntp_file" 'if [ "$_n" -lt "$_saved" ]' \
        "$_ntp_label only restores clock forward from saved mark"
    require_pattern "$_ntp_file" 'date -s "@$_saved"' \
        "$_ntp_label can restore from epoch seconds"
    require_pattern "$_ntp_file" 'ntpd -q -n $_args' \
        "$_ntp_label uses one-shot foreground SNTP"
    require_pattern "$_ntp_file" 'wait_for_ip 60' \
        "$_ntp_label bounds DHCP wait before SNTP"
    require_pattern "$_ntp_file" 'do_sync_and_persist &' \
        "$_ntp_label runs sync/persist without blocking rcS"
    require_pattern "$_ntp_file" 'sleep 600' \
        "$_ntp_label periodically refreshes last-good time"
}

zynq_ntp='br2_external_dcentos/board/zynq/rootfs-overlay/etc/init.d/S41ntp'
amlogic_ntp='br2_external_dcentos/board/amlogic/rootfs-overlay/etc/init.d/S41ntp'
zynq_busybox='br2_external_dcentos/board/zynq/busybox.config'
amlogic_busybox='br2_external_dcentos/board/amlogic/busybox-ntpd.fragment'
amlogic_common='br2_external_dcentos/configs/dcentos_am3_aml_common.fragment'
auth_rs='dcentrald/dcentrald-api/src/auth.rs'
time_rs='dcentrald/dcentrald-common/src/time.rs'
config_rs='dcentrald/dcentrald/src/config.rs'
daemon_rs='dcentrald/dcentrald/src/daemon.rs'
heater_rs='dcentrald/dcentrald-thermal/src/heater.rs'

require_ntp_script "$zynq_ntp" 'zynq S41ntp'
require_ntp_script "$amlogic_ntp" 'amlogic S41ntp'

require_file "$zynq_busybox"
require_file "$amlogic_busybox"
require_file "$amlogic_common"
require_pattern "$zynq_busybox" 'CONFIG_NTPD=y' \
    'zynq busybox enables one-shot SNTP applet'
require_pattern "$amlogic_busybox" 'CONFIG_NTPD=y' \
    'amlogic busybox fragment enables one-shot SNTP applet'
require_pattern "$amlogic_common" 'BR2_PACKAGE_BUSYBOX_CONFIG_FRAGMENT_FILES="$(BR2_EXTERNAL_DCENTOS_PATH)/board/amlogic/busybox-ntpd.fragment"' \
    'amlogic defconfig fragment wires ntpd applet into builds'

require_file "$auth_rs"
require_pattern "$auth_rs" 'use std::time::Instant;' \
    'auth imports monotonic Instant for rate limits and idle timeout'
require_pattern "$auth_rs" 'static SESSION_LAST_SEEN: LazyLock<Mutex<HashMap<String, Instant>>>' \
    'auth idle tracking is process-local monotonic state'
require_pattern "$auth_rs" 'now.duration_since(last_seen).as_secs() >= timeout' \
    'auth idle timeout compares monotonic Instants'
require_pattern "$auth_rs" 'const SESSION_TTL_SECS: u64 = 30 * 24 * 60 * 60;' \
    'auth absolute session TTL remains explicit'
require_pattern "$auth_rs" 'let expires_at = Some((now_s + SESSION_TTL_SECS).to_string());' \
    'auth absolute session TTL is persisted as epoch seconds'

require_file "$time_rs"
require_pattern "$time_rs" 'pub const MIN_TZ_OFFSET_HOURS: i8 = -12;' \
    'shared time helper pins minimum UTC offset'
require_pattern "$time_rs" 'pub const MAX_TZ_OFFSET_HOURS: i8 = 14;' \
    'shared time helper pins maximum UTC offset'
require_pattern "$time_rs" 'pub fn local_hour_from_utc(utc_hour: u8, offset_hours: i8) -> u8' \
    'shared time helper converts UTC to local hour'
require_pattern "$time_rs" 'pub fn is_valid_tz_offset(offset_hours: i8) -> bool' \
    'shared time helper validates configured timezone offsets'
require_pattern "$time_rs" 'fn negative_offset_wraps_backward_past_midnight()' \
    'shared time helper tests negative offset wraparound'

require_file "$config_rs"
require_file "$daemon_rs"
require_pattern "$config_rs" 'thermal.night_mode.timezone_offset_hours' \
    'daemon config exposes night-mode timezone offset'
require_pattern "$config_rs" 'dcentrald_common::time::is_valid_tz_offset' \
    'daemon config rejects invalid schedule timezone offsets'
require_pattern "$daemon_rs" 'dcentrald_common::time::local_hour_from_utc' \
    'daemon schedule evaluation uses shared local-hour helper'
require_pattern "$daemon_rs" 'FrequencyLimitSource::QuietMode' \
    'night mode frequency cap source remains explicit'

require_file "$heater_rs"
require_pattern "$heater_rs" 'Applied to SystemTime to derive local wall-clock hour for night mode scheduling.' \
    'heater night-mode contract documents wall-clock scheduling'
require_pattern "$heater_rs" 'rem_euclid(86400)' \
    'heater night-mode local-hour math wraps negative offsets'

if [ "$failures" -ne 0 ]; then
    printf '\ntime/NTP posture audit failed: %s failure(s)\n' "$failures" >&2
    exit 1
fi

printf '\ntime/NTP posture audit passed.\n'
