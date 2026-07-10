#!/bin/sh
#
# dcent-accept.sh — DCENT_OS one-command hardware acceptance harness.
#
# Release Commander goal: reduce the human job to  ->  plug in miner, press Enter,
# watch PASS or FAIL. This harness drives the full reversible acceptance flow for
# the Antminer rows declared in skus.conf and exits 0 (PASS) / 1 (FAIL) /
# 2 (setup error) so it drops straight into CI, a dashboard, or an operator run.
#
#   ./dcent-accept.sh all S19jPro 203.0.113.25          # full flow, reversible
#   ./dcent-accept.sh shares S21 203.0.113.135     # just the accept gate
#   ./dcent-accept.sh list                           # SKU table + release states
#
# SAFETY (load-bearing — do NOT weaken):
#   * This harness NEVER writes NAND / persistent flash and NEVER raises fan PWM.
#     `all` is reversible /tmp-first only (a reboot fully reverts). Persistent
#     install is a separate, explicitly operator-gated step (see `install-hint`),
#     never run automatically.
#   * Backup runs FIRST and must pass before first-light.
#   * fan cap: the deployed config carries thermal.fan_max_pwm<=30 (home/quiet).
#
# The PASS/FAIL decision comes from lib/accept_parse.sh (unit-tested, hardware-free,
# CI-gated) reading the firmware-agnostic CGMiner Accepted counter on port 4028 —
# so this gate reads the same on DCENT_OS, BraiinsOS, LuxOS, or stock cgminer.
#
# POSIX sh. Needs: ssh, scp, nc (or curl fallback). Windows operators: run under
# WSL/Git-Bash, or use the node helpers in tools/ for the ssh/scp legs.

set -u

DIR=$(CDPATH= cd "$(dirname "$0")" && pwd)
SCRIPTS_DIR=$(CDPATH= cd "$DIR/.." && pwd)
CONF="$DIR/skus.conf"

# shellcheck source=lib/accept_parse.sh
. "$DIR/lib/accept_parse.sh"

# ---- knobs (env-overridable) ----------------------------------------------
POOL=${DCENT_ACCEPT_POOL:-stratum+tcp://public-pool.io:21496}
API_PORT=${DCENT_ACCEPT_API_PORT:-4028}
REST_PORT=${DCENT_ACCEPT_REST_PORT:-8080}
FIRSTLIGHT_N=${DCENT_ACCEPT_FIRSTLIGHT_N:-1}     # accept-gate threshold, first light
FIRSTLIGHT_T=${DCENT_ACCEPT_FIRSTLIGHT_T:-180}   # first-light window (s)
CAPSTONE_N=${DCENT_ACCEPT_CAPSTONE_N:-5}         # accept-gate threshold, capstone soak
CAPSTONE_T=${DCENT_ACCEPT_CAPSTONE_T:-600}       # capstone window (s)
POLL=${DCENT_ACCEPT_POLL:-10}                    # accept-gate poll interval (s)
TEMP_CEILING=${DCENT_ACCEPT_TEMP_CEILING:-75}    # overtemp abort ceiling (deg C; DCENT_OS 'dangerous' threshold)

say()  { printf '%s\n' "$*"; }
info() { printf '[dcent-accept] %s\n' "$*"; }
err()  { printf '[dcent-accept] ERROR: %s\n' "$*" >&2; }

usage() {
    cat <<'EOF'
dcent-accept.sh — DCENT_OS hardware acceptance harness

USAGE:
  dcent-accept.sh <phase> <SKU> <IP> [--capstone] [--json]
  dcent-accept.sh list
  dcent-accept.sh install-hint <SKU> <IP>
  dcent-accept.sh matrix <results.jsonl>   # roll up --json results -> GO/NO-GO

PHASES:
  detect       confirm SoC + board_target + chip identity over SSH/REST
  backup       verified full-NAND / boot-region backup (must pass before install)
  firstlight   reversible /tmp deploy (dev_deploy.sh) — reboot reverts
  enum         chip-enumeration sanity vs the SKU nameplate
  shares       ACCEPT GATE: poll CGMiner Accepted counter -> PASS/FAIL   (the gate)
  soak         STABILITY GATE: poll over N min -> PASS/FAIL on shares-advancing +
               no hashrate-collapse + no thermal-excursion (catches die-spirals)
  bench        capture MHS av / elapsed / enumerated (benchmark line)
  bootlog      diagnose a captured UART/serial boot log -> stall stage (offline; arg3=logfile or -)
  ota          diagnose a witnessed OTA-capstone transcript -> stall stage (offline; arg3=transcript or -)
  all          detect -> backup -> firstlight -> enum -> shares -> bench (reversible)

FLAGS:
  --capstone   use the capstone soak thresholds (>=5 shares over 10 min) not first-light
  --json       emit a machine-readable result line for CI/dashboards
  --minutes=N  soak duration in minutes (soak phase; default 10)
  --soak-seconds=N   soak duration in seconds (overrides --minutes)
  --retention=P      min hashrate retention % for the soak gate (default 70)

SKU: one of  S9 S15 T15 S17 S17Pro S17Plus T17 T17Plus S17e T17e S19 S19Pro
     S19jPro S19kPro T19 S19XP S21 T21 S21Pro S21XP  (see `list`)

EXIT: 0 = PASS   1 = FAIL   2 = usage/setup error
EOF
}

# ---- SKU table lookup ------------------------------------------------------
# Sets SKU BOARD_TARGET ARCH CHIP CHIP_ID ENUM_EXPECT SOC BOOT_CHAIN RELEASE_STATE PACKAGE NOTE
sku_lookup() {
    want=$(printf '%s' "$1" | tr 'A-Z' 'a-z')
    line=$(grep -v '^[[:space:]]*#' "$CONF" | grep -v '^[[:space:]]*$' | while IFS='|' read -r s rest; do
        low=$(printf '%s' "$s" | tr 'A-Z' 'a-z')
        if [ "$low" = "$want" ]; then printf '%s|%s\n' "$s" "$rest"; break; fi
    done)
    if [ -z "$line" ]; then return 1; fi
    OLDIFS=$IFS; IFS='|'
    # shellcheck disable=SC2086
    set -- $line
    IFS=$OLDIFS
    SKU=$1; BOARD_TARGET=$2; ARCH=$3; CHIP=$4; CHIP_ID=$5; ENUM_EXPECT=$6
    SOC=$7; BOOT_CHAIN=$8; RELEASE_STATE=$9; PACKAGE=${10}; NOTE=${11}
    return 0
}

cmd_list() {
    printf '%-8s %-18s %-7s %-8s %-6s %-9s %-15s %s\n' \
        SKU BOARD_TARGET ARCH CHIP ENUM SOC RELEASE_STATE PACKAGE
    grep -v '^[[:space:]]*#' "$CONF" | grep -v '^[[:space:]]*$' | while IFS='|' read -r s bt ar ch cid en so bc rs pk nt; do
        printf '%-8s %-18s %-7s %-8s %-6s %-9s %-15s %s\n' "$s" "$bt" "$ar" "$ch" "$en" "$so" "$rs" "$pk"
    done
}

# ---- transport helpers -----------------------------------------------------
ssh_run() { ssh -o ConnectTimeout=6 -o StrictHostKeyChecking=no "root@$IP" "$1" 2>/dev/null; }

api_summary() {
    if command -v nc >/dev/null 2>&1; then
        printf '{"command":"summary"}' | nc -w3 "$IP" "$API_PORT" 2>/dev/null
    else
        # REST mirror fallback: /api/status carries an accepted counter shape too.
        curl -s -m 4 "http://$IP:$REST_PORT/api/system/info" 2>/dev/null
    fi
}

rest_status() { curl -s -m 4 "http://$IP:$REST_PORT/api/status" 2>/dev/null; }

# Known board_target aliases: strings the daemon treats as the SAME SKU/chip.
# The S19j Pro overlay stamps the short `am2-s19j` (serial.rs:943 exact-matches
# it, so it cannot change), while skus.conf/ use the canonical
# `am2-s19jpro-zynq`; both resolve to ZynqVariant::S19 / BM1362. Without this, a
# correctly-flashed S19j Pro (the beta SKU) false-fails detect as "WRONG SKU".
board_targets_equivalent() {
    # $1 = reported (on-miner), $2 = expected (skus.conf); order-insensitive.
    case "$1,$2" in
        am2-s19j,am2-s19jpro-zynq|am2-s19jpro-zynq,am2-s19j) return 0 ;;
        am2-s19j,am2-s19jpro|am2-s19jpro,am2-s19j) return 0 ;;
        am2-s19jpro,am2-s19jpro-zynq|am2-s19jpro-zynq,am2-s19jpro) return 0 ;;
    esac
    return 1
}

# ---- phases ----------------------------------------------------------------
cmd_detect() {
    info "detect $SKU ($CHIP / $BOARD_TARGET) on $IP"
    bt=$(ssh_run 'cat /etc/dcentos/board_target 2>/dev/null')
    say "  board_target(reported) = ${bt:-<unreachable>}   board_target(expected) = $BOARD_TARGET"
    st=$(rest_status)
    if [ -n "$st" ]; then
        say "  REST /api/status reachable ($REST_PORT)"
    else
        say "  REST /api/status not reachable (miner may be pre-deploy — run firstlight)"
    fi
    if [ -n "$bt" ] && [ "$bt" != "$BOARD_TARGET" ]; then
        if board_targets_equivalent "$bt" "$BOARD_TARGET"; then
            say "  note: reported '$bt' is a known alias of expected '$BOARD_TARGET' (same SKU/chip) — OK"
        else
            err "board_target mismatch: reported '$bt' != expected '$BOARD_TARGET' — WRONG SKU or wrong image"
            return 1
        fi
    fi
    return 0
}

cmd_backup() {
    info "backup (verified, read-only) $SKU on $IP"
    stamp=$(ssh_run 'date -u +%Y%m%dT%H%M%SZ'); stamp=${stamp:-manual}
    out="./backup-$BOARD_TARGET-$stamp"
    say "  intended backup dir: $out"
    if command -v dcent >/dev/null 2>&1; then
        say "  running: dcent backup nand $IP --out $out"
        if dcent backup nand "$IP" --out "$out"; then
            info "backup PASS (toolbox readback_verified)"; return 0
        fi
        err "toolbox backup did not verify — STOP (do not install)"; return 1
    fi
    say "  toolbox 'dcent' not on PATH — falling back to direct nanddump readback"
    mtds=$(ssh_run 'cat /proc/mtd 2>/dev/null | sed -n "s/^\(mtd[0-9]*\):.*/\1/p"')
    if [ -z "$mtds" ]; then
        err "could not read /proc/mtd over SSH — cannot verify a backup — STOP"; return 1
    fi
    say "  partitions to stream: $(printf '%s' "$mtds" | tr '\n' ' ')"
    say "  (operator: run BP-* Step 1 to stream + md5-verify each partition off-unit)"
    info "backup PLAN emitted (no writes) — complete Step 1 in the $PACKAGE package"
    return 0
}

cmd_firstlight() {
    info "first-light (reversible /tmp deploy) $SKU on $IP — reboot reverts"
    dep="$SCRIPTS_DIR/dev_deploy.sh"
    if [ ! -f "$dep" ]; then err "dev_deploy.sh missing at $dep"; return 2; fi
    say "  running: bash $dep $IP --verify"
    if bash "$dep" "$IP" --verify; then
        info "first-light deploy PASS (daemon staged to /tmp)"; return 0
    fi
    err "first-light deploy failed"; return 1
}

cmd_enum() {
    info "enumeration sanity $SKU (expect $ENUM_EXPECT, 0=capture-first)"
    body=$(rest_status)
    n=$(printf '%s' "$body" | accept_parse_enumerated)
    if [ -z "$n" ]; then
        n=$(ssh_run 'grep -aoE "enumerated [0-9]+ chips" /tmp/dcentrald.log 2>/dev/null | tail -1' | accept_parse_enumerated)
    fi
    n=${n:-0}
    v=$(accept_enum_verdict "$n" "$ENUM_EXPECT"); rc=$?
    say "  enumerated=$n expected=$ENUM_EXPECT -> $v"
    if [ "$v" = "CAPTURE" ]; then
        info "enum CAPTURE: $n chips recorded for an UNCONFIRMED SKU (feed into $PACKAGE)"; return 0
    fi
    return $rc
}

# The accept gate. Polls the CGMiner Accepted counter until >= N or T expires.
cmd_shares() {
    N=$FIRSTLIGHT_N; T=$FIRSTLIGHT_T; mode=first-light
    if [ "${CAPSTONE:-0}" = "1" ]; then N=$CAPSTONE_N; T=$CAPSTONE_T; mode=capstone; fi
    info "accept gate ($mode): need >=$N accepted shares within ${T}s on $IP:$API_PORT"
    say "  pool=$POOL  fan cap<=30 (config-enforced)  overtemp ceiling=${TEMP_CEILING}C"
    acc=0; elapsed=0; temp_seen=0
    while [ "$elapsed" -lt "$T" ]; do
        body=$(api_summary)
        got=$(printf '%s' "$body" | accept_parse_accepted)
        if accept_is_uint "$got"; then acc=$got; fi

        # Thermal safety guard: a soak that overheats must FAIL even while shares
        # accrue (cut hash before you trust the run). A confirmed reading above
        # the ceiling aborts; a missing/unreadable reading is logged once but does
        # not by itself fail the gate (the daemon's own thermal supervisor is the
        # primary protection — this is a secondary acceptance guard).
        temp=$(rest_status | accept_parse_temp_c)
        if [ -n "$temp" ]; then
            temp_seen=1
            tv=$(accept_temp_safe "$temp" "$TEMP_CEILING")
            if [ "$tv" = "HOT" ]; then
                err "ACCEPT GATE FAIL (overtemp): ${temp}C exceeded ceiling ${TEMP_CEILING}C after $acc shares in ${elapsed}s"
                [ "${JSON:-0}" = "1" ] && say "{\"result\":\"FAIL\",\"reason\":\"overtemp\",\"sku\":\"$SKU\",\"ip\":\"$IP\",\"temp_c\":\"$temp\",\"ceiling_c\":$TEMP_CEILING,\"accepted\":$acc,\"seconds\":$elapsed,\"mode\":\"$mode\"}"
                return 1
            fi
        fi

        v=$(accept_verdict "$acc" "$N")
        if [ "$v" = "PASS" ]; then
            mhs=$(printf '%s' "$body" | accept_parse_mhs_av)
            info "ACCEPT GATE PASS: $acc accepted shares in <=${elapsed}s (MHS av=${mhs:-?}, last temp=${temp:-?}C, ceiling=${TEMP_CEILING}C)"
            [ "$temp_seen" = "0" ] && say "  WARN: no temperature reading was observed during the run — thermal was NOT independently confirmed by this gate (rely on the daemon supervisor)."
            [ "${JSON:-0}" = "1" ] && say "{\"result\":\"PASS\",\"sku\":\"$SKU\",\"ip\":\"$IP\",\"accepted\":$acc,\"needed\":$N,\"seconds\":$elapsed,\"temp_c\":\"${temp:-}\",\"temp_seen\":$temp_seen,\"mode\":\"$mode\"}"
            return 0
        fi
        sleep "$POLL"
        elapsed=$((elapsed + POLL))
    done
    err "ACCEPT GATE FAIL: only $acc accepted shares in ${T}s (needed $N)"
    [ "${JSON:-0}" = "1" ] && say "{\"result\":\"FAIL\",\"sku\":\"$SKU\",\"ip\":\"$IP\",\"accepted\":$acc,\"needed\":$N,\"seconds\":$T,\"mode\":\"$mode\"}"
    return 1
}

cmd_bench() {
    info "benchmark snapshot $SKU on $IP"
    body=$(api_summary)
    mhs=$(printf '%s' "$body" | accept_parse_mhs_av)
    el=$(printf '%s' "$body" | accept_parse_elapsed)
    acc=$(printf '%s' "$body" | accept_parse_accepted)
    st=$(rest_status)
    tmp=$(printf '%s' "$st" | accept_parse_temp_c)
    enr=$(printf '%s' "$st" | accept_parse_enumerated)
    say "  MHS av=${mhs:-?}  elapsed=${el:-?}s  accepted=${acc:-?}  enumerated=${enr:-?}  temp=${tmp:-?}C"
    [ "${JSON:-0}" = "1" ] && say "{\"sku\":\"$SKU\",\"mhs_av\":\"${mhs:-}\",\"elapsed\":\"${el:-}\",\"accepted\":\"${acc:-}\",\"enumerated\":\"${enr:-}\",\"temp_c\":\"${tmp:-}\"}"
    return 0
}

cmd_install_hint() {
    info "PERSISTENT INSTALL is operator-gated — this only PRINTS the exact steps (no writes)"
    say "  1. Confirm 'backup' and 'shares' both PASSED on this exact unit first."
    say "  2. Build a fresh signed image:  bash $SCRIPTS_DIR/build_in_docker.sh   ($ARCH; honors stale-binary guard)"
    if [ "$BOOT_CHAIN" = "single-image" ]; then
        say "  3. Amlogic (no A/B): efuse preflight (BP-AMLOGIC Step 0) MUST read UNLOCKED, then BP-2:"
        say "       dcent install $IP -f output/dcentos-sysupgrade-$BOARD_TARGET.tar --accept-vnish-aml-rootfs-window --yes"
        say "     recovery = 'fw_setenv bootcmd \"run storeboot\"' (there is no A/B rollback slot)."
    else
        say "  3. Zynq A/B (writes INACTIVE slot via fw_setenv, never raw dd on weak-ECC mtd):"
        say "       dcent install $IP -f output/dcentos-sysupgrade-$BOARD_TARGET.tar --yes"
        say "     rollback = 'dcent install $IP --revert-to-stock --yes' (atomic bootslot flip)."
    fi
    say "  4. AC-cycle, then re-run:  ./dcent-accept.sh shares $SKU $IP --capstone"
    say "  Full procedure + checklist: docs/dev/2026-07-02-antminer-production-readiness/hw-procedures/$PACKAGE.md"
    return 0
}

# Release-readiness roll-up (validation dashboard). Aggregates acceptance-harness
# --json result lines (arg 2 = results file, or omit / - for stdin) into ONE
# GO/ across every SKU + phase. No hardware contact. Capstone workflow: run
# each SKU/phase with --json, collect the lines into a file, then `matrix` it.
cmd_matrix() {
    _rf=${RESULTS:-}
    if [ "$_rf" = "-" ] || [ -z "$_rf" ]; then
        v=$(accept_matrix_verdict); rc=$?
    elif [ -f "$_rf" ]; then
        v=$(accept_matrix_verdict < "$_rf"); rc=$?
    else
        err "matrix: pass a --json results file (or - / omit for stdin): dcent-accept.sh matrix <results.jsonl>"
        return 2
    fi
    info "release-readiness roll-up (validation dashboard)"
    say "  $v"
    if [ "$rc" -eq 0 ]; then
        info "RELEASE GO: every collected acceptance result PASSED"
    else
        err "RELEASE NO-GO: ${v#RELEASE_NOGO:}"
    fi
    return $rc
}

# Diagnose a captured UART / serial-console boot log (arg 3 = log file path, or '-'
# for stdin). Reports the exact stall stage so a deferred SD-first cold boot can be
# triaged without a live re-attempt (bootloader hang vs kernel panic vs userspace
# vs enum vs no-shares). No hardware contact — pure offline log analysis.
cmd_bootlog() {
    _lf=${IP:-}
    if [ -z "$_lf" ] || { [ "$_lf" != "-" ] && [ ! -f "$_lf" ]; }; then
        err "bootlog: pass a captured boot-log file (or - for stdin): dcent-accept.sh bootlog $SKU <logfile>"
        return 2
    fi
    info "boot-log stall-stage diagnosis $SKU"
    if [ "$_lf" = "-" ]; then
        v=$(accept_boot_verdict); rc=$?
    else
        v=$(accept_boot_verdict < "$_lf"); rc=$?
    fi
    say "  $v"
    if [ "$rc" -eq 0 ]; then
        info "BOOT PASS: the capture reached mining"
    else
        err "BOOT $v — the boot stalled at stage '${v#BOOT_FAIL:}'; inspect the log around that milestone"
    fi
    if [ "${JSON:-0}" = "1" ]; then
        _r=FAIL; [ "$rc" -eq 0 ] && _r=PASS
        say "{\"result\":\"$_r\",\"verdict\":\"$v\",\"sku\":\"$SKU\",\"mode\":\"bootlog\"}"
    fi
    return $rc
}

# Diagnose a witnessed OTA-capstone transcript (arg 3 = transcript file, or '-' for
# stdin). Reports the exact OTA stall stage so a failed capstone is triaged from its
# captured output instead of re-running the whole update. Honors the OTA truth
# contracts (uploaded != scheduled != flashed != mining). No hardware contact.
cmd_ota() {
    _of=${IP:-}
    if [ -z "$_of" ] || { [ "$_of" != "-" ] && [ ! -f "$_of" ]; }; then
        err "ota: pass a captured OTA-capstone transcript (or - for stdin): dcent-accept.sh ota $SKU <transcript>"
        return 2
    fi
    info "OTA-capstone stage diagnosis $SKU"
    if [ "$_of" = "-" ]; then
        v=$(accept_ota_verdict); rc=$?
    else
        v=$(accept_ota_verdict < "$_of"); rc=$?
    fi
    say "  $v"
    if [ "$rc" -eq 0 ]; then
        info "OTA PASS: capstone completed end-to-end (signed -> rebooted -> correct version -> mining)"
    else
        err "OTA $v — the capstone stalled at stage '${v#OTA_FAIL:}'; inspect the transcript around that milestone"
    fi
    if [ "${JSON:-0}" = "1" ]; then
        _r=FAIL; [ "$rc" -eq 0 ] && _r=PASS
        say "{\"result\":\"$_r\",\"verdict\":\"$v\",\"sku\":\"$SKU\",\"mode\":\"ota\"}"
    fi
    return $rc
}

# Sustained-stability soak gate. Polls the CGMiner summary + REST temp every $POLL
# for $SOAK_T seconds, then runs the pure accept_soak_verdict for a PASS/FAIL that
# catches a first-shares-then-die-spiral, thermal throttle, or stall — none of which
# the single-point 'shares' gate can see. Read-only + reversible: never raises fans,
# never writes. Reduces the operator job to "run soak, read PASS/FAIL".
cmd_soak() {
    T=${SOAK_T:-600}
    nl='
'
    info "stability soak $SKU on $IP:$API_PORT for ${T}s (interval ${POLL}s, retention ${SOAK_RETENTION:-70}%, ceiling ${TEMP_CEILING}C)"
    samples=""; elapsed=0; nsam=0
    while [ "$elapsed" -lt "$T" ]; do
        body=$(api_summary)
        acc=$(printf '%s' "$body" | accept_parse_accepted)
        accept_is_uint "$acc" || acc=0
        mhs=$(printf '%s' "$body" | accept_parse_mhs_av)
        temp=$(rest_status | accept_parse_temp_c)
        samples="${samples}${elapsed} ${acc} ${mhs:-0} ${temp:-}${nl}"
        nsam=$((nsam + 1))
        sleep "$POLL"
        elapsed=$((elapsed + POLL))
    done
    v=$(printf '%s' "$samples" | accept_soak_verdict "$TEMP_CEILING" "${SOAK_RETENTION:-70}" "${SOAK_MIN_SAMPLES:-3}"); rc=$?
    say "  samples=$nsam over ${T}s -> $v"
    if [ "$rc" -eq 0 ]; then
        info "SOAK PASS: sustained mining stayed stable over ${T}s across $nsam samples"
    else
        err "SOAK FAIL: $v"
    fi
    if [ "${JSON:-0}" = "1" ]; then
        _r=FAIL; [ "$rc" -eq 0 ] && _r=PASS
        say "{\"result\":\"$_r\",\"verdict\":\"$v\",\"sku\":\"$SKU\",\"ip\":\"$IP\",\"samples\":$nsam,\"seconds\":$T,\"mode\":\"soak\"}"
    fi
    return $rc
}

cmd_all() {
    rc=0
    cmd_detect      || rc=1
    cmd_backup      || { err "backup gate failed — aborting all (never install without a verified backup)"; return 1; }
    cmd_firstlight  || return 1
    cmd_enum        || rc=1
    cmd_shares      || rc=1
    cmd_bench       || true
    if [ "$rc" -eq 0 ]; then
        info "ACCEPTANCE ALL-PASS: $SKU on $IP (reversible /tmp). Next: ./dcent-accept.sh install-hint $SKU $IP"
    else
        err "acceptance flow had FAIL(s) for $SKU on $IP — see above (nothing persistent was written)"
    fi
    return $rc
}

# ---- arg parse -------------------------------------------------------------
PHASE=${1:-}
[ -z "$PHASE" ] && { usage; exit 2; }
case "$PHASE" in
    -h|--help|help) usage; exit 0 ;;
    list) cmd_list; exit 0 ;;
    matrix) RESULTS=${2:-}; cmd_matrix; exit $? ;;
esac

SKUARG=${2:-}
IP=${3:-}
CAPSTONE=0; JSON=0
for a in "$@"; do
    case "$a" in
        --capstone) CAPSTONE=1 ;;
        --json) JSON=1 ;;
        --soak-seconds=*) _v=${a#*=}; case "$_v" in '' | *[!0-9]*) _v=600 ;; esac; SOAK_T=$_v ;;
        --minutes=*) _v=${a#*=}; case "$_v" in '' | *[!0-9]*) _v=10 ;; esac; SOAK_T=$(( _v * 60 )) ;;
        --retention=*) _v=${a#*=}; case "$_v" in '' | *[!0-9]*) _v=70 ;; esac; SOAK_RETENTION=$_v ;;
    esac
done
[ -z "$SKUARG" ] && { err "missing SKU (see: dcent-accept.sh list)"; exit 2; }
if ! sku_lookup "$SKUARG"; then err "unknown SKU '$SKUARG' — see: dcent-accept.sh list"; exit 2; fi

case "$PHASE" in
    install-hint) IP=${IP:-<miner_ip>}; cmd_install_hint; exit $? ;;
    bootlog) cmd_bootlog; exit $? ;;
    ota) cmd_ota; exit $? ;;
esac
[ -z "$IP" ] && { err "missing miner IP"; exit 2; }

info "$SKU  [$RELEASE_STATE]  $NOTE"
case "$PHASE" in
    detect)     cmd_detect ;;
    backup)     cmd_backup ;;
    firstlight) cmd_firstlight ;;
    enum)       cmd_enum ;;
    shares)     cmd_shares ;;
    soak)       cmd_soak ;;
    bench)      cmd_bench ;;
    all)        cmd_all ;;
    *) err "unknown phase '$PHASE'"; usage; exit 2 ;;
esac
exit $?
