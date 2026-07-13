#!/bin/sh
# ============================================================================
# firmware_release_name.sh — SINGLE SOURCE OF TRUTH for the DCENT_OS
# standardized release/package name. Called by the build so compiled images
# are auto-named (no hand-typed names). Operator directive 2026-06-14.
# ----------------------------------------------------------------------------
# Convention:
#
#     DCENTOS_<BOARD><GEN>_<MODEL>_<CHANNEL><YYYYMMDD>
#
#   BOARD   control-board arch/family:
#           XIL (Xilinx Zynq) | BB (BeagleBone AM335x) | AML (Amlogic)
#           ESP (ESP32-S3 Bitaxe) | H616 (Whatsminer H616) | K230 (Avalon K230)
#   GEN     miner generation digit where the family uses one:
#           1=S9/BM1387, 2=S17/BM1397-98, 3=S19/S19jPro/S21/BM136x/BM137x.
#           H616/K230 targets omit GEN until their release families are proven.
#   MODEL   miner model, CamelCase: S9 S17 S19Pro S19jPro S19kPro S21 T21 ...
#   CHANNEL lowercase release channel: beta (also dev|rc|stable)
#   YYYYMMDD build/release date
#
#   Operator-canonical examples:
#     DCENTOS_XIL1_S9_beta20260614
#     DCENTOS_XIL2_S17_beta20260614
#     DCENTOS_XIL3_S19jPro_beta20260614
#     DCENTOS_BB3_S19jPro_beta20260614
#     DCENTOS_AML3_S21_beta20260614
#     DCENTOS_ESP3_BitaxeGamma_beta20260614
#     DCENTOS_H616_M60S_dev20260614
#     DCENTOS_K230_AvalonQ_dev20260614
#
# Usage:
#   firmware_release_name.sh <build_target> [channel] [YYYYMMDD]
#     build_target  one of the build_in_docker.sh targets (see the case below)
#     channel       default: beta
#     YYYYMMDD      default: today (date +%Y%m%d)
#
# Exit codes: 0 ok (prints the name on stdout); 2 unknown target; 3 bad args.
# ============================================================================
set -eu

target="${1:-}"
channel="${2:-beta}"
date_stamp="${3:-$(date +%Y%m%d)}"

if [ -z "$target" ]; then
    echo "usage: $0 <build_target> [channel] [YYYYMMDD]" >&2
    exit 3
fi

# Validate channel + date shape (fail closed on typos so a bad release name
# can't reach an artifact).
case "$channel" in
    beta|dev|rc|stable) ;;
    *) echo "error: channel must be beta|dev|rc|stable, got '$channel'" >&2; exit 3 ;;
esac
case "$date_stamp" in
    [0-9][0-9][0-9][0-9][0-1][0-9][0-3][0-9]) ;;
    *) echo "error: date must be YYYYMMDD, got '$date_stamp'" >&2; exit 3 ;;
esac

# build_target -> release stem (BOARD+GEN+MODEL). Keep in lockstep with
# build_in_docker.sh's target list and the memory rule's mapping table.
case "$target" in
    s9|am1-s9)             stem="DCENTOS_XIL1_S9" ;;
    am1-s15)               stem="DCENTOS_XIL1_S15" ;;
    am1-t15)               stem="DCENTOS_XIL1_T15" ;;
    am2-s17p|am2-s17pro)   stem="DCENTOS_XIL2_S17" ;;
    am2-s17plus)           stem="DCENTOS_XIL2_S17Plus" ;;
    am2-t17)               stem="DCENTOS_XIL2_T17" ;;
    am2-t17plus)           stem="DCENTOS_XIL2_T17Plus" ;;
    x17-s17e-dspic-planned) stem="DCENTOS_XIL2_S17e" ;;
    x17-t17e-pic16-planned) stem="DCENTOS_XIL2_T17e" ;;
    am2-s19)               stem="DCENTOS_XIL3_S19" ;;
    am2-s19pro)            stem="DCENTOS_XIL3_S19Pro" ;;   # BM1398. GEN=3 (S19-era); operator to confirm 2 vs 3.
    am2-s19j)              stem="DCENTOS_XIL3_S19j" ;;
    am2-s19jpro|am2-s19jpro-zynq|am2-s19jpro-sd) stem="DCENTOS_XIL3_S19jPro" ;;
    am2-t19)               stem="DCENTOS_XIL3_T19" ;;
    am3-bb|am3-bb-s19jpro|am3-bb-s19jpro-vnish) stem="DCENTOS_BB3_S19jPro" ;;
    cv1835-s19jpro)        stem="DCENTOS_CV3_S19jPro" ;;
    am3-s21)               stem="DCENTOS_AML3_S21" ;;
    am3-s19k|am3-s19kpro)  stem="DCENTOS_AML3_S19kPro" ;;
    am3-s19xp)             stem="DCENTOS_AML3_S19XP" ;;
    am3-s19jpro-aml)       stem="DCENTOS_AML3_S19jPro" ;;
    am3-t21)               stem="DCENTOS_AML3_T21" ;;
    am3-s21pro)            stem="DCENTOS_AML3_S21Pro" ;;
    am3-s21xp)             stem="DCENTOS_AML3_S21XP" ;;
    bitaxe-max|esp-bitaxe-max)             stem="DCENTOS_ESP2_BitaxeMax" ;;
    bitaxe-ultra|esp-bitaxe-ultra)         stem="DCENTOS_ESP3_BitaxeUltra" ;;
    bitaxe-supra|esp-bitaxe-supra)         stem="DCENTOS_ESP3_BitaxeSupra" ;;
    bitaxe-gamma|esp-bitaxe-gamma)         stem="DCENTOS_ESP3_BitaxeGamma" ;;
    bitaxe-hex-ultra)                      stem="DCENTOS_ESP3_BitaxeHexUltra" ;;
    bitaxe-gamma-duo)                      stem="DCENTOS_ESP3_BitaxeGammaDuo" ;;
    bitaxe-hex-supra)                      stem="DCENTOS_ESP3_BitaxeHexSupra" ;;
    bitaxe-gamma-turbo|bitaxe-gt)          stem="DCENTOS_ESP3_BitaxeGammaTurbo" ;;
    dcent-axe-900-920)                     stem="DCENTOS_ESP2_DCENTAxe" ;;
    dcent-axe-bm1397)                      stem="DCENTOS_ESP2_DCENTAxeBm1397" ;;
    dcent-axe-quad-bm1397)                 stem="DCENTOS_ESP2_DCENTAxeQuadBm1397" ;;
    dcent-axe-hex-bm1397)                  stem="DCENTOS_ESP2_DCENTAxeHexBm1397" ;;
    wm-h616-m60s|whatsminer-m60s)          stem="DCENTOS_H616_M60S" ;;
    avalon-k230-q|avalon-q-k230)           stem="DCENTOS_K230_AvalonQ" ;;
    avalon-k230-nano3|avalon-nano3)        stem="DCENTOS_K230_Nano3" ;;
    innosilicon-t2tz)                      stem="DCENTOS_INNO_T2Tz" ;;
    *)
        echo "error: unknown build target '$target' — add it to firmware_release_name.sh" >&2
        exit 2
        ;;
esac

printf '%s_%s%s\n' "$stem" "$channel" "$date_stamp"
