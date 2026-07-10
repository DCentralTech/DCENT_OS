#!/bin/bash
# DCENTos — Platform-Aware Dev Deploy Script
# D-Central Technologies, 2026
#
# Safe default behavior by board family:
#   - AM1 / S9: persistent deploy to /data/dcentrald
#   - AM2 / am2-s17: runtime-only deploy to /tmp (no NAND/rootfs mutation)
#   - Amlogic: runtime-only deploy to /tmp with --serial-mining
#
# Wave B (2026-05-19): the --passthrough CLI flag was removed. The
# [mining].passthrough = true knob in /data/dcentrald.toml is the canonical
# way to request passthrough mode; the S82 init script reads it. See
#  G-T8-1.
#
# This script is for rapid iteration without flashing NAND. It is intentionally
# conservative on experimental boards so runtime validation does not become an
# accidental install path.

set -euo pipefail

MINER_IP="${1:?Usage: $0 <miner_ip> [--skip-build] [--config FILE] [--verify] [--tail] [--rollback-on-fail] [--json] [--output FILE] [--dashboard-only]}"
shift

SKIP_BUILD=false
CONFIG_FILE=""
VERIFY=false
TAIL=false
ROLLBACK_ON_FAIL=false
JSON_OUTPUT=false
JSON_OUTPUT_FILE=""
DASHBOARD_ONLY=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --skip-build)       SKIP_BUILD=true ;;
        --config)           CONFIG_FILE="${2:?--config requires a file argument}"; shift ;;
        --verify)           VERIFY=true ;;
        --tail)             TAIL=true ;;
        --rollback-on-fail) ROLLBACK_ON_FAIL=true ;;
        --json)             JSON_OUTPUT=true ;;
        --output)           JSON_OUTPUT_FILE="${2:?--output requires a file argument}"; shift ;;
        --output=*)         JSON_OUTPUT_FILE="${1#--output=}" ;;
        # W5.1 (2026-05-07): dashboard-only deploys skip the Rust rebuild
        # entirely. The SPA is now served by server.py from
        # /usr/share/dcentos-dashboard/index.html (no longer compiled
        # into dcentrald via include_str!), so a dashboard tweak is a
        # ~30-second scp instead of a ~10-minute cargo build cycle.
        --dashboard-only)   DASHBOARD_ONLY=true ;;
        *)                  echo "Unknown option: $1" >&2; exit 1 ;;
    esac
    shift
done

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
WORKSPACE_DIR="$PROJECT_DIR/dcentrald"

SSH_TRANSPORT="openssh"
SSH_OPTS="-o StrictHostKeyChecking=no -o ConnectTimeout=10"

# Windows developer path: when password auth is needed and PuTTY tools are
# available, use plink/pscp for noninteractive deploys. This avoids raw ssh/scp
# hanging on password prompts under Git Bash / PowerShell launched sessions.
if [ -n "${DCENT_PASSWORD:-}" ] && command -v plink.exe >/dev/null 2>&1 && command -v pscp.exe >/dev/null 2>&1; then
    SSH_TRANSPORT="putty"
fi

DEPLOY_START=$(date +%s)

TARGET=""
BINARY=""
DEPLOY_MODE=""
PLATFORM_FAMILY=""
PLATFORM_DESC=""
DEPLOY_PATH=""
BACKUP_PATH=""
STAGING_PATH=""
CONFIG_REMOTE=""
LOG_PATH="/tmp/dcentrald.log"
EXPECTFILE="/tmp/dcentrald.expected_exit.pid"
VERIFY_TIMEOUT=15
HAS_PERSISTENT_SUPERVISOR=false
DEPLOY_DIR=""
DEPLOY_EXISTING_SIZE=0
TMP_FREE_BYTES=0
DEPLOY_FREE_BYTES=0
API_PORT=80

BOSMINER_PID="NONE"
BOSTOOLS_PID="NONE"
BOSER_PID="NONE"
DCENTRALD_PID="NONE"
OS_VER="NONE"
BOS_VER="NONE"
ARCH="unknown"
SOC="unknown"
MODEL=""
HWID=""
UIO_COUNT=0

log() {
    if [ "$JSON_OUTPUT" = false ]; then
        echo "$@"
    fi
}

log_step() {
    log ""
    log "=== $1 ==="
}

write_json_payload() {
    local payload="$1"
    if [ -n "$JSON_OUTPUT_FILE" ]; then
        local output_dir
        output_dir="$(dirname "$JSON_OUTPUT_FILE")"
        if ! mkdir -p "$output_dir"; then
            echo "ERROR: cannot create output directory: $output_dir" >&2
            return 1
        fi
        if ! printf '%s\n' "$payload" > "$JSON_OUTPUT_FILE"; then
            echo "ERROR: cannot write JSON output: $JSON_OUTPUT_FILE" >&2
            return 1
        fi
    fi
    if [ "$JSON_OUTPUT" = true ]; then
        printf '%s\n' "$payload"
    fi
}

json_exit() {
    local success="$1"
    local pid="${2:-null}"
    local binary_size="${3:-0}"
    local api_healthy="${4:-false}"
    local message="${5:-}"
    local deploy_end
    deploy_end=$(date +%s)
    local deploy_time=$((deploy_end - DEPLOY_START))

    local payload
    payload=$(cat <<ENDJSON
{
  "success": $success,
  "pid": $pid,
  "binary_size": $binary_size,
  "deploy_time_seconds": $deploy_time,
  "api_healthy": $api_healthy,
  "miner_ip": "$MINER_IP",
  "platform_family": "$PLATFORM_FAMILY",
  "deploy_mode": "$DEPLOY_MODE",
  "message": "$message"
}
ENDJSON
)
    write_json_payload "$payload" || exit 1

    if [ "$success" = "true" ]; then
        exit 0
    else
        exit 1
    fi
}

remote_file_exists() {
    local path="$1"
    ssh_run "[ -f '$path' ] && echo yes || echo no" 2>/dev/null
}

compute_sha256_local() {
    local path="$1"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$path" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$path" | awk '{print $1}'
    else
        return 1
    fi
}

ssh_run() {
    local remote_cmd="$1"
    if [ "$SSH_TRANSPORT" = "putty" ]; then
        if [ -n "${DCENT_HOSTKEY:-}" ]; then
            plink.exe -batch -pw "${DCENT_PASSWORD}" -hostkey "${DCENT_HOSTKEY}" "root@${MINER_IP}" "$remote_cmd"
        else
            plink.exe -batch -pw "${DCENT_PASSWORD}" "root@${MINER_IP}" "$remote_cmd"
        fi
    else
        ssh $SSH_OPTS "root@${MINER_IP}" "$remote_cmd"
    fi
}

scp_put() {
    local local_path="$1"
    local remote_path="$2"
    if [ "$SSH_TRANSPORT" = "putty" ]; then
        if [ -n "${DCENT_HOSTKEY:-}" ]; then
            pscp.exe -scp -batch -pw "${DCENT_PASSWORD}" -hostkey "${DCENT_HOSTKEY}" "$local_path" "$remote_path"
        else
            pscp.exe -scp -batch -pw "${DCENT_PASSWORD}" "$local_path" "$remote_path"
        fi
    else
        scp -O $SSH_OPTS "$local_path" "$remote_path"
    fi
}

log_step "Connecting to $MINER_IP"
if ! ssh_run "echo OK" >/dev/null 2>&1; then
    log "ERROR: Cannot SSH to root@$MINER_IP"
    json_exit false null 0 false "SSH connection failed"
fi
log "  SSH OK"
log "  SSH transport: $SSH_TRANSPORT"

# W5.1 (2026-05-07): --dashboard-only fast path. The dashboard SPA was
# decoupled from dcentrald (`include_str!` retired, build.rs deleted) and
# now ships as a static asset served by server.py from
# /usr/share/dcentos-dashboard/index.html. We can refresh just that file
# without touching the daemon — no Rust rebuild, no binary swap, no
# /etc/init.d/S82dcentrald restart, no risk of corrupting hardware.
#
# This block is platform-agnostic on purpose: every supported platform
# (zynq, am2, amlogic, beaglebone) ships server.py and the same install
# path, so the deploy is identical. Skips all the platform-detect and
# binary-deploy machinery below.
if [ "$DASHBOARD_ONLY" = true ]; then
    log_step "Dashboard-only deploy"
    LOCAL_DASHBOARD="$PROJECT_DIR/dashboard/dist/index.html"
    LOCAL_DASHBOARD_GZ="$LOCAL_DASHBOARD.gz"
    LOCAL_DASHBOARD_SHA="$LOCAL_DASHBOARD.sha256"
    if [ ! -f "$LOCAL_DASHBOARD" ]; then
        log "ERROR: dashboard not found at $LOCAL_DASHBOARD"
        log "  Run: cd DCENT_OS_Antminer/dashboard && npm run build"
        json_exit false null 0 false "dashboard/dist/index.html missing — run npm run build"
    fi
    DASHBOARD_BYTES=$(stat -c%s "$LOCAL_DASHBOARD" 2>/dev/null || stat -f%z "$LOCAL_DASHBOARD" 2>/dev/null || echo 0)
    if [ "${DASHBOARD_BYTES:-0}" -lt 100000 ]; then
        log "ERROR: dashboard appears truncated ($DASHBOARD_BYTES bytes < 100 KB floor)"
        log "  Real vite-plugin-singlefile builds are several hundred KB."
        log "  Did 'npm run build' fail? Re-run and re-check dist/index.html."
        json_exit false null "$DASHBOARD_BYTES" false "dashboard truncated"
    fi
    if [ ! -f "$LOCAL_DASHBOARD_GZ" ] || [ "$LOCAL_DASHBOARD" -nt "$LOCAL_DASHBOARD_GZ" ]; then
        log "  Generating gzip sidecar"
        if ! gzip -9 -c "$LOCAL_DASHBOARD" > "$LOCAL_DASHBOARD_GZ"; then
            log "ERROR: could not generate $LOCAL_DASHBOARD_GZ"
            json_exit false null "$DASHBOARD_BYTES" false "dashboard gzip sidecar failed"
        fi
    fi
    if [ ! -f "$LOCAL_DASHBOARD_SHA" ] || [ "$LOCAL_DASHBOARD" -nt "$LOCAL_DASHBOARD_SHA" ]; then
        log "  Generating sha256 sidecar"
        LOCAL_SHA_FOR_SIDECAR=$(compute_sha256_local "$LOCAL_DASHBOARD" 2>/dev/null || echo "")
        if [ -z "$LOCAL_SHA_FOR_SIDECAR" ]; then
            log "ERROR: could not compute sha256 for $LOCAL_DASHBOARD"
            json_exit false null "$DASHBOARD_BYTES" false "dashboard sha256 sidecar failed"
        fi
        printf '%s\n' "$LOCAL_SHA_FOR_SIDECAR" > "$LOCAL_DASHBOARD_SHA"
    fi
    log "  Local:  $LOCAL_DASHBOARD ($DASHBOARD_BYTES bytes)"

    REMOTE_DIR="/usr/share/dcentos-dashboard"
    REMOTE_INDEX="$REMOTE_DIR/index.html"
    REMOTE_GZ="$REMOTE_DIR/index.html.gz"
    REMOTE_SHA_FILE="$REMOTE_DIR/index.html.sha256"
    REMOTE_STAGE="$REMOTE_DIR/index.html.new"
    REMOTE_GZ_STAGE="$REMOTE_DIR/index.html.gz.new"
    REMOTE_SHA_STAGE="$REMOTE_DIR/index.html.sha256.new"

    # Pre-create the directory + ensure it's writable. On older overlays
    # /usr/share/dcentos-dashboard may not exist yet (pre-W5.1 image).
    ssh_run "mkdir -p '$REMOTE_DIR'" >/dev/null 2>&1 || true

    log "  Uploading to $REMOTE_STAGE"
    if ! scp_put "$LOCAL_DASHBOARD" "root@${MINER_IP}:$REMOTE_STAGE"; then
        log "ERROR: scp upload failed"
        json_exit false null "$DASHBOARD_BYTES" false "dashboard scp failed"
    fi
    if ! scp_put "$LOCAL_DASHBOARD_GZ" "root@${MINER_IP}:$REMOTE_GZ_STAGE"; then
        log "ERROR: gzip sidecar scp upload failed"
        json_exit false null "$DASHBOARD_BYTES" false "dashboard gzip scp failed"
    fi
    if ! scp_put "$LOCAL_DASHBOARD_SHA" "root@${MINER_IP}:$REMOTE_SHA_STAGE"; then
        log "ERROR: sha256 sidecar scp upload failed"
        json_exit false null "$DASHBOARD_BYTES" false "dashboard sha256 scp failed"
    fi

    # Atomic-ish swap: mv replaces in place; server.py reads on next
    # request. No daemon restart needed.
    log "  Atomic swap → $REMOTE_INDEX"
    if ! ssh_run "mv -f '$REMOTE_STAGE' '$REMOTE_INDEX' && mv -f '$REMOTE_GZ_STAGE' '$REMOTE_GZ' && mv -f '$REMOTE_SHA_STAGE' '$REMOTE_SHA_FILE' && chmod 644 '$REMOTE_INDEX' '$REMOTE_GZ' '$REMOTE_SHA_FILE'"; then
        log "ERROR: remote mv failed (check disk space + perms on /usr/share)"
        json_exit false null "$DASHBOARD_BYTES" false "dashboard swap failed"
    fi

    REMOTE_SHA=$(ssh_run "sha256sum '$REMOTE_INDEX' 2>/dev/null | awk '{print \$1}'" 2>/dev/null) || REMOTE_SHA=""
    LOCAL_SHA=$(compute_sha256_local "$LOCAL_DASHBOARD" 2>/dev/null || echo "")
    if [ -n "$LOCAL_SHA" ] && [ -n "$REMOTE_SHA" ] && [ "$LOCAL_SHA" = "$REMOTE_SHA" ]; then
        log "  SHA256 match: $LOCAL_SHA"
    elif [ -n "$LOCAL_SHA" ] && [ -n "$REMOTE_SHA" ]; then
        log "  WARNING: SHA256 mismatch (local=$LOCAL_SHA remote=$REMOTE_SHA)"
    fi

    DEPLOY_END=$(date +%s)
    DEPLOY_TIME=$((DEPLOY_END - DEPLOY_START))
    log ""
    log "=== Dashboard-only deploy complete ==="
    log "  Target:      root@$MINER_IP:$REMOTE_INDEX"
    log "  Bytes:       $DASHBOARD_BYTES"
    log "  Deploy time: ${DEPLOY_TIME}s (no Rust rebuild, no daemon restart)"
    log "  Verify:      curl -s http://$MINER_IP/api/dashboard/version"
    DASHBOARD_DEPLOY_JSON=$(cat <<ENDJSON
{
  "success": true,
  "pid": null,
  "binary_size": $DASHBOARD_BYTES,
  "deploy_time_seconds": $DEPLOY_TIME,
  "api_healthy": true,
  "miner_ip": "$MINER_IP",
  "platform_family": "any",
  "deploy_mode": "dashboard-only",
  "remote_path": "$REMOTE_INDEX",
  "sha256": "${LOCAL_SHA:-}",
  "message": "Dashboard-only deploy successful"
}
ENDJSON
)
    write_json_payload "$DASHBOARD_DEPLOY_JSON" || exit 1
    exit 0
fi

# Pre-deploy /tmp hygiene + /data probe.
# /tmp on S9 is a 64 MB tmpfs. Stale dcentrald.log* and old sysupgrade
# tarballs accumulate across deploys and can starve the staging step.
# /data is the canonical staging surface for sysupgrade tarballs (per
# ); confirm it's reachable and
# print free space for operator visibility. Failures here are
# non-fatal for runtime-only deploys (am2/amlogic) which never touch
# /data; the persistent (am1) path enforces a stricter check below.
log_step "Pre-deploy /tmp cleanup + /data probe"
PRE_DEPLOY_INFO=$(ssh_run "
    rm -f /tmp/dcentrald.log* /tmp/*.tar 2>/dev/null
    TMP_FREE_KB=\$(df -Pk /tmp 2>/dev/null | awk 'NR==2 {print \$4}')
    DATA_FREE_KB=\$(df -Pk /data 2>/dev/null | awk 'NR==2 {print \$4}')
    DATA_WRITABLE=no
    if touch /data/.dcent_stage_check 2>/dev/null; then
        rm -f /data/.dcent_stage_check 2>/dev/null
        DATA_WRITABLE=yes
    fi
    echo TMP_FREE_KB=\${TMP_FREE_KB:-0}
    echo DATA_FREE_KB=\${DATA_FREE_KB:-0}
    echo DATA_WRITABLE=\$DATA_WRITABLE
" 2>/dev/null) || true
eval "$PRE_DEPLOY_INFO" 2>/dev/null || true
TMP_FREE_KB="${TMP_FREE_KB:-0}"
DATA_FREE_KB="${DATA_FREE_KB:-0}"
DATA_WRITABLE="${DATA_WRITABLE:-no}"
log "  /tmp free after cleanup:  $((TMP_FREE_KB / 1024)) MB"
log "  /data free:               $((DATA_FREE_KB / 1024)) MB"
log "  /data writable:           $DATA_WRITABLE"

log_step "Detecting miner platform"
MINER_INFO=$(ssh_run '
    echo "BOSMINER_PID=$(pidof bosminer 2>/dev/null || echo NONE)"
    echo "BOSTOOLS_PID=$(pidof bos-tools 2>/dev/null || echo NONE)"
    echo "BOSER_PID=$(pidof boser 2>/dev/null || echo NONE)"
    echo "DCENTRALD_PID=$(pidof dcentrald 2>/dev/null || echo NONE)"
    echo "OS_VER=$(cat /etc/dcentos-version 2>/dev/null || echo NONE)"
    echo "BOS_VER=$(cat /etc/bos_version 2>/dev/null | head -1 || echo NONE)"
    echo "BOS_PLATFORM=$(cat /etc/bos_platform 2>/dev/null | head -1 || echo NONE)"
    echo "ARCH=$(uname -m 2>/dev/null || echo unknown)"
    if [ -f /sys/devices/soc0/soc_id ]; then
        echo "SOC=$(cat /sys/devices/soc0/soc_id 2>/dev/null)"
    elif grep -q zynq /proc/cpuinfo 2>/dev/null; then
        echo "SOC=zynq"
    else
        echo "SOC=unknown"
    fi
    MODEL="$(cat /config/CONF_MINER_TYPE 2>/dev/null || cat /proc/device-tree/model 2>/dev/null || echo)"
    echo "MODEL=$MODEL"
    HWID="$(cat /config/CONF_HARDWARE_ID 2>/dev/null || echo)"
    echo "HWID=$HWID"
    echo "UIO_COUNT=$(find /sys/class/uio -maxdepth 1 -name "uio*" 2>/dev/null | wc -l)"
    ' 2>/dev/null) || true

eval "$MINER_INFO" 2>/dev/null || true

SOC_LC=$(printf '%s' "$SOC" | tr '[:upper:]' '[:lower:]')
MODEL_LC=$(printf '%s' "$MODEL" | tr '[:upper:]' '[:lower:]')
HWID_LC=$(printf '%s' "$HWID" | tr '[:upper:]' '[:lower:]')
BOS_PLATFORM_LC=$(printf '%s' "${BOS_PLATFORM:-}" | tr '[:upper:]' '[:lower:]')

# Primary signal: /etc/bos_platform (most reliable). BraiinsOS ships it on
# every am1/am2 board and it cannot drift from the real hardware.
#   zynq-am1-s9       → am1
#   zynq-bm3-am2      → am2 (S17 / S19 / S19j Pro early / T17 / T19)
#   aarch64 variants  → amlogic (handled via ARCH below)
if [[ "$BOS_PLATFORM_LC" == "zynq-bm3-am2" ]]; then
    PLATFORM_FAMILY="am2"
    PLATFORM_DESC="AM2 runtime-only (detected via /etc/bos_platform)"
    TARGET="armv7-unknown-linux-musleabihf"
    DEPLOY_MODE="runtime-only"
    DEPLOY_PATH="/tmp/dcentrald_runtime"
    STAGING_PATH="/tmp/dcentrald_runtime.new"
    CONFIG_REMOTE="/tmp/dcentrald.runtime.toml"
    VERIFY_TIMEOUT=30
elif [[ "$BOS_PLATFORM_LC" == "zynq-am1-s9" ]]; then
    PLATFORM_FAMILY="am1"
    PLATFORM_DESC="AM1 persistent (detected via /etc/bos_platform)"
    TARGET="armv7-unknown-linux-musleabihf"
    DEPLOY_MODE="persistent"
    DEPLOY_PATH="/data/dcentrald"
    BACKUP_PATH="/tmp/dcentrald_backup"
    STAGING_PATH="/tmp/dcentrald_new"
    CONFIG_REMOTE="/data/dcentrald.toml"
    HAS_PERSISTENT_SUPERVISOR=true
elif [[ "$ARCH" == "aarch64" ]] || [[ "$SOC_LC" == *"amlogic"* ]] || [[ "$MODEL_LC" == *"amlogic"* ]]; then
    PLATFORM_FAMILY="amlogic"
    PLATFORM_DESC="Amlogic runtime-only"
    TARGET="aarch64-unknown-linux-musl"
    DEPLOY_MODE="runtime-only"
    DEPLOY_PATH="/tmp/dcentrald_runtime"
    STAGING_PATH="/tmp/dcentrald_runtime.new"
    CONFIG_REMOTE="/tmp/dcentrald.runtime.toml"
    VERIFY_TIMEOUT=30
elif [ "${UIO_COUNT:-0}" -ge 19 ] || [[ "$HWID_LC" == *"am2"* ]] || [[ "$MODEL_LC" == *"s17"* ]] || [[ "$MODEL_LC" == *"s19"* ]] || [[ "$MODEL_LC" == *"t17"* ]] || [[ "$MODEL_LC" == *"t19"* ]]; then
    # Fallback heuristic when /etc/bos_platform is unavailable (stock Bitmain
    # firmware, degraded boot, or non-BraiinsOS baseline).
    PLATFORM_FAMILY="am2"
    PLATFORM_DESC="AM2 runtime-only (UIO/HWID heuristic)"
    TARGET="armv7-unknown-linux-musleabihf"
    DEPLOY_MODE="runtime-only"
    DEPLOY_PATH="/tmp/dcentrald_runtime"
    STAGING_PATH="/tmp/dcentrald_runtime.new"
    CONFIG_REMOTE="/tmp/dcentrald.runtime.toml"
    VERIFY_TIMEOUT=30
else
    PLATFORM_FAMILY="am1"
    PLATFORM_DESC="AM1 persistent"
    TARGET="armv7-unknown-linux-musleabihf"
    DEPLOY_MODE="persistent"
    DEPLOY_PATH="/data/dcentrald"
    BACKUP_PATH="/tmp/dcentrald_backup"
    STAGING_PATH="/tmp/dcentrald_new"
    CONFIG_REMOTE="/data/dcentrald.toml"
    HAS_PERSISTENT_SUPERVISOR=true
fi

DEPLOY_DIR="$(dirname "$DEPLOY_PATH")"

if [ "$DEPLOY_MODE" = "runtime-only" ] && [ -z "$CONFIG_FILE" ]; then
    if [ "$(remote_file_exists "$CONFIG_REMOTE")" != "yes" ] && [ "$(remote_file_exists /data/dcentrald.toml)" != "yes" ] && [ "$(remote_file_exists /etc/dcentrald.toml)" != "yes" ]; then
        log "ERROR: $PLATFORM_FAMILY runtime-only deploy requires an explicit --config or an existing dcentrald config on the miner."
        json_exit false null 0 false "No config available for runtime-only $PLATFORM_FAMILY deploy"
    fi
fi

log "  Platform:    $PLATFORM_DESC"
log "  Arch:        $ARCH"
log "  SoC:         $SOC"
log "  bos_platform: ${BOS_PLATFORM:-NONE}"
log "  Model:       ${MODEL:-unknown}"
log "  HWID:        ${HWID:-unknown}"
log "  UIO count:   ${UIO_COUNT:-0}"
log "  BraiinsOS:   $BOS_VER"
log "  DCENTos:     $OS_VER"
log "  bosminer:    PID=$BOSMINER_PID"
log "  bos-tools:   PID=$BOSTOOLS_PID"
log "  boser:       PID=$BOSER_PID"
log "  dcentrald:   PID=$DCENTRALD_PID"

if [ "$MINER_IP" = "203.0.113.109" ] || [[ "${CONFIG_FILE:-}" == *"dcentrald_s19jpro_xil.toml"* ]]; then
    log "ERROR: XIL is a home-quiet target. Generic dev_deploy.sh uses raw bosminer stop/kill paths and is blocked for this unit."
    log "See the home-quiet handoff procedure in the project documentation."
    json_exit false null 0 false "XIL requires guarded quiet handoff; generic dev_deploy blocked"
fi

BINARY="$WORKSPACE_DIR/target/$TARGET/release/dcentrald"

if [ "$SKIP_BUILD" = false ]; then
    log_step "Building dcentrald (release, $TARGET)"
    cd "$WORKSPACE_DIR"

    # Export zig-cc wrappers for crates with build.rs (ring, secp256k1-sys,
    # reqwest, rumqttc) when the caller hasn't set their own cross-compiler.
    # rust-lld links; the CC crate needs an actual C cross-compiler. Without
    # these, cargo fails with `failed to find tool 'arm-linux-musleabihf-gcc'`.
    # See dcentrald/.cargo/config.toml for why we don't default this via
    # cargo's [env] block on Windows.
    case "$TARGET" in
        armv7-unknown-linux-musleabihf)
            : "${CC_armv7_unknown_linux_musleabihf:=$WORKSPACE_DIR/zig-cc-arm.bat}"
            : "${AR_armv7_unknown_linux_musleabihf:=$WORKSPACE_DIR/zig-ar-arm.bat}"
            export CC_armv7_unknown_linux_musleabihf AR_armv7_unknown_linux_musleabihf
            log "  CC_armv7_unknown_linux_musleabihf=$CC_armv7_unknown_linux_musleabihf"
            ;;
        aarch64-unknown-linux-musl)
            : "${CC_aarch64_unknown_linux_musl:=$WORKSPACE_DIR/zig-cc-aarch64.bat}"
            : "${AR_aarch64_unknown_linux_musl:=$WORKSPACE_DIR/zig-ar-aarch64.bat}"
            export CC_aarch64_unknown_linux_musl AR_aarch64_unknown_linux_musl
            log "  CC_aarch64_unknown_linux_musl=$CC_aarch64_unknown_linux_musl"
            ;;
    esac

    if ! cargo build --release --target "$TARGET" 2>&1 | while IFS= read -r line; do log "  $line"; done; then
        log "ERROR: Build failed"
        json_exit false null 0 false "Build failed"
    fi
    log "  Build complete."
else
    log_step "Skipping build (--skip-build)"
fi

if [ ! -f "$BINARY" ]; then
    log "ERROR: Binary not found at $BINARY"
    log "Run without --skip-build to compile first."
    json_exit false null 0 false "Binary not found at $BINARY"
fi

BINARY_SIZE=$(stat -c%s "$BINARY" 2>/dev/null || stat -f%z "$BINARY" 2>/dev/null || echo 0)
BINARY_MB=$(( BINARY_SIZE / 1024 / 1024 ))
BINARY_SHA256=$(compute_sha256_local "$BINARY") || {
    log "ERROR: Could not compute SHA256 for $BINARY"
    json_exit false null "$BINARY_SIZE" false "Could not compute local binary SHA256"
}
log "  Binary: $BINARY_SIZE bytes (${BINARY_MB} MB)"
log "  SHA256: $BINARY_SHA256"

if [ "$DEPLOY_MODE" = "persistent" ]; then
    log_step "Preflight space check"
    REMOTE_SPACE_INFO=$(ssh_run "
        TMP_FREE_KB=\$(df -k /tmp 2>/dev/null | awk 'NR==2 {print \$4}')
        DEPLOY_FREE_KB=\$(df -k '$DEPLOY_DIR' 2>/dev/null | awk 'NR==2 {print \$4}')
        EXISTING_SIZE=0
        if [ -f '$DEPLOY_PATH' ]; then
            EXISTING_SIZE=\$(wc -c < '$DEPLOY_PATH' 2>/dev/null || echo 0)
        fi
        echo TMP_FREE_BYTES=\$((\${TMP_FREE_KB:-0} * 1024))
        echo DEPLOY_FREE_BYTES=\$((\${DEPLOY_FREE_KB:-0} * 1024))
        echo DEPLOY_EXISTING_SIZE=\${EXISTING_SIZE:-0}
    " 2>/dev/null) || true

    eval "$REMOTE_SPACE_INFO" 2>/dev/null || true
    TMP_FREE_BYTES="${TMP_FREE_BYTES:-0}"
    DEPLOY_FREE_BYTES="${DEPLOY_FREE_BYTES:-0}"
    DEPLOY_EXISTING_SIZE="${DEPLOY_EXISTING_SIZE:-0}"

    TMP_REQUIRED_BYTES=$((BINARY_SIZE + DEPLOY_EXISTING_SIZE))
    DEPLOY_AVAILABLE_AFTER_REPLACE=$((DEPLOY_FREE_BYTES + DEPLOY_EXISTING_SIZE))

    log "  /tmp free:            $TMP_FREE_BYTES bytes"
    log "  $DEPLOY_DIR free:     $DEPLOY_FREE_BYTES bytes"
    log "  Existing binary size: $DEPLOY_EXISTING_SIZE bytes"

    if [ "$TMP_FREE_BYTES" -lt "$TMP_REQUIRED_BYTES" ]; then
        log "ERROR: Not enough /tmp space for staging + backup (need $TMP_REQUIRED_BYTES bytes)"
        json_exit false null "$BINARY_SIZE" false "Insufficient /tmp space for persistent deploy staging"
    fi

    if [ "$DEPLOY_AVAILABLE_AFTER_REPLACE" -lt "$BINARY_SIZE" ]; then
        log "ERROR: Not enough $DEPLOY_DIR space after replacing existing binary"
        json_exit false null "$BINARY_SIZE" false "Insufficient persistent storage space after replace"
    fi
fi

if [ -n "$CONFIG_FILE" ]; then
    log_step "Uploading config: $CONFIG_FILE"
    if [ ! -f "$CONFIG_FILE" ]; then
        log "ERROR: Config file not found: $CONFIG_FILE"
        json_exit false null "$BINARY_SIZE" false "Config file not found: $CONFIG_FILE"
    fi
    scp_put "$CONFIG_FILE" "root@$MINER_IP:$CONFIG_REMOTE"
    log "  Deployed to $CONFIG_REMOTE"
fi

log_step "Stopping daemons"

if [ "$DCENTRALD_PID" != "NONE" ]; then
    log "  Stopping dcentrald (PID $DCENTRALD_PID) — SIGTERM for graceful shutdown..."
    ssh_run "echo $DCENTRALD_PID > $EXPECTFILE 2>/dev/null; kill -TERM $DCENTRALD_PID 2>/dev/null; for i in \$(seq 1 30); do kill -0 $DCENTRALD_PID 2>/dev/null || break; sleep 1; done; if kill -0 $DCENTRALD_PID 2>/dev/null; then rm -f $EXPECTFILE; kill -9 $DCENTRALD_PID 2>/dev/null; else rm -f $EXPECTFILE; fi; true" 2>/dev/null || true
    log "  dcentrald stopped"
fi

if [ "$BOSMINER_PID" != "NONE" ]; then
    if [ "$PLATFORM_FAMILY" = "amlogic" ]; then
        log "  Amlogic warm-takeover: SIGKILL bosminer/bos-tools/boser to preserve live board state..."
        ssh_run "for pid in \$(pidof bos-tools 2>/dev/null) \$(pidof bosminer 2>/dev/null) \$(pidof boser 2>/dev/null); do kill -9 \$pid 2>/dev/null || true; done; true" 2>/dev/null || true
        sleep 1
    else
        log "  Stopping bosminer (PID $BOSMINER_PID)..."
        ssh_run "kill -TERM $BOSMINER_PID 2>/dev/null; sleep 10; kill -9 $BOSMINER_PID 2>/dev/null; true" 2>/dev/null || true
    fi
fi

if [ "$DCENTRALD_PID" = "NONE" ] && [ "$BOSMINER_PID" = "NONE" ]; then
    log "  No daemons running."
fi

log_step "Deploying binary"
if [ "$DEPLOY_MODE" = "persistent" ]; then
    if [ "$DEPLOY_EXISTING_SIZE" -gt 0 ]; then
        ssh_run "rm -f '$BACKUP_PATH' && cp '$DEPLOY_PATH' '$BACKUP_PATH'" 2>/dev/null
        log "  Backed up $DEPLOY_PATH -> $BACKUP_PATH"
    else
        ssh_run "rm -f '$BACKUP_PATH'" 2>/dev/null || true
        log "  No existing persistent binary to back up"
    fi
else
    log "  Runtime-only mode — persistent system binary will not be modified"
fi

log "  Uploading binary ($BINARY_SIZE bytes)..."
scp_put "$BINARY" "root@$MINER_IP:$STAGING_PATH"
REMOTE_STAGE_SHA256=$(ssh_run "sha256sum '$STAGING_PATH' 2>/dev/null | awk '{print \$1}'" 2>/dev/null) || REMOTE_STAGE_SHA256=""
if [ "$REMOTE_STAGE_SHA256" != "$BINARY_SHA256" ]; then
    log "ERROR: Staging hash mismatch on miner ($REMOTE_STAGE_SHA256 != $BINARY_SHA256)"
    ssh_run "rm -f '$STAGING_PATH'" 2>/dev/null || true
    json_exit false null "$BINARY_SIZE" false "Uploaded staging binary hash mismatch"
fi
log "  Staging SHA256 verified"

if [ "$DEPLOY_MODE" = "persistent" ]; then
    # Stage in /tmp, then remove the live /data binary before copying the new
    # one into place. Some DCENT_OS images keep /data tight enough that a
    # direct overwrite can fail mid-copy with ENOSPC and leave a corrupted
    # binary behind. The backup in /tmp remains available for rollback.
    ssh_run "chmod +x $STAGING_PATH && rm -f $DEPLOY_PATH && cp $STAGING_PATH $DEPLOY_PATH && chmod +x $DEPLOY_PATH && rm -f $STAGING_PATH" 2>/dev/null
else
    ssh_run "chmod +x $STAGING_PATH && mv $STAGING_PATH $DEPLOY_PATH" 2>/dev/null
fi

REMOTE_DEPLOY_SHA256=$(ssh_run "sha256sum '$DEPLOY_PATH' 2>/dev/null | awk '{print \$1}'" 2>/dev/null) || REMOTE_DEPLOY_SHA256=""
if [ "$REMOTE_DEPLOY_SHA256" != "$BINARY_SHA256" ]; then
    log "ERROR: Installed binary hash mismatch on miner ($REMOTE_DEPLOY_SHA256 != $BINARY_SHA256)"
    if [ "$DEPLOY_MODE" = "persistent" ] && [ "$DEPLOY_EXISTING_SIZE" -gt 0 ]; then
        log "  Restoring backup before restart"
        ssh_run "rm -f '$DEPLOY_PATH' && cp '$BACKUP_PATH' '$DEPLOY_PATH' && chmod +x '$DEPLOY_PATH'" 2>/dev/null || true
        if [ "$HAS_PERSISTENT_SUPERVISOR" = true ] && [ "$(remote_file_exists /etc/init.d/S82dcentrald)" = "yes" ]; then
            ssh_run "/etc/init.d/S82dcentrald start" 2>/dev/null || true
        fi
    fi
    json_exit false null "$BINARY_SIZE" false "Installed binary hash mismatch"
fi
log "  Installed SHA256 verified"

log "  Deployed to $DEPLOY_PATH"

log_step "Starting dcentrald"

START_SCRIPT=$(cat <<EOF
CONFIG=""
if [ -f "$CONFIG_REMOTE" ]; then
    CONFIG="--config $CONFIG_REMOTE"
    echo "CONFIG_USED=$CONFIG_REMOTE"
elif [ -f /data/dcentrald.toml ]; then
    CONFIG="--config /data/dcentrald.toml"
    echo "CONFIG_USED=/data/dcentrald.toml"
elif [ -f /etc/dcentrald.toml ]; then
    CONFIG="--config /etc/dcentrald.toml"
    echo "CONFIG_USED=/etc/dcentrald.toml"
else
    echo "CONFIG_USED=builtin"
fi

EXTRA_ARGS=""
if [ "$PLATFORM_FAMILY" = "amlogic" ]; then
    EXTRA_ARGS="--serial-mining"
fi
if [ "$PLATFORM_FAMILY" = "am2" ]; then
    # am2 (S19j Pro / S17 / S19 / T17 / T19) requires the hybrid serial-init +
    # FPGA work-dispatch path. main.rs auto-detects this from /etc/bos_platform,
    # but passing the flag explicitly is belt-and-suspenders in case the
    # platform file is unreadable on degraded boots.
    EXTRA_ARGS="--s19j-hybrid"
fi

if [ "$DEPLOY_MODE" = "persistent" ] && [ -x /etc/init.d/S82dcentrald ]; then
    /etc/init.d/S82dcentrald start
    sleep 1
    echo "NEW_PID=\$(pidof dcentrald 2>/dev/null | awk '{print \$1}')"
else
    nohup $DEPLOY_PATH \$CONFIG \$EXTRA_ARGS >$LOG_PATH 2>&1 &
    echo "NEW_PID=\$!"
fi
EOF
)

START_OUTPUT=$(ssh_run "$START_SCRIPT" 2>/dev/null) || true
eval "$START_OUTPUT" 2>/dev/null || true

CONFIG_USED="${CONFIG_USED:-unknown}"
NEW_PID="${NEW_PID:-null}"
if [ "$CONFIG_USED" != "builtin" ] && [ "$CONFIG_USED" != "unknown" ]; then
    REMOTE_API_PORT=$(ssh_run "awk '
        BEGIN { in_api = 0 }
        /^\[api\]/ { in_api = 1; next }
        /^\[/ { in_api = 0 }
        in_api && \$1 == \"http_port\" {
            gsub(/[^0-9]/, \"\", \$3)
            print \$3
            exit
        }
    ' '$CONFIG_USED' 2>/dev/null" 2>/dev/null) || REMOTE_API_PORT=""
    if [ -n "$REMOTE_API_PORT" ]; then
        API_PORT="$REMOTE_API_PORT"
    fi
fi

log "  Config: $CONFIG_USED"
log "  PID: $NEW_PID"
if [ "$PLATFORM_FAMILY" = "amlogic" ]; then
    log "  Mode: serial-mining runtime-only"
elif [ "$DEPLOY_MODE" = "runtime-only" ]; then
    log "  Mode: runtime-only cold-init"
fi

sleep 1

RUNNING_PID=$(ssh_run "pidof dcentrald 2>/dev/null || echo NONE" 2>/dev/null) || RUNNING_PID="NONE"
if [ "$RUNNING_PID" = "NONE" ]; then
    log "  WARNING: dcentrald exited immediately!"
    log "  Last 20 lines of log:"
    if [ "$JSON_OUTPUT" = false ]; then
        ssh_run "tail -20 $LOG_PATH 2>/dev/null" 2>/dev/null || true
    fi

    if [ "$ROLLBACK_ON_FAIL" = true ]; then
        log ""
        log "=== Rolling back ==="
        if [ "$DEPLOY_MODE" = "persistent" ]; then
            ssh_run "[ -f $BACKUP_PATH ] && rm -f $DEPLOY_PATH && cp $BACKUP_PATH $DEPLOY_PATH && chmod +x $DEPLOY_PATH" 2>/dev/null || true
            ssh_run "if [ -x /etc/init.d/S82dcentrald ]; then /etc/init.d/S82dcentrald start; else nohup $DEPLOY_PATH --config $CONFIG_REMOTE >$LOG_PATH 2>&1 & fi" 2>/dev/null || true
            sleep 1
            ROLLBACK_PID=$(ssh_run "pidof dcentrald 2>/dev/null || echo NONE" 2>/dev/null) || ROLLBACK_PID="NONE"
            log "  Rollback PID: $ROLLBACK_PID"
            json_exit false null "$BINARY_SIZE" false "dcentrald exited immediately, rolled back to backup (PID=$ROLLBACK_PID)"
        elif [ "$OS_VER" != "NONE" ] && [ "$HAS_PERSISTENT_SUPERVISOR" = true ]; then
            ssh_run "[ -x /etc/init.d/S82dcentrald ] && /etc/init.d/S82dcentrald start || true" 2>/dev/null || true
        fi
    fi

    json_exit false null "$BINARY_SIZE" false "dcentrald exited immediately"
fi

NEW_PID="$RUNNING_PID"
API_HEALTHY=false

if [ "$VERIFY" = true ]; then
    log_step "Verifying API health (polling for ${VERIFY_TIMEOUT}s)"
    VERIFY_DEADLINE=$(($(date +%s) + VERIFY_TIMEOUT))

    while [ "$(date +%s)" -lt "$VERIFY_DEADLINE" ]; do
        HTTP_CODE=$(ssh_run "wget -q -O /dev/null -S http://127.0.0.1:$API_PORT/api/status 2>&1 | grep 'HTTP/' | tail -1 | awk '{print \$2}'" 2>/dev/null) || HTTP_CODE=""

        if [ "$HTTP_CODE" = "200" ]; then
            API_HEALTHY=true
            log "  API healthy (HTTP 200)"
            break
        fi

        log "  Waiting... (HTTP=$HTTP_CODE)"
        sleep 2
    done

    if [ "$API_HEALTHY" = false ]; then
        log "  WARNING: API did not respond with 200 within ${VERIFY_TIMEOUT}s"

        if [ "$ROLLBACK_ON_FAIL" = true ] && [ "$DEPLOY_MODE" = "persistent" ]; then
            log ""
            log "=== Rolling back (health check failed) ==="
            ssh_run "echo $NEW_PID > $EXPECTFILE 2>/dev/null; kill -TERM $NEW_PID 2>/dev/null; for i in \$(seq 1 30); do kill -0 $NEW_PID 2>/dev/null || break; sleep 1; done; if kill -0 $NEW_PID 2>/dev/null; then rm -f $EXPECTFILE; kill -9 $NEW_PID 2>/dev/null; else rm -f $EXPECTFILE; fi; true" 2>/dev/null || true
            ssh_run "[ -f $BACKUP_PATH ] && rm -f $DEPLOY_PATH && cp $BACKUP_PATH $DEPLOY_PATH && chmod +x $DEPLOY_PATH" 2>/dev/null || true
            ssh_run "if [ -x /etc/init.d/S82dcentrald ]; then /etc/init.d/S82dcentrald start; else nohup $DEPLOY_PATH --config $CONFIG_REMOTE >$LOG_PATH 2>&1 & fi" 2>/dev/null || true
            sleep 1
            ROLLBACK_PID=$(ssh_run "pidof dcentrald 2>/dev/null || echo NONE" 2>/dev/null) || ROLLBACK_PID="NONE"
            log "  Restored backup, PID=$ROLLBACK_PID"
            json_exit false "$ROLLBACK_PID" "$BINARY_SIZE" false "API health check failed, rolled back to backup"
        fi
    fi
fi

DEPLOY_END=$(date +%s)
DEPLOY_TIME=$((DEPLOY_END - DEPLOY_START))

log_step "Deploy Complete"
log "  Target:      root@$MINER_IP"
log "  Platform:    $PLATFORM_DESC"
log "  Mode:        $DEPLOY_MODE"
log "  Binary:      $DEPLOY_PATH ($BINARY_SIZE bytes)"
log "  Config:      $CONFIG_USED"
log "  PID:         $NEW_PID"
log "  API health:  $API_HEALTHY"
log "  Deploy time: ${DEPLOY_TIME}s"
log ""
log "  Log:       ssh root@$MINER_IP 'tail -f $LOG_PATH'"
log "  REST API:  http://$MINER_IP:$API_PORT/"
log "  CGMiner:   http://$MINER_IP:4028/"
log ""

DEPLOY_JSON=$(cat <<ENDJSON
{
  "success": true,
  "pid": $NEW_PID,
  "binary_size": $BINARY_SIZE,
  "deploy_time_seconds": $DEPLOY_TIME,
  "api_healthy": $API_HEALTHY,
  "miner_ip": "$MINER_IP",
  "platform_family": "$PLATFORM_FAMILY",
  "deploy_mode": "$DEPLOY_MODE",
  "config": "$CONFIG_USED",
  "message": "Deploy successful"
}
ENDJSON
)
write_json_payload "$DEPLOY_JSON" || exit 1

if [ "$TAIL" = true ]; then
    log "=== Tailing log (Ctrl+C to stop) ==="
    ssh_run "tail -f $LOG_PATH"
fi
