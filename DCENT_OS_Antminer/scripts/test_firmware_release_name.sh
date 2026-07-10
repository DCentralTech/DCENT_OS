#!/bin/sh
# Host-only self-test for firmware_release_name.sh. No artifacts are opened,
# built, published, uploaded, or installed.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
HELPER="$SCRIPT_DIR/firmware_release_name.sh"
DATE_STAMP=20260705

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

check_name() {
    target=$1
    channel=$2
    expected=$3
    got=$(sh "$HELPER" "$target" "$channel" "$DATE_STAMP")
    [ "$got" = "$expected" ] || fail "$target/$channel expected $expected got $got"
}

check_rejects() {
    expected_rc=$1
    shift
    set +e
    sh "$HELPER" "$@" >/dev/null 2>&1
    rc=$?
    set -e
    [ "$rc" -eq "$expected_rc" ] || fail "$* expected rc=$expected_rc got rc=$rc"
}

check_name s9 beta "DCENTOS_XIL1_S9_beta$DATE_STAMP"
check_name am1-s15 beta "DCENTOS_XIL1_S15_beta$DATE_STAMP"
check_name am1-t15 beta "DCENTOS_XIL1_T15_beta$DATE_STAMP"
check_name am2-s17p beta "DCENTOS_XIL2_S17_beta$DATE_STAMP"
check_name am2-s17plus beta "DCENTOS_XIL2_S17Plus_beta$DATE_STAMP"
check_name am2-t17 beta "DCENTOS_XIL2_T17_beta$DATE_STAMP"
check_name am2-t17plus beta "DCENTOS_XIL2_T17Plus_beta$DATE_STAMP"
check_name x17-s17e-dspic-planned beta "DCENTOS_XIL2_S17e_beta$DATE_STAMP"
check_name x17-t17e-pic16-planned beta "DCENTOS_XIL2_T17e_beta$DATE_STAMP"
check_name am2-s19 beta "DCENTOS_XIL3_S19_beta$DATE_STAMP"
check_name am2-s19j beta "DCENTOS_XIL3_S19j_beta$DATE_STAMP"
check_name am2-s19jpro beta "DCENTOS_XIL3_S19jPro_beta$DATE_STAMP"
check_name am2-s19jpro-zynq beta "DCENTOS_XIL3_S19jPro_beta$DATE_STAMP"
check_name am2-t19 beta "DCENTOS_XIL3_T19_beta$DATE_STAMP"
check_name am3-s19k beta "DCENTOS_AML3_S19kPro_beta$DATE_STAMP"
check_name am3-s19xp beta "DCENTOS_AML3_S19XP_beta$DATE_STAMP"
check_name am3-s21 rc "DCENTOS_AML3_S21_rc$DATE_STAMP"
check_name am3-s21pro beta "DCENTOS_AML3_S21Pro_beta$DATE_STAMP"
check_name am3-s21xp beta "DCENTOS_AML3_S21XP_beta$DATE_STAMP"

check_name bitaxe-gamma dev "DCENTOS_ESP3_BitaxeGamma_dev$DATE_STAMP"
check_name bitaxe-hex-ultra beta "DCENTOS_ESP3_BitaxeHexUltra_beta$DATE_STAMP"
check_name bitaxe-gamma-duo beta "DCENTOS_ESP3_BitaxeGammaDuo_beta$DATE_STAMP"
check_name bitaxe-hex-supra beta "DCENTOS_ESP3_BitaxeHexSupra_beta$DATE_STAMP"
check_name bitaxe-gamma-turbo beta "DCENTOS_ESP3_BitaxeGammaTurbo_beta$DATE_STAMP"
check_name bitaxe-gt beta "DCENTOS_ESP3_BitaxeGammaTurbo_beta$DATE_STAMP"
check_name dcent-axe-900-920 beta "DCENTOS_ESP2_DCENTAxe_beta$DATE_STAMP"
check_name dcent-axe-bm1397 beta "DCENTOS_ESP2_DCENTAxeBm1397_beta$DATE_STAMP"
check_name dcent-axe-quad-bm1397 beta "DCENTOS_ESP2_DCENTAxeQuadBm1397_beta$DATE_STAMP"
check_name dcent-axe-hex-bm1397 beta "DCENTOS_ESP2_DCENTAxeHexBm1397_beta$DATE_STAMP"
check_name whatsminer-m60s rc "DCENTOS_H616_M60S_rc$DATE_STAMP"
check_name avalon-q-k230 stable "DCENTOS_K230_AvalonQ_stable$DATE_STAMP"
check_name avalon-nano3 beta "DCENTOS_K230_Nano3_beta$DATE_STAMP"
check_name innosilicon-t2tz dev "DCENTOS_INNO_T2Tz_dev$DATE_STAMP"

check_rejects 2 no-such-target beta "$DATE_STAMP"
check_rejects 3 s9 nightly "$DATE_STAMP"
check_rejects 3 s9 beta 2026-07-05

printf 'FIRMWARE_RELEASE_NAME_TEST_OK date=%s\n' "$DATE_STAMP"
