#!/bin/sh
# DCENTos post-build script
# Ensures essential directories exist in the target rootfs
set -e
TARGET_DIR=$1

mkdir -p "${TARGET_DIR}/etc"
mkdir -p "${TARGET_DIR}/proc"
mkdir -p "${TARGET_DIR}/sys"
mkdir -p "${TARGET_DIR}/tmp"
mkdir -p "${TARGET_DIR}/root"
mkdir -p "${TARGET_DIR}/root/tools"
mkdir -p "${TARGET_DIR}/root/.ssh"
mkdir -p "${TARGET_DIR}/lib/modules"
mkdir -p "${TARGET_DIR}/usr/local/bin"
mkdir -p "${TARGET_DIR}/var/log"
mkdir -p "${TARGET_DIR}/data"
mkdir -p "${TARGET_DIR}/etc/dcentos"

# Install the canonical host/target archive-admission policy.  Keeping one
# source prevents the release verifier and on-device sysupgrade paths from
# drifting on tar member safety.
ARCHIVE_ADMISSION_SRC="${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/lib/sysupgrade_archive_admission.sh"
ARCHIVE_ADMISSION_DST="${TARGET_DIR}/usr/libexec/dcentos/sysupgrade-archive-admission.sh"
[ -r "$ARCHIVE_ADMISSION_SRC" ] || {
    echo "DCENTos post-build (zynq): ERROR: archive-admission helper not found at $ARCHIVE_ADMISSION_SRC" >&2
    exit 1
}
mkdir -p "$(dirname "$ARCHIVE_ADMISSION_DST")"
cp "$ARCHIVE_ADMISSION_SRC" "$ARCHIVE_ADMISSION_DST"
chmod 0644 "$ARCHIVE_ADMISSION_DST"

# Install the semantic JSON/version authority used by every Zynq consumer.
# Python 3 is a required package in dcentos-common.fragment; missing either
# side is a build-time error rather than a runtime downgrade in policy.
MANIFEST_JSON_SRC="${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/lib/sysupgrade_manifest_json.py"
MANIFEST_JSON_DST="${TARGET_DIR}/usr/libexec/dcentos/sysupgrade-manifest-json.py"
[ -r "$MANIFEST_JSON_SRC" ] || {
    echo "DCENTos post-build: ERROR: manifest JSON helper not found at $MANIFEST_JSON_SRC" >&2
    exit 1
}
cp "$MANIFEST_JSON_SRC" "$MANIFEST_JSON_DST"
chmod 0755 "$MANIFEST_JSON_DST"

# Install the evidence-backed payload geometry used by producers, host gates,
# and target consumers. The host source is canonical; the target filename is
# an installed ABI, not a second implementation.
ZYNQ_GEOMETRY_SRC="${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/lib/sysupgrade_zynq_geometry.sh"
ZYNQ_GEOMETRY_DST="${TARGET_DIR}/usr/libexec/dcentos/sysupgrade-zynq-geometry.sh"
[ -r "$ZYNQ_GEOMETRY_SRC" ] || {
    echo "DCENTos post-build (zynq): ERROR: Zynq geometry helper not found at $ZYNQ_GEOMETRY_SRC" >&2
    exit 1
}
cp "$ZYNQ_GEOMETRY_SRC" "$ZYNQ_GEOMETRY_DST"
chmod 0644 "$ZYNQ_GEOMETRY_DST"

# W1.5 (2026-05-07): pre-create /data/dcent/ with tight perms so the auth
# file (/data/dcent/auth.json) lands in a 0700 root:root directory on first
# boot. dcentrald-api::auth::save_auth() will also tighten on every write,
# and dcentrald-api::auth::verify_auth_file_perms() auto-corrects on
# startup, but staging perms here means the file is never readable by
# non-root processes, even briefly.
mkdir -p "${TARGET_DIR}/data/dcent"
chmod 0700 "${TARGET_DIR}/data/dcent"
chown 0:0 "${TARGET_DIR}/data/dcent" 2>/dev/null || true

# Make init scripts executable
chmod +x "${TARGET_DIR}"/etc/init.d/* 2>/dev/null || true

# Make early-init and persistent storage scripts executable
chmod +x "${TARGET_DIR}"/etc/dcentos-early-init.sh 2>/dev/null || true

# Make tools executable
chmod +x "${TARGET_DIR}"/root/tools/*.py 2>/dev/null || true
chmod +x "${TARGET_DIR}"/root/tools/*.sh 2>/dev/null || true
chmod +x "${TARGET_DIR}"/usr/bin/dcent-shell 2>/dev/null || true
chmod +x "${TARGET_DIR}"/usr/sbin/sysupgrade 2>/dev/null || true
# W1.1 default-credential lockdown: SSH gate helper must be mode 0755.
# Called by dcentrald + first-boot wizard to flip the dropbear gate.
chmod 0755 "${TARGET_DIR}"/usr/sbin/dcent-enable-ssh 2>/dev/null || true

#  W10-A (R3'-H1) +  W12-B: install per-platform
# revert scripts into the rootfs overlay from the canonical
# source-of-truth in DCENT_OS_Antminer/scripts/. Single copy each in
# git, deployed to /usr/sbin/revert_to_stock_<plat>.sh on the miner.
# These are the paths the daemon's restore_to_stock route probes at
# runtime via PROFILE_TABLE.<sig>.revert_script.
#
# The S9 canonical script is currently a fail-closed containment boundary.
# Keep the legacy filename as a symlink so an old entry point cannot drift into
# a second implementation or retain stale destructive behavior.
REVERT_S9_SRC="${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/revert_to_stock_s9.sh"
REVERT_S17_SRC="${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/revert_to_stock_s17.sh"
REVERT_S19_AM2_SRC="${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/revert_to_stock_s19_am2.sh"
rm -f "${TARGET_DIR}/usr/sbin/revert_to_stock_s9.sh" \
    "${TARGET_DIR}/usr/sbin/revert_to_stock.sh"
if [ -f "$REVERT_S9_SRC" ]; then
    cp "$REVERT_S9_SRC" "${TARGET_DIR}/usr/sbin/revert_to_stock_s9.sh"
    chmod +x "${TARGET_DIR}/usr/sbin/revert_to_stock_s9.sh" 2>/dev/null || true
    ln -s revert_to_stock_s9.sh "${TARGET_DIR}/usr/sbin/revert_to_stock.sh"
    echo "DCENTos post-build (zynq): installed revert_to_stock_s9.sh from scripts/"
else
    echo "DCENTos post-build (zynq): ERROR: S9 restore containment script not found at $REVERT_S9_SRC" >&2
    exit 1
fi
#  W16: S17 am2-s17 (BM1397+) revert script. The same zynq
# rootfs overlay ships both am1-s9 and am2-s17 because the runtime
# Zynq variant detection (chain1-* vs chain6-* UIO names) picks the
# right code path; carrying both scripts in /usr/sbin keeps the
# `dcentrald-api::routes::restore_to_stock::PROFILE_TABLE` revert_script
# probe satisfied on either control board.
if [ -f "$REVERT_S17_SRC" ]; then
    cp "$REVERT_S17_SRC" "${TARGET_DIR}/usr/sbin/revert_to_stock_s17.sh"
    chmod +x "${TARGET_DIR}/usr/sbin/revert_to_stock_s17.sh" 2>/dev/null || true
    echo "DCENTos post-build (zynq): installed revert_to_stock_s17.sh from scripts/"
else
    echo "DCENTos post-build (zynq): WARNING: revert_to_stock_s17.sh not found at $REVERT_S17_SRC" >&2
fi
#  W19-followup: S19 Pro / S19j Pro Zynq am2 (XC7Z020 BM1398 +
# BM1362) revert script. The same zynq rootfs overlay ships am1-s9 +
# am2-s17 + am2-s19; the runtime Zynq variant detection (DT model
# antminer-s19/am2-s19/xc7z020 vs antminer-s17/am2-s17 vs am1-s9 chain
# UIO names) picks the right code path. Carrying all three scripts in
# /usr/sbin keeps the
# `dcentrald-api::routes::restore_to_stock::PROFILE_TABLE` revert_script
# probe satisfied on every Zynq am1/am2 control board.
if [ -f "$REVERT_S19_AM2_SRC" ]; then
    cp "$REVERT_S19_AM2_SRC" "${TARGET_DIR}/usr/sbin/revert_to_stock_s19_am2.sh"
    chmod +x "${TARGET_DIR}/usr/sbin/revert_to_stock_s19_am2.sh" 2>/dev/null || true
    echo "DCENTos post-build (zynq): installed revert_to_stock_s19_am2.sh from scripts/"
else
    echo "DCENTos post-build (zynq): WARNING: revert_to_stock_s19_am2.sh not found at $REVERT_S19_AM2_SRC" >&2
fi
#  W10-G: ship the stock-Bitmain manifest into the target rootfs.
# `restore_to_stock::lookup_in_stock_manifest()` probes
# /etc/dcentos/stock-bitmain-manifest.json first, then falls back to
# the in-tree path. Mirroring W10-A's pattern keeps the source-of-truth
# in knowledge-base/ and the runtime copy in the overlay.
MANIFEST_SRC="${BR2_EXTERNAL_DCENTOS_PATH}/../../../knowledge-base/firmware-archive/stock-bitmain-manifest.json"
if [ -f "$MANIFEST_SRC" ]; then
    cp "$MANIFEST_SRC" "${TARGET_DIR}/etc/dcentos/stock-bitmain-manifest.json"
    chmod 644 "${TARGET_DIR}/etc/dcentos/stock-bitmain-manifest.json" 2>/dev/null || true
    echo "DCENTos post-build: installed stock-bitmain-manifest.json"
else
    echo "DCENTos post-build: WARNING: stock-bitmain-manifest.json not found at $MANIFEST_SRC" >&2
fi

# Make web server and MCP server executable
chmod +x "${TARGET_DIR}"/root/web/server.py 2>/dev/null || true
chmod +x "${TARGET_DIR}"/root/web/mcp_server.py 2>/dev/null || true

# W5.1 (2026-05-07): install the dashboard SPA at the canonical location
# served by server.py. Source-of-truth is dashboard/dist/index.html from
# `cd DCENT_OS_Antminer/dashboard && npm run build`. The previous flow
# embedded the file into the dcentrald binary via include_str! and a
# matching build.rs size gate; both were deleted in W5.1 because a full
# Rust rebuild + sysupgrade for every dashboard tweak was unworkable.
#
# 100 KB floor enforces "vite-plugin-singlefile actually inlined assets"
# (production builds land at several hundred KB). Below that almost
# certainly means an empty placeholder or a truncated build; refuse to
# ship rather than silently shipping a stale dashboard via sysupgrade.
DASHBOARD_SRC="${BR2_EXTERNAL_DCENTOS_PATH}/../dashboard/dist/index.html"
DASHBOARD_GZ_SRC="${DASHBOARD_SRC}.gz"
DASHBOARD_SHA_SRC="${DASHBOARD_SRC}.sha256"
DASHBOARD_DEST_DIR="${TARGET_DIR}/usr/share/dcentos-dashboard"
if [ -f "$DASHBOARD_SRC" ]; then
    mkdir -p "$DASHBOARD_DEST_DIR"
    cp "$DASHBOARD_SRC" "$DASHBOARD_DEST_DIR/index.html"
    chmod 644 "$DASHBOARD_DEST_DIR/index.html"
    if [ ! -f "$DASHBOARD_GZ_SRC" ] || [ "$DASHBOARD_SRC" -nt "$DASHBOARD_GZ_SRC" ]; then
        gzip -9 -c "$DASHBOARD_SRC" > "$DASHBOARD_DEST_DIR/index.html.gz"
    else
        cp "$DASHBOARD_GZ_SRC" "$DASHBOARD_DEST_DIR/index.html.gz"
    fi
    if [ ! -f "$DASHBOARD_SHA_SRC" ] || [ "$DASHBOARD_SRC" -nt "$DASHBOARD_SHA_SRC" ]; then
        sha256sum "$DASHBOARD_SRC" | awk '{print $1}' > "$DASHBOARD_DEST_DIR/index.html.sha256"
    else
        cp "$DASHBOARD_SHA_SRC" "$DASHBOARD_DEST_DIR/index.html.sha256"
    fi
    chmod 644 "$DASHBOARD_DEST_DIR/index.html.gz" "$DASHBOARD_DEST_DIR/index.html.sha256"
    DASHBOARD_SIZE=$(stat -c%s "$DASHBOARD_SRC" 2>/dev/null || stat -f%z "$DASHBOARD_SRC")
    echo "DCENTos post-build (zynq): installed dashboard SPA ($DASHBOARD_SIZE bytes) at /usr/share/dcentos-dashboard/index.html"
    if [ "$DASHBOARD_SIZE" -lt 100000 ]; then
        echo "DCENTos post-build (zynq): ERROR: dashboard appears truncated ($DASHBOARD_SIZE bytes < 100 KB floor)" >&2
        echo "  Run: cd DCENT_OS_Antminer/dashboard && npm run build" >&2
        exit 1
    fi
else
    echo "DCENTos post-build (zynq): ERROR: dashboard not found at $DASHBOARD_SRC" >&2
    echo "  Run: cd DCENT_OS_Antminer/dashboard && npm run build" >&2
    exit 1
fi

# Install dcentos-init as /sbin/init if present
# This replaces BusyBox init (or procd) with our custom Rust PID 1 process.
# Build with: cargo build --release --target armv7-unknown-linux-musleabihf -p dcentos-init
# Then copy to: $BR2_EXTERNAL_DCENTOS_PATH/../dcentrald/target/armv7-unknown-linux-musleabihf/release/dcentos-init
DCENTOS_INIT="${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/target/armv7-unknown-linux-musleabihf/release/dcentos-init"
if [ -f "$DCENTOS_INIT" ]; then
    # Remove BusyBox init symlink if it exists
    rm -f "${TARGET_DIR}/sbin/init" 2>/dev/null || true
    cp "$DCENTOS_INIT" "${TARGET_DIR}/sbin/init"
    chmod 755 "${TARGET_DIR}/sbin/init"
    echo "DCENTos post-build: installed dcentos-init as /sbin/init"
    mkdir -p "${TARGET_DIR}/etc/dcentos"
    sha256sum "$DCENTOS_INIT" | cut -d' ' -f1 > "${TARGET_DIR}/etc/dcentos/dcentos-init.sha256"
    echo "DCENTos post-build: dcentos-init sha256 $(cat "${TARGET_DIR}/etc/dcentos/dcentos-init.sha256")"
else
    echo "DCENTos post-build: ERROR: dcentos-init not found at $DCENTOS_INIT" >&2
    echo "  Refusing to ship a release image with an unstaged PID 1. Build dcentos-init first." >&2
    exit 1
fi

# Install dcentrald mining daemon from the cross-compiled target directory.
# Runs AFTER Buildroot's rootfs-overlay copy, so the fresh build wins over
# any stale binary checked into the overlay.
DCENTRALD_BIN="${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/target/armv7-unknown-linux-musleabihf/release/dcentrald"
if [ -f "$DCENTRALD_BIN" ]; then
    STAGED_BIN="${TARGET_DIR}/usr/local/bin/dcentrald"
    cp "$DCENTRALD_BIN" "$STAGED_BIN"
    chmod 755 "$STAGED_BIN"
    DCENTRALD_SIZE=$(stat -c%s "$DCENTRALD_BIN" 2>/dev/null || stat -f%z "$DCENTRALD_BIN")
    echo "DCENTos post-build: installed dcentrald ($DCENTRALD_SIZE bytes)"
    # Bug #7 (2026-05-05): refuse to ship a too-small dcentrald. Past stale
    # caches surfaced as a 6 MB pre-Phase-B binary on the overlay path. The
    # current release with panic=abort + LTO + strip lands ~16 MB; anything
    # under 12 MB is almost certainly an old or placeholder build. The build
    # fails loudly so we don't silently flash old code via sysupgrade.
    if [ "$DCENTRALD_SIZE" -lt 12000000 ]; then
        echo "DCENTos post-build: ERROR: dcentrald appears stale ($DCENTRALD_SIZE bytes, expected >= 12 MB)" >&2
        echo "  Run: docker volume rm dcentos-build-work" >&2
        echo "  Then: cargo build --release --target armv7-unknown-linux-musleabihf -p dcentrald" >&2
        exit 1
    fi
    . "${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/lib/dcentrald_version_gate.sh"
    dcent_require_dcentrald_version_match \
        "$TARGET_DIR" \
        "$STAGED_BIN" \
        "DCENTos post-build (zynq)" \
        "${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/Cargo.toml"
else
    echo "DCENTos post-build: ERROR: dcentrald not found at $DCENTRALD_BIN" >&2
    echo "  Refusing to ship a stale overlay daemon. Build dcentrald first." >&2
    exit 1
fi

# Embed board identity for update/install compatibility checks.
echo "am1" > "${TARGET_DIR}/etc/dcentos/board_family"
echo "am1-s9" > "${TARGET_DIR}/etc/dcentos/board_target"

# Embed the trusted release public key when provided.
if [ -n "${DCENT_RELEASE_PUBKEY_FILE:-}" ]; then
    if [ ! -f "${DCENT_RELEASE_PUBKEY_FILE}" ]; then
        echo "DCENTos post-build: ERROR: release public key not found at ${DCENT_RELEASE_PUBKEY_FILE}" >&2
        exit 1
    fi
    cp "${DCENT_RELEASE_PUBKEY_FILE}" "${TARGET_DIR}/etc/dcentos/release_ed25519.pub"
    chmod 644 "${TARGET_DIR}/etc/dcentos/release_ed25519.pub"
    echo "DCENTos post-build: embedded release public key"
elif [ "${DCENT_REQUIRE_RELEASE_KEY:-0}" = "1" ]; then
    echo "DCENTos post-build: ERROR: DCENT_REQUIRE_RELEASE_KEY=1 but DCENT_RELEASE_PUBKEY_FILE is not set" >&2
    exit 1
else
    echo "DCENTos post-build: WARNING: no release public key embedded (lab-only image)"
fi

# Production-readiness matrix §7 #1 (public-image trust boundary): stamp the
# release-image marker + tighten first-boot SSH posture when DCENT_RELEASE_IMAGE=1.
# NO-OP on DEV/LAB builds (root:dcentral + passwordless opt-out preserved).
. "${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/lib/release_image_provision.sh"
dcent_provision_release_image "$TARGET_DIR" "zynq"

strip_sysupgrade_offline_harness() {
    _sysupgrade="${TARGET_DIR}/usr/sbin/sysupgrade"
    [ -f "$_sysupgrade" ] || return 0
    _tmp="${_sysupgrade}.strip.$$"
    awk '
        !skip && $0 ~ /^if[[:space:]]/ && index($0, "DCENT_SYSUPGRADE_OFFLINE_HARNESS") {
            skip = 1
            depth = 1
            next
        }
        skip {
            if ($0 ~ /^[[:space:]]*if[[:space:]].*;[[:space:]]*then$/) {
                depth++
            }
            if ($0 ~ /^[[:space:]]*fi[[:space:]]*$/) {
                depth--
                if (depth == 0) {
                    skip = 0
                }
            }
            next
        }
        { print }
    ' "$_sysupgrade" > "$_tmp"
    mv "$_tmp" "$_sysupgrade"
    chmod 0755 "$_sysupgrade"
    if grep -Eq 'DCENT_SYSUPGRADE_OFFLINE_HARNESS|DCENT_SYSUPGRADE_OFFLINE_MARKER|dcent-sysupgrade-offline-nandsim-harness-v1|offline harness overrides are disabled' "$_sysupgrade"; then
        echo "DCENTos post-build: ERROR: deployed sysupgrade still contains offline harness hooks" >&2
        exit 1
    fi
    echo "DCENTos post-build: stripped offline harness hooks from deployed sysupgrade"
}

strip_sysupgrade_offline_harness

# -----------------------------------------------------------------------------
# Defensive CRLF strip on shebang scripts (S9-vs-S19jPro PARITY — A2e-1).
# A Windows core.autocrlf=true checkout can leave CRLF in rootfs-overlay scripts
# that no .gitattributes rule covers; a CRLF shebang (#!/bin/sh\r) makes the
# script non-executable on the device ("not found" at exec) — the exact prior
# live incident class. The am2-s19jpro post-build.sh already carries this sweep;
# the S9 (zynq) image was unprotected (only sysupgrade/revert_to_stock* are
# .gitattributes eol=lf-pinned, NOT usr/bin/dcent-shell, usr/sbin/autologin,
# usr/sbin/dcent-enable-ssh, etc.). This runs AFTER the overlay copy + all
# installs, normalizing every #!-script in the staged rootfs to LF. Binaries
# (no #! magic) are skipped, so it is safe. Belt-and-suspenders with the
# broadened /br2_external_dcentos/**/usr/{sbin,bin} eol=lf .gitattributes rules.
# -----------------------------------------------------------------------------
CRLF_FIXED=0
for d in usr/sbin usr/local/bin usr/bin etc/init.d sbin bin; do
    [ -d "${TARGET_DIR}/${d}" ] || continue
    for f in "${TARGET_DIR}/${d}"/*; do
        [ -f "$f" ] || continue
        # Only touch text scripts that begin with a shebang.
        if [ "$(head -c2 "$f" 2>/dev/null)" = "#!" ] && grep -qU "$(printf '\r')" "$f" 2>/dev/null; then
            sed -i 's/\r$//' "$f"
            CRLF_FIXED=$((CRLF_FIXED + 1))
        fi
    done
done
echo "DCENTos post-build (zynq): CRLF-normalized $CRLF_FIXED shebang script(s) in staged rootfs"

echo "DCENTos post-build: directories and permissions set."
