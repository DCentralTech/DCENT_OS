#!/bin/sh
#
# Static regression checks for sysupgrade packaging/verifier safety.
# This script is offline only: no SSH, uploads, flashing, or live target access.

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

require_pattern() {
    file=$1
    pattern=$2
    label=$3

    if grep -F -- "$pattern" "$file" >/dev/null 2>&1; then
        pass "$label"
    else
        fail "$label: missing pattern '$pattern' in $file"
    fi
}

reject_pattern() {
    file=$1
    pattern=$2
    label=$3

    if grep -F -- "$pattern" "$file" >/dev/null 2>&1; then
        fail "$label: forbidden pattern '$pattern' in $file"
    else
        pass "$label"
    fi
}

# Like reject_pattern, but only inspects EXECUTABLE shell lines: comment
# lines (leading-whitespace then '#') and `echo`/`printf` recovery-hint
# lines are stripped first. This lets a sysupgrade script keep a documented
# recovery hint that NAMES the banned raw-env path in prose without the
# guard false-positiving on the comment/echo text — while still catching a
# genuine `flash_erase /dev/mtd4` / `nandwrite -p /dev/mtd4` invocation.
reject_executable_pattern() {
    file=$1
    pattern=$2
    label=$3

    if grep -Ev '^[[:space:]]*#' "$file" \
        | grep -Ev '^[[:space:]]*(echo|printf)[[:space:]]' \
        | grep -F -- "$pattern" >/dev/null 2>&1; then
        fail "$label: forbidden EXECUTABLE pattern '$pattern' in $file"
    else
        pass "$label"
    fi
}

make_test_sysupgrade_package() {
    pkgdir=$1
    status=$2

    mkdir -p "$pkgdir"
    printf '\047\005\031\126kernel\n' > "$pkgdir/kernel"
    printf '\047\005\031\126root\n' > "$pkgdir/root"
    printf 'board=am3-s19k\n' > "$pkgdir/METADATA"

    kernel_size=$(wc -c < "$pkgdir/kernel" | tr -d ' ')
    root_size=$(wc -c < "$pkgdir/root" | tr -d ' ')
    metadata_size=$(wc -c < "$pkgdir/METADATA" | tr -d ' ')
    kernel_sha=$(sha256sum "$pkgdir/kernel" | awk '{ print $1 }')
    root_sha=$(sha256sum "$pkgdir/root" | awk '{ print $1 }')
    metadata_sha=$(sha256sum "$pkgdir/METADATA" | awk '{ print $1 }')

    cat > "$pkgdir/MANIFEST.json" <<EOF
{
  "product": "DCENT_OS",
  "package_type": "sysupgrade",
  "board": "am3-s19k",
  "board_target": "am3-s19k",
  "version": "test",
  "status": "$status",
  "payloads": {
    "kernel": { "path": "sysupgrade-am3-s19k/kernel", "size": $kernel_size, "sha256": "$kernel_sha" },
    "rootfs": { "path": "sysupgrade-am3-s19k/root", "size": $root_size, "sha256": "$root_sha" },
    "metadata": { "path": "sysupgrade-am3-s19k/METADATA", "size": $metadata_size, "sha256": "$metadata_sha" }
  }
}
EOF
    (cd "$pkgdir" && sha256sum kernel root METADATA > SHA256SUMS)
}

make_test_oversized_zynq_package() {
    pkgdir=$1
    board=$2
    root_size=$3

    mkdir -p "$pkgdir"
    printf '\320\015\376\355kernel\n' > "$pkgdir/kernel"
    printf 'hsqs' > "$pkgdir/root"
    if command -v truncate >/dev/null 2>&1; then
        truncate -s "$root_size" "$pkgdir/root"
    else
        dd if=/dev/zero of="$pkgdir/root" bs=1 count=1 seek=$((root_size - 1)) conv=notrunc >/dev/null 2>&1
    fi
    printf 'board=%s\n' "$board" > "$pkgdir/METADATA"

    kernel_size=$(wc -c < "$pkgdir/kernel" | tr -d ' ')
    actual_root_size=$(wc -c < "$pkgdir/root" | tr -d ' ')
    metadata_size=$(wc -c < "$pkgdir/METADATA" | tr -d ' ')
    kernel_sha=$(sha256sum "$pkgdir/kernel" | awk '{ print $1 }')
    root_sha=$(sha256sum "$pkgdir/root" | awk '{ print $1 }')
    metadata_sha=$(sha256sum "$pkgdir/METADATA" | awk '{ print $1 }')

    cat > "$pkgdir/MANIFEST.json" <<EOF
{
  "product": "DCENT_OS",
  "package_type": "sysupgrade",
  "board": "$board",
  "board_target": "$board",
  "version": "test",
  "status": "lab_unsigned",
  "payloads": {
    "kernel": { "path": "sysupgrade-$board/kernel", "size": $kernel_size, "sha256": "$kernel_sha" },
    "rootfs": { "path": "sysupgrade-$board/root", "size": $actual_root_size, "sha256": "$root_sha" },
    "metadata": { "path": "sysupgrade-$board/METADATA", "size": $metadata_size, "sha256": "$metadata_sha" }
  }
}
EOF
    (cd "$pkgdir" && sha256sum kernel root METADATA > SHA256SUMS)
}

signing_policy_selftest() {
    tmpdir=$(mktemp -d 2>/dev/null || echo "/tmp/dcentos-signing-selftest.$$")
    rm -rf "$tmpdir"
    mkdir -p "$tmpdir"

    make_test_sysupgrade_package "$tmpdir/sysupgrade-am3-s19k" release
    (cd "$tmpdir" && tar cf unsigned-release.tar sysupgrade-am3-s19k)
    if sh scripts/pre_flash_validate.sh --package-only "$tmpdir/unsigned-release.tar" am3-s19k >/dev/null 2>&1; then
        rm -rf "$tmpdir"
        return 1
    fi
    if DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1 sh scripts/pre_flash_validate.sh --package-only "$tmpdir/unsigned-release.tar" am3-s19k >/dev/null 2>&1; then
        rm -rf "$tmpdir"
        return 1
    fi

    rm -rf "$tmpdir/sysupgrade-am3-s19k"
    make_test_sysupgrade_package "$tmpdir/sysupgrade-am3-s19k" lab_unsigned
    (cd "$tmpdir" && tar cf unsigned-lab.tar sysupgrade-am3-s19k)
    if DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1 sh scripts/pre_flash_validate.sh --package-only "$tmpdir/unsigned-lab.tar" am3-s19k >/dev/null 2>&1; then
        :
    else
        rm -rf "$tmpdir"
        return 1
    fi

    if SUP_DIR="$tmpdir/stage-release" DCENT_PACKAGE_STATUS=release sh -c '. scripts/lib/sysupgrade_package_common.sh; dcent_stage_release_key' >/dev/null 2>&1; then
        rm -rf "$tmpdir"
        return 1
    fi
    if SUP_DIR="$tmpdir/stage-lab" DCENT_PACKAGE_STATUS=lab_unsigned DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1 sh -c '. scripts/lib/sysupgrade_package_common.sh; dcent_stage_release_key' >/dev/null 2>&1; then
        :
    else
        rm -rf "$tmpdir"
        return 1
    fi

    if command -v openssl >/dev/null 2>&1; then
        mkdir -p "$tmpdir/stage-generated"
        : > "$tmpdir/stage-generated/SHA256SUMS"
        openssl genpkey -algorithm Ed25519 -out "$tmpdir/generated.key" >/dev/null 2>&1 || {
            rm -rf "$tmpdir"
            return 1
        }
        if SUP_DIR="$tmpdir/stage-generated" DCENT_RELEASE_SIGNING_KEY="$tmpdir/generated.key" DCENT_PACKAGE_STATUS=release sh -c '. scripts/lib/sysupgrade_package_common.sh; dcent_stage_release_key' >/dev/null 2>&1; then
            rm -rf "$tmpdir"
            return 1
        fi
        if SUP_DIR="$tmpdir/stage-generated" DCENT_RELEASE_SIGNING_KEY="$tmpdir/generated.key" DCENT_PACKAGE_STATUS=lab_generated_key DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1 sh -c '. scripts/lib/sysupgrade_package_common.sh; dcent_stage_release_key' >/dev/null 2>&1; then
            [ -f "$tmpdir/stage-generated/release_ed25519.pub" ] || {
                rm -rf "$tmpdir"
                return 1
            }
        else
            rm -rf "$tmpdir"
            return 1
        fi
    fi

    rm -rf "$tmpdir"
    return 0
}

zynq_payload_window_selftest() {
    tmpdir=$(mktemp -d 2>/dev/null || echo "/tmp/dcentos-zynq-window-selftest.$$")
    rm -rf "$tmpdir"
    mkdir -p "$tmpdir"

    oversized_root=$((134 * 124 * 1024 + 1))
    make_test_oversized_zynq_package "$tmpdir/sysupgrade-am1-s9" am1-s9 "$oversized_root"
    (cd "$tmpdir" && tar cf oversized-zynq.tar sysupgrade-am1-s9)
    if DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1 sh scripts/pre_flash_validate.sh --package-only "$tmpdir/oversized-zynq.tar" am1-s9 >/dev/null 2>&1; then
        rm -rf "$tmpdir"
        return 1
    fi

    rm -rf "$tmpdir"
    return 0
}

# CE-183: prove dcent_require_release_image_hardening fails closed for
# release-status packages that lack DCENT_RELEASE_IMAGE=1 / the rootfs marker,
# and is a no-op for non-release lab packages.
release_image_hardening_coupling_selftest() {
    tmpdir=$(mktemp -d 2>/dev/null || echo "/tmp/dcentos-relimg-selftest.$$")
    rm -rf "$tmpdir"
    mkdir -p "$tmpdir"

    # 1) release status + DCENT_RELEASE_IMAGE=0 -> must fail closed.
    if DCENT_PACKAGE_STATUS=release DCENT_RELEASE_IMAGE=0 \
        sh -c '. scripts/lib/sysupgrade_package_common.sh; dcent_require_release_image_hardening' >/dev/null 2>&1; then
        rm -rf "$tmpdir"
        return 1
    fi

    # 2) release status + DCENT_RELEASE_IMAGE=1 + TARGET_DIR without marker -> fail.
    mkdir -p "$tmpdir/rootfs-nomarker/etc/dcentos"
    if DCENT_PACKAGE_STATUS=release DCENT_RELEASE_IMAGE=1 TARGET_DIR="$tmpdir/rootfs-nomarker" \
        sh -c '. scripts/lib/sysupgrade_package_common.sh; dcent_require_release_image_hardening' >/dev/null 2>&1; then
        rm -rf "$tmpdir"
        return 1
    fi

    # 3) release status + DCENT_RELEASE_IMAGE=1 + TARGET_DIR WITH marker -> ok.
    mkdir -p "$tmpdir/rootfs-ok/etc/dcentos"
    : > "$tmpdir/rootfs-ok/etc/dcentos/release-image"
    if DCENT_PACKAGE_STATUS=release DCENT_RELEASE_IMAGE=1 TARGET_DIR="$tmpdir/rootfs-ok" \
        sh -c '. scripts/lib/sysupgrade_package_common.sh; dcent_require_release_image_hardening' >/dev/null 2>&1; then
        :
    else
        rm -rf "$tmpdir"
        return 1
    fi

    # 4) non-release lab status -> no-op even without DCENT_RELEASE_IMAGE.
    if DCENT_PACKAGE_STATUS=lab_unsigned DCENT_RELEASE_IMAGE=0 \
        sh -c '. scripts/lib/sysupgrade_package_common.sh; dcent_require_release_image_hardening' >/dev/null 2>&1; then
        :
    else
        rm -rf "$tmpdir"
        return 1
    fi

    rm -rf "$tmpdir"
    return 0
}

# CE-374: functional proof that BOTH NAND backup planners bind the restore
# proof to the exact unit — a marker-only proof (or a MAC/HWID mismatch, or no
# --expect-* supplied) must NOT reach plan_ready=1, and only a matched identity
# emits restore_matched_mac in the JSON. The planners need bash (`local`,
# pipefail); skip gracefully if bash is unavailable.
nand_backup_identity_selftest() {
    command -v bash >/dev/null 2>&1 || return 0
    nbtmp=$(mktemp -d 2>/dev/null || echo "/tmp/dcentos-nandid-selftest.$$")
    rm -rf "$nbtmp"
    mkdir -p "$nbtmp"
    {
        echo 'layout_profile_candidate=1'
        echo '| Node | Size | Erase | Name | Artifact |'
        echo '| --- | --- | --- | --- | --- |'
        echo '| /dev/mtd7 | 0x100000 | 0x20000 | firmware1 | mtd7_firmware1.nanddump |'
    } > "$nbtmp/manifest.md"
    printf 'restore_verified=1\n' > "$nbtmp/marker.txt"
    printf 'restore_verified=1\nrestore_mac=aa:bb:cc:dd:ee:ff\nrestore_hwid=HWID123\n' > "$nbtmp/id.txt"

    nb_rc=0
    for plan in scripts/am1_nand_backup_plan.sh scripts/am2_nand_backup_plan.sh; do
        # (a) marker-only proof -> plan_ready=0 + no "for this exact unit".
        out=$(bash "$plan" --manifest "$nbtmp/manifest.md" \
            --restore-artifact-proof "$nbtmp/marker.txt" \
            --output "$nbtmp/a.md" --json-template "$nbtmp/a.json" 2>/dev/null) || nb_rc=1
        printf '%s\n' "$out" | grep -q 'plan_ready=0' || nb_rc=1
        if grep -q 'verified for this exact unit' "$nbtmp/a.md"; then nb_rc=1; fi

        # (b) matching identity + --expect-* -> plan_ready=1 + matched MAC in JSON.
        out=$(bash "$plan" --manifest "$nbtmp/manifest.md" \
            --restore-artifact-proof "$nbtmp/id.txt" \
            --expect-mac 32:30:41:27:F6:AB --expect-hwid HWID123 \
            --output "$nbtmp/b.md" --json-template "$nbtmp/b.json" 2>/dev/null) || nb_rc=1
        printf '%s\n' "$out" | grep -q 'plan_ready=1' || nb_rc=1
        grep -q '"restore_matched_mac": "aa:bb:cc:dd:ee:ff"' "$nbtmp/b.json" || nb_rc=1

        # (c) wrong MAC -> plan_ready=0.
        out=$(bash "$plan" --manifest "$nbtmp/manifest.md" \
            --restore-artifact-proof "$nbtmp/id.txt" \
            --expect-mac de:ad:be:ef:00:00 --expect-hwid HWID123 \
            --output "$nbtmp/c.md" --json-template "$nbtmp/c.json" 2>/dev/null) || nb_rc=1
        printf '%s\n' "$out" | grep -q 'plan_ready=0' || nb_rc=1

        # (d) identity in proof but no --expect-* supplied -> plan_ready=0.
        out=$(bash "$plan" --manifest "$nbtmp/manifest.md" \
            --restore-artifact-proof "$nbtmp/id.txt" \
            --output "$nbtmp/d.md" --json-template "$nbtmp/d.json" 2>/dev/null) || nb_rc=1
        printf '%s\n' "$out" | grep -q 'plan_ready=0' || nb_rc=1
    done
    rm -rf "$nbtmp"
    return $nb_rc
}

require_no_active_fw_env_lines() {
    file=$1
    label=$2

    if grep -Ev '^[[:space:]]*(#|$)' "$file" >/dev/null 2>&1; then
        fail "$label: fw_env.config has active device lines"
    else
        pass "$label"
    fi
}

# Assert a one-line board identity file (etc/dcentos/<name>) exists and its
# sole non-empty line is exactly the expected canonical string. Buildroot
# overlay identity files are authored as a single value + trailing newline,
# so this rejects both a missing file and a drifted value.
require_identity_file() {
    file=$1
    expected=$2
    label=$3

    if [ ! -f "$file" ]; then
        fail "$label: missing identity file $file"
        return
    fi

    actual=$(tr -d '\r' < "$file" | sed -n '/[^[:space:]]/{p;q;}' | sed 's/[[:space:]]*$//')
    if [ "$actual" = "$expected" ]; then
        pass "$label"
    else
        fail "$label: $file = '$actual', expected '$expected'"
    fi
}

BB_S99='br2_external_dcentos/board/beaglebone/am3-bb/rootfs-overlay/etc/init.d/S99upgrade'
BB_FW_ENV='br2_external_dcentos/board/beaglebone/am3-bb/rootfs-overlay/etc/fw_env.config'
VERIFY='scripts/verify_sysupgrade_signature.sh'
PACKAGE='scripts/package_sysupgrade.sh'
PRE_FLASH='scripts/pre_flash_validate.sh'
VERSION_GATE='scripts/lib/dcentrald_version_gate.sh'
AM2_POST_IMAGE='br2_external_dcentos/board/zynq/am2-s19jpro/post-image.sh'
AM3_S19K_POST_IMAGE='br2_external_dcentos/board/amlogic/am3-s19kpro/post-image.sh'
AM3_S21_POST_IMAGE='br2_external_dcentos/board/amlogic/am3-s21/post-image.sh'
AMLOGIC_S99UPGRADE='br2_external_dcentos/board/amlogic/rootfs-overlay/etc/init.d/S99upgrade'
AM3_GEOMETRY='scripts/lib/am3_geometry.sh'
SYSUPGRADE_COMMON='scripts/lib/sysupgrade_package_common.sh'
AM3_BB_REVERT='scripts/revert_to_stock_am335x_bb.sh'
S9_REVERT='scripts/revert_to_stock_s9.sh'
S17_REVERT='scripts/revert_to_stock_s17.sh'
S19_AM2_REVERT='scripts/revert_to_stock_s19_am2.sh'
AM3_S19K_REVERT='scripts/revert_to_stock_am3_aml_s19k.sh'
AM3_S21_REVERT='scripts/revert_to_stock_am3_aml_s21.sh'
BUILD_DOCKER='scripts/build_in_docker.sh'
BUILD_WSL='scripts/build_in_wsl.sh'
BUILD_AMLOGIC_NATIVE='scripts/build_amlogic_native_install.sh'
S9_SYSUPGRADE='br2_external_dcentos/board/zynq/rootfs-overlay/usr/sbin/sysupgrade'
AM2_SYSUPGRADE='br2_external_dcentos/board/zynq/am2-s19jpro/rootfs-overlay/usr/sbin/sysupgrade'
AM2_S19PRO_SYSUPGRADE='br2_external_dcentos/board/zynq/am2-s19pro/rootfs-overlay/usr/sbin/sysupgrade'
AM2_S17PRO_SYSUPGRADE='br2_external_dcentos/board/zynq/am2-s17pro/rootfs-overlay/usr/sbin/sysupgrade'
RESTORE_ROUTE='dcentrald/dcentrald-api/src/routes/restore_to_stock.rs'
CI_BETA_XIL_RELEASE_GATES='scripts/ci_beta_xil_release_gates.sh'
BETA_XIL_RELEASE_NAMES='scripts/verify_beta_xil_release_names.sh'

require_pattern "$BB_S99" 'no NAND recovery flag or fw_env commit' 'am3-bb S99upgrade declares no NAND/fw_env commit'
require_pattern "$BB_S99" 'dcentrald API management endpoint is reachable' 'am3-bb S99upgrade uses management-readiness wording'
reject_pattern "$BB_S99" 'fw_setenv' 'am3-bb S99upgrade does not call fw_setenv'
reject_pattern "$BB_S99" 'flash_erase' 'am3-bb S99upgrade does not erase NAND'
reject_pattern "$BB_S99" 'nandwrite' 'am3-bb S99upgrade does not write NAND'
reject_pattern "$BB_S99" 'RECOVERY_FLAG_OFFSET' 'am3-bb S99upgrade does not inherit Amlogic recovery offsets'
require_no_active_fw_env_lines "$BB_FW_ENV" 'am3-bb fw_env.config masks Amlogic env layout'

require_pattern "$VERIFY" '#!/bin/sh' 'verifier is POSIX/BusyBox shell'
require_pattern "$VERIFY" 'validate_payload_hash "$SUBDIR_NAME/kernel" "kernel"' 'verifier checks kernel payload hash'
require_pattern "$VERIFY" 'validate_payload_hash "$SUBDIR_NAME/root" "rootfs"' 'verifier checks rootfs payload hash'
require_pattern "$VERIFY" 'validate_payload_hash "$SUBDIR_NAME/METADATA" "metadata"' 'verifier checks metadata payload hash'
require_pattern "$VERIFY" 'validate_payload_hash "$SUBDIR_NAME/release_ed25519.pub" "verification_key"' 'verifier checks embedded verification key hash'
require_pattern "$VERIFY" 'Manifest board_target' 'verifier checks manifest board_target'
require_pattern "$VERIFY" 'Manifest product' 'verifier checks manifest product'
require_pattern "$VERIFY" 'Public key is not a valid PEM public key' 'verifier rejects malformed placeholder public keys'
require_pattern "$VERIFY" 'Package manifest and signature validated' 'verifier uses manifest/signature validated wording'
reject_pattern "$VERIFY" 'Signature verified:' 'verifier no longer reports signature-only proof'

require_pattern "$PACKAGE" 'infer_package_version' 'packager infers package version'
require_pattern "$PACKAGE" 'Package version is required for fail-closed release manifests' 'packager fails closed if version cannot be set'
require_pattern "$PACKAGE" '"version": "$PACKAGE_VERSION"' 'packager writes a non-null manifest version'
reject_pattern "$PACKAGE" '"version": null' 'packager does not emit null manifest version'
require_pattern "$PRE_FLASH" 'AM3 kernel/root uImage magic valid' 'package-only validator has explicit AM3 uImage profile'
require_pattern "$PRE_FLASH" 'AM3 root payload fits am3 rootfs window' 'package-only validator keeps AM3 rootfs window gate'
require_pattern "$PRE_FLASH" 'squashfs-style root payload magic valid' 'package-only validator has squashfs-style board profile'
require_pattern "$PRE_FLASH" 'AM3 uImage/rootfs-window checks skipped for squashfs-style' 'package-only validator does not label S9/am2 packages as AM3 uImage'
require_pattern "$PRE_FLASH" 'AM1_S9_ROOTFS_MAX_BYTES=$((134 * ZYNQ_UBI_LEB_SIZE_BYTES))' 'package-only validator pins S9 rootfs byte window'
require_pattern "$PRE_FLASH" 'AM2_ZYNQ_KERNEL_MAX_BYTES=$(((23 + 4) * ZYNQ_UBI_LEB_SIZE_BYTES))' 'package-only validator pins AM2 kernel byte window with runtime tolerance'
require_pattern "$PRE_FLASH" 'assert_payload_fits_window "$board kernel" "$kernel_size" "$ZYNQ_KERNEL_MAX_BYTES" "zynq kernel window"' 'package-only validator rejects oversized Zynq kernels'
require_pattern "$PRE_FLASH" 'assert_payload_fits_window "$board root" "$root_size" "$ZYNQ_ROOTFS_MAX_BYTES" "zynq rootfs window"' 'package-only validator rejects oversized Zynq rootfs payloads'
require_pattern "$PRE_FLASH" 'am1-s9|am2-s19j|am2-s19jpro|am2-s17p)' 'package-only validator covers am2-s17p runtime-only tarballs'
require_pattern "$PRE_FLASH" 'validate_package_only "$TARBALL" "am1-s9"' 'am1-s9 live pre-flash validates the package before declaring backup-floor success (CE-352)'
require_pattern "$BUILD_DOCKER" 'am3-s19kpro|am3-s21|am3-s19jpro-aml|am3-t21|am2-s19jpro|am2-s17pro)' 'build_in_docker package-validates am2-s17pro tarballs when present'
require_pattern "$S9_SYSUPGRADE" 'payload_fits_ubi_volume' 'S9 sysupgrade validates payload byte fit before UBI writes'
require_pattern "$AM2_SYSUPGRADE" 'payload_fits_ubi_volume' 'am2-s19j sysupgrade validates payload byte fit before UBI writes'
require_pattern "$AM2_S19PRO_SYSUPGRADE" 'payload_fits_ubi_volume' 'am2-s19pro sysupgrade validates payload byte fit before UBI writes'
require_pattern "$AM2_S17PRO_SYSUPGRADE" 'payload_fits_ubi_volume' 'am2-s17p sysupgrade validates payload byte fit before UBI writes'
require_pattern "$S9_SYSUPGRADE" 'validate_sysupgrade_tar_preextract "$ROOTFS"' 'S9 sysupgrade bounds package tar size before extraction'
require_pattern "$AM2_SYSUPGRADE" 'validate_sysupgrade_tar_preextract "$ROOTFS"' 'am2-s19j sysupgrade bounds package tar size before extraction'
require_pattern "$AM2_S19PRO_SYSUPGRADE" 'validate_sysupgrade_tar_preextract "$ROOTFS"' 'am2-s19pro sysupgrade bounds package tar size before extraction'
require_pattern "$AM2_S17PRO_SYSUPGRADE" 'validate_sysupgrade_tar_preextract "$ROOTFS"' 'am2-s17p sysupgrade bounds package tar size before extraction'
require_pattern "$S9_SYSUPGRADE" 'Refusing before tar extraction' 'S9 sysupgrade reports pre-extraction package refusal'
require_pattern "$AM2_SYSUPGRADE" 'Refusing before tar extraction' 'am2-s19j sysupgrade reports pre-extraction package refusal'
require_pattern "$AM2_S19PRO_SYSUPGRADE" 'Refusing before tar extraction' 'am2-s19pro sysupgrade reports pre-extraction package refusal'
require_pattern "$AM2_S17PRO_SYSUPGRADE" 'Refusing before tar extraction' 'am2-s17p sysupgrade reports pre-extraction package refusal'
require_pattern "$S9_SYSUPGRADE" 'Refusing before ubiupdatevol' 'S9 sysupgrade names the pre-write oversized-payload refusal'
require_pattern "$AM2_SYSUPGRADE" 'Refusing before ubiupdatevol' 'am2-s19j sysupgrade names the pre-write oversized-payload refusal'
require_pattern "$AM2_S19PRO_SYSUPGRADE" 'Refusing before ubiupdatevol' 'am2-s19pro sysupgrade names the pre-write oversized-payload refusal'
require_pattern "$AM2_S17PRO_SYSUPGRADE" 'Refusing before ubiupdatevol' 'am2-s17p sysupgrade names the pre-write oversized-payload refusal'
for zynq_sysupgrade in "$S9_SYSUPGRADE" "$AM2_SYSUPGRADE" "$AM2_S19PRO_SYSUPGRADE" "$AM2_S17PRO_SYSUPGRADE"; do
    require_pattern "$zynq_sysupgrade" 'enforce_sysupgrade_version_floor || exit 1' "Zynq sysupgrade $(basename "$zynq_sysupgrade") enforces downgrade floor before payload writes"
    require_pattern "$zynq_sysupgrade" 'DCENT_SYSUPGRADE_VERSION_PATH' "Zynq sysupgrade $(basename "$zynq_sysupgrade") exposes offline version-file seam"
    require_pattern "$zynq_sysupgrade" 'DCENT_ALLOW_DOWNGRADE' "Zynq sysupgrade $(basename "$zynq_sysupgrade") requires explicit downgrade override"
    require_pattern "$zynq_sysupgrade" '[ "$ALLOW_DOWNGRADE" = "1" ] && ! is_release_status "$PACKAGE_STATUS"' "Zynq sysupgrade $(basename "$zynq_sysupgrade") allows downgrade override only for non-release packages"
    require_pattern "$zynq_sysupgrade" 'Downgrade refused: package version' "Zynq sysupgrade $(basename "$zynq_sysupgrade") reports signed-package downgrade refusal"
    require_pattern "$zynq_sysupgrade" 'command -v jsonfilter' "Zynq sysupgrade $(basename "$zynq_sysupgrade") prefers jsonfilter for manifest parsing"
    require_pattern "$zynq_sysupgrade" 'command -v python3' "Zynq sysupgrade $(basename "$zynq_sysupgrade") checks python3 before manifest parsing"
    require_pattern "$zynq_sysupgrade" 'Error: manifest_field needs jsonfilter or python3' "Zynq sysupgrade $(basename "$zynq_sysupgrade") fails clearly when manifest parser is unavailable"
done
require_pattern "$BUILD_DOCKER" 'release/verified builds fail closed on missing or mismatched toolchain' 'docker release builds document mandatory toolchain SHA gate'
require_pattern "$BUILD_DOCKER" 'ERROR (DEVOPS-002): no expected SHA256 pinned' 'docker release build fails closed when no toolchain SHA is pinned'
require_pattern "$BUILD_DOCKER" 'TOOLCHAIN_SHA256_MANDATORY=1' 'docker release build makes toolchain SHA mismatch mandatory'
require_pattern "$CI_BETA_XIL_RELEASE_GATES" 'verify_beta_xil_release_names.sh' 'Xilinx beta release gate verifies release-name/packet consistency before artifact checks'
require_pattern "$BETA_XIL_RELEASE_NAMES" 'firmware_release_name.sh' 'Xilinx beta release-name verifier derives names from the canonical helper'
if sh "$BETA_XIL_RELEASE_NAMES" >/dev/null; then
    pass "Xilinx beta release-name verifier passes on committed packet/checksum metadata"
else
    fail "Xilinx beta release-name verifier failed"
fi

# --- BUG 1 (build): Zynq sysupgrade kernel must be a bootable FIT/uImage ------
# The S9/Zynq NAND boot path uses U-Boot `bootm`, which boots ONLY a FIT
# (d00dfeed) or a legacy uImage (27051956). The BraiinsOS-extracted kernel.bin
# is a BARE ARM zImage (magic 0x016f2818 at offset 0x24) — copying it straight
# into sysupgrade-<board>/kernel bricked .135. These guards pin (a) the
# packager wraps the bare zImage into a NAND FIT, and (b) the pre-flash
# validator REJECTS a bare-zImage kernel for the zynq boards.
require_pattern "$PACKAGE" 'build_nand_kernel_fit' 'packager defines the bare-zImage -> NAND FIT wrapper'
require_pattern "$PACKAGE" 'kernel_is_bootm_ready' 'packager classifies the kernel container (FIT/uImage vs bare zImage)'
require_pattern "$PACKAGE" 'is a bare zImage' 'packager logs when it wraps a bare zImage into a FIT'
require_pattern "$PACKAGE" 'no ramdisk' 'packager NAND FIT mirrors the SD .its minus the ramdisk node'
require_pattern "$PACKAGE" 'Staged sysupgrade kernel is not bootm-ready' 'packager fails closed if the staged kernel is not bootm-ready'
require_pattern "$PACKAGE" 'load = <0x00008000>' 'packager NAND FIT loads the kernel at 0x8000'
require_pattern "$PRE_FLASH" 'is NOT a bootm-ready FIT/uImage' 'pre-flash validator rejects a bare-zImage zynq kernel'
require_pattern "$PRE_FLASH" 'kernel payload is a bootable FIT' 'pre-flash validator accepts a FIT zynq kernel'
# --- end BUG 1 Zynq FIT-kernel coverage --------------------------------------

# --- BUG 3: DCENT_OS DHCP client must keep a stable MAC-based identity --------
# On the BraiinsOS->DCENT_OS swap, DCENT_OS pulled a NEW lease (.135 -> .100)
# because its udhcpc sent a different client-identifier than BraiinsOS. The fix
# makes S40network send RFC 2132 option 61 (client-identifier) = "01"+MAC, the
# same form stock/Braiins firmware use, so the lease (and IP) survive the swap.
S40NETWORK='br2_external_dcentos/board/zynq/rootfs-overlay/etc/init.d/S40network'
require_pattern "$S40NETWORK" '/sys/class/net/$INTERFACE/address' 'S40network reads the interface MAC for the DHCP client-id'
require_pattern "$S40NETWORK" '0x3d:$CLIENTID_HEX' 'S40network sends udhcpc raw option 61 (client-identifier)'
require_pattern "$S40NETWORK" 'CLIENTID_HEX="01' 'S40network builds the option-61 client-id as Ethernet-type "01" + MAC'
# --- end BUG 3 DHCP stable-identity coverage ---------------------------------
require_pattern 'br2_external_dcentos/board/zynq/post-image-ramdisk.sh' 'MAX_UBI_SIZE=$((134 * 124 * 1024))' 'S9 post-image warning threshold uses live 2026-06-05 rootfs volume capacity'
require_pattern 'br2_external_dcentos/board/zynq/post-image-ramdisk.sh' 'exceeds live S9 UBI rootfs volume' 'S9 post-image warning names live UBI rootfs fit blocker'
reject_pattern 'br2_external_dcentos/board/zynq/post-image-ramdisk.sh' '166 * 124 * 1024' 'S9 post-image no longer uses stale 166-LEB capacity'
reject_pattern 'br2_external_dcentos/board/zynq/post-image-ramdisk.sh' 'within observed S9 volume' 'S9 post-image no longer reports stale observed-volume OK wording'
require_pattern "$VERSION_GATE" 'dcent_require_dcentrald_version_match' 'shared dcentrald version gate helper exists'
require_pattern "$VERSION_GATE" 'binary string $actual_string != expected $expected_string' 'dcentrald version gate compares binary user-agent version to staged metadata'
require_pattern "$VERSION_GATE" 'staged version $staged_version from $staged_source != Cargo workspace version $cargo_version' 'dcentrald version gate compares staged metadata to Cargo workspace version'
require_pattern "$VERSION_GATE" 'DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1' 'dcentrald version gate uses existing explicit lab override pattern'
require_pattern "$PACKAGE" 'dcent_require_dcentrald_version_match' 'standalone packager gates staged dcentrald version when target dir is present'
require_pattern 'scripts/build_in_docker.sh' 'build_in_docker Phase 5' 'docker build validates staged ARM dcentrald before Buildroot'
require_pattern "$BUILD_DOCKER" '--lab-unsigned' 'docker build requires explicit lab unsigned flag'
require_pattern "$BUILD_DOCKER" 'release sysupgrade builds require DCENT_RELEASE_SIGNING_KEY' 'docker build fails closed for unsigned release sysupgrade builds'
reject_pattern "$BUILD_DOCKER" 'DCENT_ALLOW_UNSIGNED_SYSUPGRADE="${DCENT_ALLOW_UNSIGNED_SYSUPGRADE:-1}"' 'docker build does not default to unsigned lab override'
reject_pattern "$BUILD_DOCKER" '-e DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1' 'docker build does not hardcode unsigned lab override into container'
require_pattern "$BUILD_WSL" 'release sysupgrade packaging requires DCENT_RELEASE_SIGNING_KEY' 'WSL build fails closed for unsigned release sysupgrade builds'
reject_pattern "$BUILD_WSL" 'export DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1' 'WSL build does not auto-enable unsigned lab override'
require_pattern "$BUILD_AMLOGIC_NATIVE" '--lab-unsigned' 'amlogic native build accepts explicit lab unsigned flag'
reject_pattern "$BUILD_AMLOGIC_NATIVE" 'DCENT_ALLOW_UNSIGNED_SYSUPGRADE="${DCENT_ALLOW_UNSIGNED_SYSUPGRADE:-1}"' 'amlogic native build does not default to unsigned lab override'
require_pattern 'br2_external_dcentos/board/zynq/post-build.sh' 'dcent_require_dcentrald_version_match' 'S9 post-build gates staged dcentrald version'
require_pattern 'br2_external_dcentos/board/zynq/am2-s19jpro/post-build.sh' 'dcent_require_dcentrald_version_match' 'am2 post-build gates staged dcentrald version'
require_pattern 'br2_external_dcentos/board/zynq/am2-s19jpro/post-build.sh' 'revert_to_stock_s19_am2.sh' 'am2 post-build ships profile revert helper'
require_pattern 'br2_external_dcentos/board/zynq/am2-s19jpro/post-build.sh' 'stock-bitmain-manifest.json' 'am2 post-build ships stock Bitmain manifest'
require_pattern 'br2_external_dcentos/board/amlogic/am3-s19kpro/post-build.sh' 'dcent_require_dcentrald_version_match' 'am3-s19k post-build gates staged dcentrald version'
require_pattern 'br2_external_dcentos/board/amlogic/am3-s19kpro/post-build.sh' 'usr/sbin/lib/am3_geometry.sh' 'am3-s19k post-build ships AM3 geometry helper for revert'
require_pattern 'br2_external_dcentos/board/amlogic/am3-s21/post-build.sh' 'dcent_require_dcentrald_version_match' 'am3-s21 post-build gates staged dcentrald version'
require_pattern 'br2_external_dcentos/board/amlogic/am3-s21/post-build.sh' 'usr/sbin/lib/am3_geometry.sh' 'am3-s21 post-build ships AM3 geometry helper for revert'
require_pattern 'br2_external_dcentos/board/beaglebone/am3-bb/post-build.sh' 'dcent_require_dcentrald_version_match' 'am3-bb post-build gates staged dcentrald version'
require_pattern "$AM2_POST_IMAGE" '"version": "${PACKAGE_VERSION}"' 'am2 post-image writes a non-null manifest version'
require_pattern "$AM3_S19K_POST_IMAGE" '"version": "${PACKAGE_VERSION}"' 'am3-s19k post-image writes a non-null manifest version'
require_pattern "$AM3_S21_POST_IMAGE" '"version": "${PACKAGE_VERSION}"' 'am3-s21 post-image writes a non-null manifest version'
require_pattern "$AM2_POST_IMAGE" 'dcent_write_sysupgrade_manifest' 'am2 post-image uses shared AM2/AM3 manifest writer'
require_pattern "$AM3_S19K_POST_IMAGE" 'dcent_write_sysupgrade_manifest' 'am3-s19k post-image uses shared AM2/AM3 manifest writer'
require_pattern "$AM3_S21_POST_IMAGE" 'dcent_write_sysupgrade_manifest' 'am3-s21 post-image uses shared AM2/AM3 manifest writer'
require_pattern "$SYSUPGRADE_COMMON" 'dcent_stage_release_key' 'shared manifest helper stages release key'
require_pattern "$SYSUPGRADE_COMMON" 'dcent_sign_sysupgrade_manifest' 'shared manifest helper signs and verifies manifest'
require_pattern "$SYSUPGRADE_COMMON" 'production release package requires trusted release keys/signatures' 'shared signing helper fails closed for production release mode'
require_pattern "$SYSUPGRADE_COMMON" 'self-derived generated-key package' 'shared signing helper labels generated-key flow lab-only'
require_pattern "$PACKAGE" 'Production release signing requires --verify-pubkey or DCENT_RELEASE_PUBKEY_FILE' 'standalone packager requires trusted public key for production signing'
require_pattern "$PACKAGE" 'unsigned package generation' 'standalone packager gates unsigned package generation'
require_pattern "$AM3_S19K_POST_IMAGE" 'DCENT_TARGET_SIDE_SYSUPGRADE=false' 'am3-s19k metadata disables target-side sysupgrade'
require_pattern "$AM3_S21_POST_IMAGE" 'DCENT_TARGET_SIDE_SYSUPGRADE=false' 'am3-s21 metadata disables target-side sysupgrade'
require_pattern "$AM3_S19K_POST_IMAGE" 'host_driven_rootfs_window_lab' 'am3-s19k metadata marks host-driven lab install'
require_pattern "$AM3_S21_POST_IMAGE" 'host_driven_rootfs_window_lab' 'am3-s21 metadata marks host-driven lab install'
require_pattern "$AM3_GEOMETRY" 'DCENT_AM3_ROOTFS_OFFSET_HEX="${DCENT_AM3_ROOTFS_OFFSET_HEX:-0x05700000}"' 'am3 geometry centralizes rootfs offset'
require_pattern "$AM3_GEOMETRY" 'DCENT_AM3_ROOTFS_WINDOW_HEX="${DCENT_AM3_ROOTFS_WINDOW_HEX:-0x02800000}"' 'am3 geometry centralizes rootfs window'
require_pattern "$AM3_S21_REVERT" 'ROOTFS_OFFSET="$DCENT_AM3_ROOTFS_OFFSET_HEX"' 'am3-s21 revert uses shared rootfs offset'
require_pattern "$RESTORE_ROUTE" '.arg(&post_dwell_fp.sha256)' 'restore route passes post-dwell SHA into revert helper'
for revert_script in "$S9_REVERT" "$S17_REVERT" "$S19_AM2_REVERT" "$AM3_S19K_REVERT" "$AM3_S21_REVERT"; do
    require_pattern "$revert_script" 'EXPECTED_SHA256=' "stock revert helper $(basename "$revert_script") accepts expected SHA"
    require_pattern "$revert_script" 'Firmware SHA-256 verified at extraction time.' "stock revert helper $(basename "$revert_script") verifies SHA before extract"
    require_pattern "$revert_script" 'MAX_EXTRACTED_KB' "stock revert helper $(basename "$revert_script") caps extracted size"
    require_pattern "$revert_script" 'firmware archive contains hard-linked files' "stock revert helper $(basename "$revert_script") rejects hard-linked files"
done

# NAND-safety for the AMLOGIC reverts. Amlogic (A113D) writes the stock rootfs
# into a uImage window with `nandwrite -p` PAGE writes and must NEVER call
# `flash_erase` (partition-erase = the weak-ECC brick / corruption class of the
# .74/.139 incident; the s21 script documents this in prose). This is
# Amlogic-only: the Zynq reverts legitimately `flash_erase` the INACTIVE firmware
# SLOT (never the env — that flips via fw_setenv, already checked below). The
# guard is comment-aware so the scripts that DOCUMENT the ban don't self-trip.
AM3_S19JPRO_REVERT='scripts/revert_to_stock_am3_aml_s19jpro.sh'
AM3_T21_REVERT='scripts/revert_to_stock_am3_aml_t21.sh'
for aml_revert in "$AM3_S19K_REVERT" "$AM3_S21_REVERT" "$AM3_S19JPRO_REVERT" "$AM3_T21_REVERT"; do
    reject_executable_pattern "$aml_revert" 'flash_erase' "amlogic revert helper $(basename "$aml_revert") never flash_erases a partition (nandwrite page-writes only; brick op)"
done
require_pattern "$S17_REVERT" 'command -v fw_printenv' 'S17 stock revert requires fw_printenv before destructive work'
require_pattern "$S17_REVERT" 'command -v fw_setenv' 'S17 stock revert requires fw_setenv before destructive work'
require_pattern "$S17_REVERT" "Refusing to infer active slot" 'S17 stock revert fails closed on unknown bootslot'
reject_executable_pattern "$S17_REVERT" 'fw_printenv -n bootslot 2>/dev/null || echo "a"' 'S17 stock revert no longer defaults unknown bootslot to slot a'
require_pattern "$S17_REVERT" 'fw_setenv --script "$FW_SETENV_SCRIPT"' 'S17 stock revert applies boot env flip through fw_setenv script'
require_pattern "$S17_REVERT" 'POST_FLIP_SLOT=$(fw_printenv -n bootslot' 'S17 stock revert verifies post-flip bootslot through fw_printenv'
require_pattern "$S17_REVERT" 'Post-flip boot slot verified via fw_printenv' 'S17 stock revert reports success only after env readback'
require_pattern "$AM3_BB_REVERT" 'AM335x BB NAND revert is disabled' 'am3-bb NAND revert disabled pending evidence'
require_pattern "$AM3_BB_REVERT" 'DCENT_AM3_BB_PROC_MTD_EVIDENCE' 'am3-bb NAND revert requires proc-mtd evidence before override'
require_pattern "$AM3_BB_REVERT" 'DCENT_AM3_BB_ENABLE_NAND_REVERT is not accepted as a bypass' 'am3-bb NAND revert has no env-only bypass'
require_pattern 'br2_external_dcentos/board/beaglebone/am3-bb/post-build.sh' 'echo "am3-bb" > "${TARGET_DIR}/etc/dcentos/board_family"' 'am3-bb post-build stamps unambiguous board_family'

# --- Phase 2D / 2E Buildroot variant identity coverage (QA-F002) -------------
# Phases 2D (am2-s19pro-zynq) and 2E (am2-s17pro-zynq) shipped ~4000 LOC of
# Buildroot variant with ZERO static/CI coverage. Regression-pin the per-variant
# board-identity overlay files so the Phase 4A S99verify V1-V14 multi-family
# port (which consumes these etc/dcentos/* files) and the per-variant
# sysupgrade writer cannot silently drift. Canonical strings VERIFIED from the
# on-disk overlay files, not guessed.
AM2_S19PRO_OVL='br2_external_dcentos/board/zynq/am2-s19pro/rootfs-overlay/etc/dcentos'
AM2_S17PRO_OVL='br2_external_dcentos/board/zynq/am2-s17pro/rootfs-overlay/etc/dcentos'

require_identity_file "$AM2_S19PRO_OVL/platform" 'zynq-bm3-am2' 'am2-s19pro overlay stamps platform=zynq-bm3-am2'
require_identity_file "$AM2_S19PRO_OVL/board_family" 'am2' 'am2-s19pro overlay stamps board_family=am2'
require_identity_file "$AM2_S19PRO_OVL/board_target" 'am2-s19pro' 'am2-s19pro overlay stamps board_target=am2-s19pro'

require_identity_file "$AM2_S17PRO_OVL/platform" 'zynq-bm3-am2' 'am2-s17pro overlay stamps platform=zynq-bm3-am2'
require_identity_file "$AM2_S17PRO_OVL/board_family" 'am2' 'am2-s17pro overlay stamps board_family=am2'
require_identity_file "$AM2_S17PRO_OVL/board_target" 'am2-s17p' 'am2-s17pro overlay stamps board_target=am2-s17p'
# --- end Phase 2D / 2E variant identity coverage -----------------------------

# --- AM3-BB cold-boot management-only gate (matrix §7 #4) --------------------
# Productionization sweep (2026-05-21) QA CRITICAL-3 + Thermal CRITICAL +
# DevOps WARNING-3: the AM3-BB install image baked `[mining] enabled = true` +
# a real Public Pool and S82dcentrald unconditionally passed --am3-bb-mining on
# the beaglebone platform, so a cold-boot install image auto-energized the PSU
# and drove the ASIC chains while SD-first/thermal/restore proof remained
# blocked. The fix gates the cold-boot mining start behind a proof marker
# (mirrors the /etc/dcentos/release-image trust-boundary pattern): absent the
# marker the daemon comes up management-only (no --am3-bb-mining + a
# management-only config whose empty pool + `[mining].enabled=false` makes
# dcentrald's `mining_start_enabled()` gate keep the hardware off). These
# guards pin that the install image does NOT auto-mine without the proof
# marker, and that the management-only fail-closed config ships.
BB_S82='br2_external_dcentos/board/beaglebone/am3-bb-s19jpro/rootfs-overlay/etc/init.d/S82dcentrald'
BB_MGMT_CFG='br2_external_dcentos/board/beaglebone/am3-bb-s19jpro/rootfs-overlay/etc/dcentrald.management-only.toml'
BB_S19JPRO_POSTBUILD='br2_external_dcentos/board/beaglebone/am3-bb-s19jpro/post-build.sh'

require_pattern "$BB_S82" 'AM3_BB_COLDBOOT_PROOF_MARKER="/data/dcentos/am3-bb-coldboot-proven"' 'am3-bb-s19jpro S82dcentrald defines the cold-boot proof marker'
require_pattern "$BB_S82" 'if [ -f "$AM3_BB_COLDBOOT_PROOF_MARKER" ]; then' 'am3-bb-s19jpro S82dcentrald gates mining start on the proof marker'
require_pattern "$BB_S82" 'MANAGEMENT-ONLY until cold-boot proof' 'am3-bb-s19jpro S82dcentrald logs management-only posture when unproven'
require_pattern "$BB_S82" 'AM3_BB_MGMT_ONLY_CONFIG="/etc/dcentrald.management-only.toml"' 'am3-bb-s19jpro S82dcentrald points at the management-only config'
# The ONLY --am3-bb-mining invocation must be inside the marker-present branch.
# There must be exactly one --am3-bb-mining ARGS assignment and it must follow
# the marker test. Assert the marker test precedes the --am3-bb-mining ARGS.
if awk '
    /if \[ -f "\$AM3_BB_COLDBOOT_PROOF_MARKER" \]; then/ { seen_marker = 1 }
    /ARGS="--am3-bb-mining \$ARGS"/ { if (!seen_marker) { bad = 1 } }
    END { exit (bad ? 1 : 0) }
' "$BB_S82"; then
    pass 'am3-bb-s19jpro S82dcentrald: --am3-bb-mining ARGS only set inside the proof-marker branch'
else
    fail 'am3-bb-s19jpro S82dcentrald: --am3-bb-mining ARGS set OUTSIDE the proof-marker branch (would auto-mine on cold boot)'
fi
require_pattern "$BB_MGMT_CFG" 'enabled = false' 'am3-bb-s19jpro management-only config disables mining'
require_identity_file "$BB_MGMT_CFG" '# DCENT_OS Mining Daemon Configuration - Antminer S19j Pro AM335x BB' 'am3-bb-s19jpro management-only config ships with the expected header'
# The management-only config must have an EMPTY pool url (defense in depth: a
# blank pool alone makes mining_start_enabled() false even if `enabled` drifts).
require_pattern "$BB_MGMT_CFG" 'url = ""' 'am3-bb-s19jpro management-only config has an empty pool url'
require_pattern "$BB_S19JPRO_POSTBUILD" '/etc/dcentrald.management-only.toml missing from rootfs' 'am3-bb-s19jpro post-build verifies the management-only config ships'
# --- end AM3-BB cold-boot management-only gate -------------------------------

reject_pattern "$AM3_BB_REVERT" 'flash_erase' 'am3-bb NAND revert script contains no erase path'
reject_pattern "$AM3_BB_REVERT" 'nandwrite' 'am3-bb NAND revert script contains no write path'
reject_pattern "$AM3_BB_REVERT" 'fw_setenv' 'am3-bb NAND revert script contains no env write path'
require_pattern "$S9_SYSUPGRADE" 'PACKAGE_STATUS=$(manifest_field "$PACKAGE_MANIFEST" status' 'S9 target sysupgrade reads manifest package status'
require_pattern "$S9_SYSUPGRADE" 'unsigned release package is not allowed' 'S9 target sysupgrade rejects unsigned release packages even with lab override'
require_pattern "$S9_SYSUPGRADE" 'DCENT_PACKAGE_STATUS to be a non-release lab value' 'S9 target raw lab sysupgrade requires non-release status'
require_pattern "$AM2_SYSUPGRADE" 'PACKAGE_STATUS=$(manifest_field "$PACKAGE_MANIFEST" status' 'am2 target sysupgrade reads manifest package status'
require_pattern "$AM2_SYSUPGRADE" 'unsigned release package is not allowed' 'am2 target sysupgrade rejects unsigned release packages even with lab override'
require_pattern "$AM2_SYSUPGRADE" 'DCENT_PACKAGE_STATUS to be a non-release lab value' 'am2 target raw lab sysupgrade requires non-release status'
require_pattern "$AM2_SYSUPGRADE" 'DCENT_ALLOW_AM2_S19J_AMBIGUOUS_BOS_PLATFORM' 'am2-s19j sysupgrade requires guided override for ambiguous generic AM2 platform'
require_pattern "$AM2_SYSUPGRADE" 'ambiguous AM2 platform marker' 'am2-s19j sysupgrade explains zynq-bm3-am2 ambiguity'
require_pattern "$AM2_SYSUPGRADE" 'Wrong AM2 image = brick' 'am2-s19j sysupgrade treats ambiguous first-flash as a brick guard'
reject_executable_pattern "$AM2_SYSUPGRADE" 'zynq-bm3-am2) DETECTED_BOARD="am2-s19j"' 'am2-s19j sysupgrade never unconditionally maps generic AM2 platform to S19j'

# --- AM2 U-Boot env-flip must use fw_setenv, NEVER raw mtd4 writes ----------
# Production-readiness matrix §7 #6/#8 + load-bearing rule
# : the AM2 control board's
# pl35x-nand env partition (mtd4) is weak-ECC and was bricked TWICE by the
# raw dd/flash_erase/nandwrite env-flip. The .139-proven am2-s19jpro variant
# is the canonical fw_setenv model; the am2-s19pro + am2-s17pro siblings must
# match it. These guards prevent the raw-env-write path from ever returning
# to ANY am2 sysupgrade variant (the rootfs A/B writes use ubiupdatevol, not
# nandwrite, so a literal flash_erase/nandwrite token in these scripts can
# only be a regression to the banned env-flip method).
for am2_sup in "$AM2_SYSUPGRADE" "$AM2_S19PRO_SYSUPGRADE" "$AM2_S17PRO_SYSUPGRADE"; do
    am2_name=$(echo "$am2_sup" | sed 's#.*/zynq/##; s#/rootfs-overlay.*##')
    require_pattern "$am2_sup" 'fw_setenv' "am2 sysupgrade [$am2_name] flips boot-selector via fw_setenv"
    require_pattern "$am2_sup" 'fw_printenv' "am2 sysupgrade [$am2_name] verifies env flip via fw_printenv"
    require_pattern "$am2_sup" 'WRONG_BOARD_EXIT=78' "am2 sysupgrade [$am2_name] uses a distinct wrong-board exit"
    require_pattern "$am2_sup" 'exit "$WRONG_BOARD_EXIT"' "am2 sysupgrade [$am2_name] exits non-zero on wrong board even in test/dry-run"
    reject_pattern "$am2_sup" 'dry-run permissive, continuing' "am2 sysupgrade [$am2_name] never green-lights wrong-board dry-runs"
    # Executable-only: the proven am2-s19jpro variant keeps a recovery-hint
    # comment that NAMES the banned raw path in prose; only an actual
    # invocation is a regression.
    reject_executable_pattern "$am2_sup" 'flash_erase /dev/mtd4' "am2 sysupgrade [$am2_name] never erases the mtd4 env partition"
    reject_executable_pattern "$am2_sup" 'nandwrite -p /dev/mtd4' "am2 sysupgrade [$am2_name] never nandwrites the mtd4 env partition"
    reject_executable_pattern "$am2_sup" 'switch_firmware.py' "am2 sysupgrade [$am2_name] does not invoke the raw-env switch_firmware.py helper"
done

require_pattern "$AM2_S19PRO_SYSUPGRADE" 'PACKAGE_STATUS=$(manifest_field "$PACKAGE_MANIFEST" status' 'am2-s19pro sysupgrade reads manifest package status'
require_pattern "$AM2_S17PRO_SYSUPGRADE" 'PACKAGE_STATUS=$(manifest_field "$PACKAGE_MANIFEST" status' 'am2-s17pro sysupgrade reads manifest package status'
require_pattern "$AM2_S19PRO_SYSUPGRADE" 'DCENT_ALLOW_AM2_S19PRO_AMBIGUOUS_BOS_PLATFORM' 'am2-s19pro sysupgrade requires explicit lab override for ambiguous generic AM2 platform'
require_pattern "$AM2_S19PRO_SYSUPGRADE" 'ambiguous AM2 platform marker' 'am2-s19pro sysupgrade explains zynq-bm3-am2 ambiguity'
require_pattern "$AM2_S19PRO_SYSUPGRADE" 'Wrong AM2 image = brick' 'am2-s19pro sysupgrade treats ambiguous first-flash as a brick guard'
reject_executable_pattern "$AM2_S19PRO_SYSUPGRADE" 'zynq-bm3-am2) DETECTED_BOARD="am2-s19pro"' 'am2-s19pro sysupgrade never unconditionally maps generic AM2 platform to S19 Pro'
require_pattern "$AM2_S17PRO_SYSUPGRADE" 'DCENT_ALLOW_AM2_S17P_AMBIGUOUS_BOS_PLATFORM' 'am2-s17pro sysupgrade requires explicit lab override for ambiguous generic AM2 platform'
require_pattern "$AM2_S17PRO_SYSUPGRADE" 'ambiguous AM2 platform marker' 'am2-s17pro sysupgrade explains zynq-bm3-am2 ambiguity'
require_pattern "$AM2_S17PRO_SYSUPGRADE" 'Wrong AM2 image = brick' 'am2-s17pro sysupgrade treats ambiguous first-flash as a brick guard'
reject_executable_pattern "$AM2_S17PRO_SYSUPGRADE" 'zynq-bm3-am2) DETECTED_BOARD="am2-s17p"' 'am2-s17pro sysupgrade never unconditionally maps generic AM2 platform to S17'
# --- end AM2 env-flip fw_setenv coverage ------------------------------------

# --- BASE zynq sysupgrade is am1-s9-ONLY; AM2 reachability is overlay-fenced -
# Productionization sweep (2026-05-21) DevOps CRITICAL-1 / Security CRITICAL-5
# claimed the BASE board/zynq/rootfs-overlay/usr/sbin/sysupgrade raw mtd4
# env-flip is AM2-reachable. It is NOT: Buildroot applies BR2_ROOTFS_OVERLAY
# left-to-right (later overlays overwrite earlier files), so every AM2 product
# defconfig that lists the base overlay ALSO appends its own product overlay
# that REPLACES usr/sbin/sysupgrade with the fw_setenv writer. The base raw
# script therefore ships ONLY in the am1-s9 image. These guards pin that
# invariant so a future defconfig/overlay change cannot silently expose the
# raw env-flip to an AM2 weak-ECC target.
#
# (1) The base script has NO AM2 board-detection branch — it resolves only
#     am1-s9 / unknown. (A regression that taught it to self-detect AM2 would
#     make the raw mtd4 path AM2-reachable.)
# Executable-only: the scope-documenting comment block legitimately NAMES the
# am2 platform/board strings in prose; only an actual executable reference
# (e.g. a DETECTED_BOARD="am2-*" assignment or a `zynq-bm3-am2` case branch)
# would make the raw mtd4 path AM2-reachable.
reject_executable_pattern "$S9_SYSUPGRADE" 'zynq-bm3-am2' 'base zynq sysupgrade has no executable am2 platform branch'
reject_executable_pattern "$S9_SYSUPGRADE" 'DETECTED_BOARD="am2' 'base zynq sysupgrade never resolves an am2 board target'
require_pattern "$S9_SYSUPGRADE" 'DETECTED_BOARD="am1-s9"' 'base zynq sysupgrade only ever resolves am1-s9'
require_pattern "$S9_SYSUPGRADE" 'ships ONLY in the am1-s9 image' 'base zynq sysupgrade documents its am1-only scope + AM2 overlay-replace'

# (2) Overlay-precedence invariant: every defconfig that lists the BASE zynq
#     overlay AND targets AM2 (platform zynq-bm3-am2) MUST also append a
#     product overlay with its own fw_setenv sysupgrade. We assert each known
#     AM2 zynq defconfig chains base + a product overlay dir on its
#     BR2_ROOTFS_OVERLAY line; and that the matching product sysupgrade exists
#     and uses fw_setenv (already covered above). am1-s9 chains the base only.
CONFIGS='br2_external_dcentos/configs'
for am2_cfg in dcentos_am2_s19jpro_defconfig dcentos_am2_s19pro_defconfig dcentos_am2_s17pro_zynq_defconfig; do
    cfg_path="$CONFIGS/$am2_cfg"
    if [ ! -f "$cfg_path" ]; then
        fail "AM2 overlay-precedence [$am2_cfg]: defconfig missing"
        continue
    fi
    ovl_line=$(grep '^BR2_ROOTFS_OVERLAY=' "$cfg_path" || echo '')
    case "$ovl_line" in
        *board/zynq/rootfs-overlay*board/zynq/am2-*/rootfs-overlay*)
            pass "AM2 overlay-precedence [$am2_cfg]: base overlay precedes an am2 product overlay (product fw_setenv sysupgrade wins)" ;;
        *)
            fail "AM2 overlay-precedence [$am2_cfg]: BR2_ROOTFS_OVERLAY must chain base zynq overlay THEN an am2-*/rootfs-overlay product override (else the raw-mtd4 base sysupgrade would ship on AM2)" ;;
    esac
done

# (3) The am1-s9 defconfig chains the BASE overlay only (so the raw-mtd4 base
#     sysupgrade is the intended am1-s9 healthy-NAND writer, not a leak).
S9_DEFCONFIG="$CONFIGS/dcentos_s9_defconfig"
s9_ovl_line=$(grep '^BR2_ROOTFS_OVERLAY=' "$S9_DEFCONFIG" || echo '')
case "$s9_ovl_line" in
    *board/zynq/am2-*) fail "am1-s9 defconfig must NOT chain an am2 product overlay" ;;
    *board/zynq/rootfs-overlay*) pass "am1-s9 defconfig chains the base zynq overlay only (raw mtd4 is the intended healthy-NAND path)" ;;
    *) fail "am1-s9 defconfig BR2_ROOTFS_OVERLAY does not reference the base zynq overlay" ;;
esac
# --- end base-sysupgrade am1-only overlay-precedence coverage ----------------

# --- Legacy sd_nand_install.sh: raw mtd4 fallback is am1/S9-ONLY (am2-fenced) -
# RE WARNING (2026-05-21): scripts/sd_nand_install.sh retains a raw
# nanddump/flash_erase/nandwrite /dev/mtd4 fallback when fw_setenv is absent.
# It is am1/S9-ONLY: the script HARD-REFUSES (die) any aarch64 / s17 / s19 /
# s21 / am2 board family at its top before any NAND detection, so the raw
# fallback can never execute on an AM2 weak-ECC pl35x-nand env partition. Pin
# both the early refusal AND that the helper prefers fw_setenv first.
SD_NAND_INSTALL='scripts/sd_nand_install.sh'
require_pattern "$SD_NAND_INSTALL" 'validated only for S9/AM1. Refusing this board family' 'sd_nand_install refuses non-S9/AM1 (am2/aarch64/s17/s19/s21) board families'
require_pattern "$SD_NAND_INSTALL" 'am2*' 'sd_nand_install board-family refusal case matches am2'
require_pattern "$SD_NAND_INSTALL" 'fw_setenv firmware' 'sd_nand_install prefers fw_setenv for the env flip'
# --- end sd_nand_install am1-only coverage -----------------------------------
require_pattern "$AM3_S19K_POST_IMAGE" 'rootfs uImage exceeds Amlogic rootfs window' 'am3-s19k post-image fails if rootfs exceeds rootfs window'
require_pattern "$AM3_S21_POST_IMAGE" 'rootfs uImage exceeds Amlogic rootfs window' 'am3-s21 post-image fails if rootfs exceeds rootfs window'
reject_pattern "$AM2_POST_IMAGE" '"version": null' 'am2 post-image does not emit null manifest version'
reject_pattern "$AM3_S19K_POST_IMAGE" '"version": null' 'am3-s19k post-image does not emit null manifest version'
reject_pattern "$AM3_S21_POST_IMAGE" '"version": null' 'am3-s21 post-image does not emit null manifest version'

# --- Amlogic S99upgrade raw-NAND exception is named and readback-verified ----
# am3-aml deliberately writes one recovery-state byte in mtd5 after health
# checks pass. That exception must stay explicit, Amlogic-only, immediately
# verified with read_recovery_flag(), and separate from the fw_setenv-only
# firstboot env clear.
require_pattern "$AMLOGIC_S99UPGRADE" 'AMLOGIC_RAW_NAND_RECOVERY_FLAG_EXCEPTION' 'amlogic S99upgrade names its only raw-NAND recovery-flag exception'
require_pattern "$AMLOGIC_S99UPGRADE" 'NEW=$(read_recovery_flag)' 'amlogic S99upgrade reads back the recovery flag after nandwrite'
require_pattern "$AMLOGIC_S99UPGRADE" '[ "$NEW" != "0x03" ]' 'amlogic S99upgrade fails when recovery-flag readback is not 0x03'
require_pattern "$AMLOGIC_S99UPGRADE" 'recovery flag readback = $NEW (expected 0x03)' 'amlogic S99upgrade reports the exact recovery-flag readback mismatch'
require_pattern "$AMLOGIC_S99UPGRADE" 'fw_setenv firstboot 0' 'amlogic S99upgrade keeps firstboot env clearing on fw_setenv, not raw NAND'
# --- end amlogic S99upgrade raw-NAND exception -------------------------------

# --- Zynq A/B boot-success contract (W8 parity: auto-rollback not defeated) --
# W8 PARITY-SIGNOFF flagged the A/B auto-rollback as "⚠️ defeated by S99": the
# zynq S99upgrade committed a fresh slot (cleared upgrade_stage) the instant the
# daemon bound its API socket — a daemon that binds /api/status then crash-loops
# (or never reaches a steady state) was committed as good, defeating the U-Boot
# auto-revert the inactive-slot write had armed. The fix adds (a) a REAL health
# check (/api/system/health must parse + report positive daemon.uptime_s) and
# (b) a sustained BOOT-SUCCESS WINDOW (the same dcentrald PID must survive
# MIN_HEALTHY_UPTIME_S). These guards pin the contract so it cannot silently
# regress to the bind-then-commit hole. Brick-safety: the new gates only ever
# make the commit STRICTER (a needless non-commit reverts to the known-good old
# slot; this is the lower-risk failure), and absent health evidence soft-passes rather than
# falsely reverting a good unit.
ZYNQ_S99UPGRADE='br2_external_dcentos/board/zynq/rootfs-overlay/etc/init.d/S99upgrade'
ZYNQ_S99VERIFY='br2_external_dcentos/board/zynq/rootfs-overlay/etc/init.d/S99verify'

require_pattern "$ZYNQ_S99UPGRADE" 'BOOT-SUCCESS CONTRACT' 'zynq S99upgrade documents the boot-success contract'
require_pattern "$ZYNQ_S99UPGRADE" '/api/system/health' 'zynq S99upgrade gates commit on the real health endpoint, not just a socket bind'
require_pattern "$ZYNQ_S99UPGRADE" 'daemon_real_health_verdict' 'zynq S99upgrade evaluates a three-state real-health verdict'
require_pattern "$ZYNQ_S99UPGRADE" 'MIN_HEALTHY_UPTIME_S' 'zynq S99upgrade enforces a sustained boot-success window'
require_pattern "$ZYNQ_S99UPGRADE" 'boot-success window' 'zynq S99upgrade re-confirms the daemon survives the window'
require_pattern "$ZYNQ_S99UPGRADE" 'died inside the boot-success window' 'zynq S99upgrade fails closed on a crash-loop inside the window'
require_pattern "$ZYNQ_S99UPGRADE" 'UPGRADE_COMMIT_MARKER' 'zynq S99upgrade records its commit decision for S99verify'
# Brick-safety: the unknown/absent-health path must SOFT-PASS, not revert a
# good unit. Pin that the three-state verdict keeps an explicit soft-pass arm.
require_pattern "$ZYNQ_S99UPGRADE" 'not blocking commit on its absence' 'zynq S99upgrade soft-passes when health evidence is merely absent (brick-safe)'
# The real-health gate must default-safe: window default present and tunable.
require_pattern "$ZYNQ_S99UPGRADE" 'DCENTOS_BOOT_SUCCESS_WINDOW_S' 'zynq S99upgrade boot-success window is tunable with a safe default'

# S99verify is a report-only consumer of S99upgrade's decision (single commit
# authority) and must never re-commit a slot the upgrader blocked.
require_pattern "$ZYNQ_S99VERIFY" 'UPGRADE_COMMIT_MARKER' 'zynq S99verify observes the S99upgrade commit-decision marker'
require_pattern "$ZYNQ_S99VERIFY" 'report-only proof consumer' 'zynq S99verify documents its non-mutating proof role'
require_pattern "$ZYNQ_S99VERIFY" 'S99upgrade blocked commit' 'zynq S99verify preserves upgrade_stage when S99upgrade blocked the slot'

# Both init scripts must stay POSIX/BusyBox-ash safe (no bashisms).
require_pattern "$ZYNQ_S99UPGRADE" '#!/bin/sh' 'zynq S99upgrade is POSIX/BusyBox shell'
require_pattern "$ZYNQ_S99VERIFY" '#!/bin/sh' 'zynq S99verify is POSIX/BusyBox shell'
reject_pattern "$ZYNQ_S99UPGRADE" '[[ ' 'zynq S99upgrade has no bash [[ ]] test'
# --- end zynq boot-success contract coverage ---------------------------------

# --- Zynq upgrade_stage CLEAR must be reliable on weak-ECC mtd4 (BUG 2) -------
# Live S9 (.100, 2026-06-05): S99upgrade logged "OK: firmware committed via
# fw_setenv" THEN "WARN: upgrade_stage still present after fw_setenv" — the clear
# silently failed, so a power-cycle auto-reverted the unit to BraiinsOS (install
# NOT permanent). Root cause was the WRITE/VERIFY logic, not the env geometry
# (offsets 0x0/0x20000 + ENV_SIZE 0x20000 are CRC-verified against a real mtd4
# dump). The old code ran TWO separate fw_setenv calls + a single readback; on
# weak-ECC pl35x-nand a first-call copy that reads back CRC-bad makes the SECOND
# call re-read the stale copy and re-persist upgrade_stage. The fix mirrors the
# proven sysupgrade write path: one ATOMIC `fw_setenv --script -` transaction
# clearing BOTH vars, RETRIED with a per-attempt fw_printenv verify, failing
# loudly (blocked marker -> U-Boot reverts to the known-good slot) if all
# attempts fail. NEVER raw nandwrite mtd4 (load-bearing
# ).
require_pattern "$ZYNQ_S99UPGRADE" 'fw_setenv --script -' 'zynq S99upgrade clears upgrade_stage via a single atomic fw_setenv --script transaction'
require_pattern "$ZYNQ_S99UPGRADE" 'FW_COMMIT_RETRIES' 'zynq S99upgrade retries the upgrade_stage clear (weak-ECC mtd4 resilience)'
require_pattern "$ZYNQ_S99UPGRADE" 'DCENTOS_FW_COMMIT_RETRIES' 'zynq S99upgrade retry count is tunable with a safe default'
require_pattern "$ZYNQ_S99UPGRADE" 'STILL_PRESENT' 'zynq S99upgrade verifies upgrade_stage is actually gone after each attempt'
require_pattern "$ZYNQ_S99UPGRADE" 'could NOT be cleared after' 'zynq S99upgrade fails loudly when every clear attempt fails'
# The clear must remain fw_setenv-only — never a raw NAND write on weak mtd4.
# (Match an actual command targeting the device, not the word in prose/warnings:
# the script legitimately documents "DO NOT raw-nandwrite" in comments.)
reject_pattern "$ZYNQ_S99UPGRADE" 'nandwrite /dev/mtd' 'zynq S99upgrade never raw-nandwrites mtd4'
reject_pattern "$ZYNQ_S99UPGRADE" 'flash_erase /dev/mtd' 'zynq S99upgrade never flash_erases mtd4'
# A blocked (unconfirmed) clear must NOT advertise a committed slot to S99verify.
require_pattern "$ZYNQ_S99UPGRADE" 'echo "blocked" > "$UPGRADE_COMMIT_MARKER"' 'zynq S99upgrade marks the slot blocked when the clear is unconfirmed (fail-safe)'
# fw_env.config geometry must stay the CRC-verified redundant pair.
ZYNQ_FW_ENV='br2_external_dcentos/board/zynq/rootfs-overlay/etc/fw_env.config'
require_pattern "$ZYNQ_FW_ENV" '/dev/mtd4   0x00000   0x20000   0x20000   1' 'zynq fw_env.config copy A geometry (CRC-verified)'
require_pattern "$ZYNQ_FW_ENV" '/dev/mtd4   0x20000   0x20000   0x20000   1' 'zynq fw_env.config copy B geometry (CRC-verified)'
# --- end zynq upgrade_stage-clear reliability coverage -----------------------

# --- CV1835 eMMC signed-sysupgrade + pre-extraction validation (CE-091/287) --
# The CV1835 eMMC safe-sysupgrade consumer was the outlier: it extracted the
# untrusted tar with ZERO pre-extraction validation, accepted any inner dir, and
# did no signature verify (only SHA sidecars inside the same attacker-controlled
# tar). These static pins mirror how this file already regression-pins the
# zynq/am2 consumers. NOTE: a full functional dry-run of safe_sysupgrade_cv_emmc.sh
# is not reachable host-side — it hard-gates on /etc/dcentos/board_target (and
# DCENT_CV1835_EMMC_PROVEN) with no offline seam, so the member-traversal-reject
# and unsigned-release-reject paths are pinned statically here + by the
# line-number ORDERING check in scripts/ci_offline_gates.sh's
# cv1835_emmc_signed_sysupgrade_check.
CV1835_SAFE_SU='scripts/safe_sysupgrade_cv_emmc.sh'
CV1835_POST_IMAGE='br2_external_dcentos/board/cvitek/cv1835-s19jpro/post-image.sh'
CV1835_POST_BUILD='br2_external_dcentos/board/cvitek/cv1835-s19jpro/post-build.sh'
OTA_SIGNATURE_RS='dcentrald/dcentrald-api/src/ota_signature.rs'

# Pre-extraction validation (member traversal/type + package-size ceiling).
require_pattern "$CV1835_SAFE_SU" 'validate_sysupgrade_tar_members "$UPGRADE_TAR"' 'cv1835 safe-sysupgrade validates tar members before extraction'
require_pattern "$CV1835_SAFE_SU" 'validate_sysupgrade_tar_preextract "$UPGRADE_TAR"' 'cv1835 safe-sysupgrade bounds package size/free-space before extraction'
require_pattern "$CV1835_SAFE_SU" 'Refusing before tar extraction' 'cv1835 safe-sysupgrade reports pre-extraction package refusal'
require_pattern "$CV1835_SAFE_SU" 'unsafe tar member path' 'cv1835 safe-sysupgrade rejects absolute/.. traversal tar member paths'
require_pattern "$CV1835_SAFE_SU" 'unsafe tar member type' 'cv1835 safe-sysupgrade rejects symlink/hardlink/device tar members'
require_pattern "$CV1835_SAFE_SU" 'dcentos-cv1835-s19jpro-sysupgrade' 'cv1835 safe-sysupgrade pins the produced top-level dir name'
# The old any-directory find|head-1 acceptance must be gone.
reject_pattern "$CV1835_SAFE_SU" 'find "$STAGE_DIR" -mindepth 1 -maxdepth 1 -type d | head -1' 'cv1835 safe-sysupgrade no longer accepts any first inner directory'

# Mandatory Ed25519 signature verification against the pinned release key.
require_pattern "$CV1835_SAFE_SU" 'openssl pkeyutl -verify -rawin -pubin' 'cv1835 safe-sysupgrade verifies MANIFEST.sig with openssl pkeyutl'
require_pattern "$CV1835_SAFE_SU" '/etc/dcentos/release_ed25519.pub' 'cv1835 safe-sysupgrade verifies against the pinned release key'
require_pattern "$CV1835_SAFE_SU" 'refusing unsigned CV1835 eMMC sysupgrade' 'cv1835 safe-sysupgrade refuses an unsigned package fail-closed'
require_pattern "$CV1835_SAFE_SU" '&& ! is_release_status "$MANIFEST_STATUS"' 'cv1835 safe-sysupgrade only accepts the unsigned lab override for a non-release status'
require_pattern "$CV1835_SAFE_SU" 'verify_manifest_payload_sha "$NEW_KERNEL" "uImage"' 'cv1835 consumer binds uImage to the signed manifest digest'
require_pattern "$CV1835_SAFE_SU" 'verify_manifest_payload_sha "$NEW_ROOTFS" "rootfs.gz"' 'cv1835 consumer binds rootfs.gz to the signed manifest digest'

# Producer: signed package emission + rootfs pubkey staging.
require_pattern "$CV1835_POST_IMAGE" 'dcent_stage_release_key' 'cv1835 post-image stages release_ed25519.pub via the shared signing helper'
require_pattern "$CV1835_POST_IMAGE" 'dcent_sign_sysupgrade_manifest' 'cv1835 post-image signs MANIFEST.json (emits MANIFEST.sig)'
require_pattern "$CV1835_POST_IMAGE" 'required deployable rootfs missing' 'cv1835 producer fails when consumer-required rootfs.gz is unavailable'
require_pattern "$CV1835_POST_IMAGE" 'required deployable kernel missing' 'cv1835 producer fails when consumer-required uImage is unavailable'
require_pattern "$CV1835_POST_IMAGE" '"path": "dcentos-${BOARD_NAME}-sysupgrade/uImage"' 'cv1835 producer declares the consumer-required uImage in the canonical payload registry'
require_pattern "$CV1835_POST_IMAGE" '"path": "dcentos-${BOARD_NAME}-sysupgrade/rootfs.gz"' 'cv1835 producer declares the consumer-required rootfs.gz in the canonical payload registry'
require_pattern "$OTA_SIGNATURE_RS" 'accepted_leaves: &["kernel", "uImage"]' 'public OTA registry maps the canonical kernel kind to CV uImage'
require_pattern "$OTA_SIGNATURE_RS" 'accepted_leaves: &["root", "rootfs.gz"]' 'public OTA registry maps the canonical rootfs kind to CV rootfs.gz'
reject_pattern "$CV1835_POST_IMAGE" 'rootfs.ext2' 'cv1835 producer does not advertise the unsupported ext2 fallback'
reject_pattern "$CV1835_POST_IMAGE" 'sysupgrade will be rootfs-only' 'cv1835 producer does not emit an unusable rootfs-only package'
require_pattern "$CV1835_POST_BUILD" 'etc/dcentos/release_ed25519.pub' 'cv1835 post-build stages the pinned release_ed25519.pub into the rootfs'
# --- end CV1835 eMMC signed-sysupgrade coverage ------------------------------

# =============================================================================
# Bucket-A P0 blocker coverage (CE-105 / CE-056 / CE-153 / CE-408 / CE-341 /
# CE-126 / CE-374 / CE-382). Additive, fail-closed pins so the new guards
# cannot silently rot. All files are already offline/static-checkable here.
# =============================================================================

# --- CE-105: sd_nand_install ubi_replace proves inactive-slot targeting -------
require_pattern "$SD_NAND_INSTALL" 'IS the active firmware slot' 'sd_nand_install ubi_replace refuses to overwrite the active firmware slot'
require_pattern "$SD_NAND_INSTALL" 'prove inactive-slot targeting' 'sd_nand_install ubi_replace proves inactive-slot targeting before writing'
require_pattern "$SD_NAND_INSTALL" 'fw_printenv -n firmware' 'sd_nand_install reads the active firmware slot via fw_printenv'
require_pattern "$SD_NAND_INSTALL" 'refusing NAND write' 'sd_nand_install refuses a NAND write when the boot source is unknown'

# --- CE-056: package_sysupgrade fail-closes am2-s19j S9 placeholder -----------
require_pattern "$PACKAGE" 'DCENT_ALLOW_AM2_S9_PLACEHOLDER' 'package_sysupgrade fail-closes am2-s19j S9-placeholder unless explicit lab override'
require_pattern "$PACKAGE" 'Refusing to package am2-s19j with an S9 placeholder' 'package_sysupgrade names the am2-s19j S9-placeholder brick refusal'
require_pattern "$PACKAGE" 'PLACEHOLDER-DO-NOT-FLASH' 'package_sysupgrade forces non-flashable naming for am2-s19j placeholder builds'

# --- CE-153: host switch_firmware.py aligned to the guarded overlay copy ------
HOST_SWITCH_FW='scripts/switch_firmware.py'
require_pattern "$HOST_SWITCH_FW" '--i-understand-this-is-not-fw-setenv' 'host switch_firmware.py requires the not-fw-setenv acknowledgement'
require_pattern "$HOST_SWITCH_FW" 'REFUSING: switch_firmware.py is DEPRECATED' 'host switch_firmware.py refuses to run without the ack'
require_pattern "$HOST_SWITCH_FW" 'DEPRECATED FOR THE OTA/SYSUPGRADE WRITE PATH' 'host switch_firmware.py carries the deprecation banner'
require_pattern "$HOST_SWITCH_FW" 'distinct flags' 'host switch_firmware.py writes redundant env copies with distinct flags'

# --- CE-408: revert_to_stock_s19_am2 requires hash-bound stock provenance -----
require_pattern "$S19_AM2_REVERT" 'DCENT_STOCK_REVERT_ALLOW_UNVERIFIED' 'revert_to_stock_s19_am2 gates the unverified path behind an explicit lab override'
require_pattern "$S19_AM2_REVERT" 'refusing stock revert with an unauthenticated runtime download' 'revert_to_stock_s19_am2 refuses a no-image unauthenticated download by default'
require_pattern "$S19_AM2_REVERT" 'refusing stock revert without expected SHA-256' 'revert_to_stock_s19_am2 refuses a supplied image with no expected SHA by default'

# --- CE-341: build_in_docker labels/gates the canonical release alias ---------
require_pattern "$BUILD_DOCKER" 'LAB-UNSIGNED-NOT-FOR-RELEASE' 'build_in_docker labels a non-release-grade canonical alias as lab-unsigned'
require_pattern "$BUILD_DOCKER" 'signature_trust=' 'build_in_docker records signature_trust in the release sidecar'
require_pattern "$BUILD_DOCKER" 'proof_scope=' 'build_in_docker records proof_scope in the release sidecar'
require_pattern "$BUILD_DOCKER" 'RELEASE_GRADE=1' 'build_in_docker computes a release-grade verdict for the canonical alias'

# --- CE-126: legacy web-installer quarantine + no world-open CGMiner API ------
# Pin the SOURCE copy (the RC baseline). The public-releases/ mirror is a
# regenerated artifact (tools/public-release/refresh-all.sh) — it inherits this
# fix at publish time and must NOT be hand-edited here (the plan excludes
# public-releases from the RC cut).
WEB_INSTALLER='scripts/build_web_installer.sh'
for web_inst in "$WEB_INSTALLER"; do
    if [ -f "$web_inst" ]; then
        require_pattern "$web_inst" 'DCENT_ALLOW_LEGACY_WEB_INSTALLER' "web-installer [$web_inst] is quarantined behind an explicit opt-in"
        require_pattern "$web_inst" 'quarantined unsafe research tool' "web-installer [$web_inst] refuses to run by default"
        reject_pattern "$web_inst" 'W:0/0' "web-installer [$web_inst] emits no world-open CGMiner api-allow"
        reject_pattern "$web_inst" 'W:0\/0' "web-installer [$web_inst] emits no world-open CGMiner api-allow (sed-escaped form)"
    else
        pass "web-installer copy not present in this tree (skipped): $web_inst"
    fi
done

# --- CE-374: NAND backup planners bind restore-proof to the exact unit --------
for nand_plan in scripts/am1_nand_backup_plan.sh scripts/am2_nand_backup_plan.sh; do
    require_pattern "$nand_plan" '--expect-mac' "$(basename "$nand_plan") accepts an --expect-mac identity arg"
    require_pattern "$nand_plan" '--expect-hwid' "$(basename "$nand_plan") accepts an --expect-hwid identity arg"
    require_pattern "$nand_plan" 'restore_verified_identity_matched' "$(basename "$nand_plan") sets restore-proof OK only on identity match"
    require_pattern "$nand_plan" 'restore_matched_mac' "$(basename "$nand_plan") emits the matched MAC in JSON"
    require_pattern "$nand_plan" 'identity_mismatch' "$(basename "$nand_plan") fails closed on identity mismatch"
    require_pattern "$nand_plan" 'advisory, not identity-bound' "$(basename "$nand_plan") downgrades marker-only proof wording"
done
if nand_backup_identity_selftest; then
    pass "CE-374 NAND backup planners bind restore-proof to the exact unit (identity match required for plan_ready)"
else
    fail "CE-374 NAND backup planner identity-binding selftest failed"
fi

# --- CE-382: SD writer fails closed on a post-write verify failure ------------
WRITE_SD='scripts/write_sd_card.sh'
require_pattern "$WRITE_SD" 'VERIFY_FAILED=1' 'write_sd_card marks a post-write verification failure'
require_pattern "$WRITE_SD" 'Post-write verification FAILED' 'write_sd_card refuses to declare the card ready after a failed verify'
require_pattern "$WRITE_SD" 'BOOT.BIN: NOT FOUND ON CARD' 'write_sd_card fails closed when a copied BOOT.BIN is absent on the card'
# The fail-closed guard (error/exit) MUST sit BEFORE the success banner so the
# "=== SD Card Ready ===" line can never print on a failed verify.
if awk '
    /Post-write verification FAILED/ { seen_guard = 1 }
    /=== SD Card Ready ===/ { if (!seen_guard) { bad = 1 } }
    END { exit (bad ? 1 : 0) }
' "$WRITE_SD"; then
    pass 'write_sd_card: fail-closed verify guard precedes the SD Card Ready banner'
else
    fail 'write_sd_card: SD Card Ready banner can print before the fail-closed verify guard'
fi

if signing_policy_selftest; then
    pass "signing policy selftest fails closed for release and accepts explicit lab override"
else
    fail "signing policy selftest failed"
fi
if zynq_payload_window_selftest; then
    pass "zynq package-only selftest rejects oversized rootfs payloads"
else
    fail "zynq package-only selftest failed"
fi
if release_image_hardening_coupling_selftest; then
    pass "CE-183 release-status packaging fails closed without release-image hardening and no-ops for lab status"
else
    fail "CE-183 release-status hardening coupling selftest failed"
fi

for phrase in \
    'Package + upload + flash' \
    'Upload and flash' \
    'Ready to flash' \
    'during flash' \
    'Upload verified.' \
    'Refusing to flash' \
    '=== SUCCESS ===' \
    'DCENTos is running on'
do
    reject_pattern "$PACKAGE" "$phrase" "package_sysupgrade avoids overclaim phrase: $phrase"
done

if [ "$failures" -ne 0 ]; then
    printf '\nSysupgrade packaging static checks failed: %s failure(s)\n' "$failures" >&2
    exit 1
fi

printf '\nSysupgrade packaging static checks passed.\n'
