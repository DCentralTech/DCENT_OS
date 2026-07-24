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

require_exact_line() {
    file=$1
    line=$2
    label=$3

    if grep -Fqx -- "$line" "$file" >/dev/null 2>&1; then
        pass "$label"
    else
        fail "$label: missing exact line '$line' in $file"
    fi
}

require_ordered_patterns() {
    file=$1
    first=$2
    second=$3
    label=$4

    if awk -v first="$first" -v second="$second" '
        first_line == 0 && index($0, first) { first_line = NR }
        first_line > 0 && index($0, second) { second_line = NR; exit }
        END { exit (first_line > 0 && second_line > first_line) ? 0 : 1 }
    ' "$file"; then
        pass "$label"
    else
        fail "$label: '$first' must precede '$second' in $file"
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
    if [ "$status" = "lab_unsigned" ]; then
        manifest_profile=dcentos.sysupgrade-unsigned-lab/v1
    else
        manifest_profile=dcentos.sysupgrade-authority/v1
    fi

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
  "schema": 1,
  "manifest_profile": "$manifest_profile",
  "product": "DCENT_OS",
  "package_type": "sysupgrade",
  "installable": true,
  "artifact_maturity": "experimental",
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
  "schema": 1,
  "manifest_profile": "dcentos.sysupgrade-unsigned-lab/v1",
  "product": "DCENT_OS",
  "package_type": "sysupgrade",
  "installable": true,
  "artifact_maturity": "experimental",
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

run_zynq_target_payload_authority() {
    authority_target_script=$1
    authority_fixture_root=$2
    {
        sed -n '/^verify_sha256() {/,/^validate_sysupgrade_tar_members() {/p' "$authority_target_script" | sed '$d'
        cat <<'SELFTEST'
validate_extracted_package_leaves &&
validate_manifest_payload_binding kernel kernel "$PACKAGE_KERNEL" &&
validate_manifest_payload_binding rootfs root "$ROOTFS" &&
validate_manifest_payload_binding metadata METADATA "$PACKAGE_SUBDIR/METADATA" &&
validate_manifest_payload_binding verification_key release_ed25519.pub "$PACKAGE_RELEASE_KEY"
SELFTEST
    } | PACKAGE_SUBDIR="$authority_fixture_root/sysupgrade-test" \
        PACKAGE_SUBDIR_NAME=sysupgrade-test \
        PACKAGE_MANIFEST="$authority_fixture_root/sysupgrade-test/MANIFEST.json" \
        PACKAGE_KERNEL="$authority_fixture_root/sysupgrade-test/kernel" \
        ROOTFS="$authority_fixture_root/sysupgrade-test/root" \
        PACKAGE_RELEASE_KEY="$authority_fixture_root/sysupgrade-test/release_ed25519.pub" \
        sh
}

write_zynq_target_authority_manifest() {
    authority_manifest_dir=$1
    authority_kernel_path=$2
    authority_kernel_size_value=$3
    authority_kernel_sha_value=$4
    authority_root_size=$(wc -c < "$authority_manifest_dir/root" | tr -d '[:space:]')
    authority_metadata_size=$(wc -c < "$authority_manifest_dir/METADATA" | tr -d '[:space:]')
    authority_key_size=$(wc -c < "$authority_manifest_dir/release_ed25519.pub" | tr -d '[:space:]')
    authority_root_sha=$(sha256sum "$authority_manifest_dir/root" | awk '{print $1}')
    authority_metadata_sha=$(sha256sum "$authority_manifest_dir/METADATA" | awk '{print $1}')
    authority_key_sha=$(sha256sum "$authority_manifest_dir/release_ed25519.pub" | awk '{print $1}')
    cat > "$authority_manifest_dir/MANIFEST.json" <<EOF
{
  "payloads": {
    "kernel": { "path": "$authority_kernel_path", "size": $authority_kernel_size_value, "sha256": "$authority_kernel_sha_value" },
    "rootfs": { "path": "sysupgrade-test/root", "size": $authority_root_size, "sha256": "$authority_root_sha" },
    "metadata": { "path": "sysupgrade-test/METADATA", "size": $authority_metadata_size, "sha256": "$authority_metadata_sha" },
    "verification_key": { "path": "sysupgrade-test/release_ed25519.pub", "size": $authority_key_size, "sha256": "$authority_key_sha" }
  }
}
EOF
}

zynq_target_payload_authority_selftest() {
    tmpdir=$(mktemp -d 2>/dev/null || echo "/tmp/dcentos-zynq-authority-selftest.$$")
    rm -rf "$tmpdir"
    fixture_dir="$tmpdir/sysupgrade-test"
    mkdir -p "$fixture_dir"
    printf 'kernel-bytes' > "$fixture_dir/kernel"
    printf 'rootfs-bytes' > "$fixture_dir/root"
    printf 'board=test\n' > "$fixture_dir/METADATA"
    printf 'test-release-key' > "$fixture_dir/release_ed25519.pub"
    kernel_size=$(wc -c < "$fixture_dir/kernel" | tr -d '[:space:]')
    kernel_sha=$(sha256sum "$fixture_dir/kernel" | awk '{print $1}')
    write_zynq_target_authority_manifest "$fixture_dir" sysupgrade-test/kernel "$kernel_size" "$kernel_sha"

    for target_script in \
        br2_external_dcentos/board/zynq/rootfs-overlay/usr/sbin/sysupgrade \
        br2_external_dcentos/board/zynq/am2-s19jpro/rootfs-overlay/usr/sbin/sysupgrade \
        br2_external_dcentos/board/zynq/am2-s19pro/rootfs-overlay/usr/sbin/sysupgrade \
        br2_external_dcentos/board/zynq/am2-s17pro/rootfs-overlay/usr/sbin/sysupgrade; do
        run_zynq_target_payload_authority "$target_script" "$tmpdir" >/dev/null 2>&1 || {
            rm -rf "$tmpdir"
            return 1
        }
    done

    printf 'unknown' > "$fixture_dir/unknown.bin"
    if run_zynq_target_payload_authority \
        br2_external_dcentos/board/zynq/rootfs-overlay/usr/sbin/sysupgrade \
        "$tmpdir" >/dev/null 2>&1; then
        rm -rf "$tmpdir"
        return 1
    fi
    rm -f "$fixture_dir/unknown.bin"

    mkdir -p "$fixture_dir/nested"
    printf 'nested' > "$fixture_dir/nested/payload"
    if run_zynq_target_payload_authority \
        br2_external_dcentos/board/zynq/rootfs-overlay/usr/sbin/sysupgrade \
        "$tmpdir" >/dev/null 2>&1; then
        rm -rf "$tmpdir"
        return 1
    fi
    rm -rf "$fixture_dir/nested"

    for invalid_case in path string-size zero-size uppercase-sha wrong-sha; do
        case "$invalid_case" in
            path)
                write_zynq_target_authority_manifest "$fixture_dir" sysupgrade-test/root "$kernel_size" "$kernel_sha"
                ;;
            string-size)
                write_zynq_target_authority_manifest "$fixture_dir" sysupgrade-test/kernel '"'"$kernel_size"'"' "$kernel_sha"
                ;;
            zero-size)
                write_zynq_target_authority_manifest "$fixture_dir" sysupgrade-test/kernel 0 "$kernel_sha"
                ;;
            uppercase-sha)
                uppercase_sha=$(printf '%s' "$kernel_sha" | tr 'a-f' 'A-F')
                write_zynq_target_authority_manifest "$fixture_dir" sysupgrade-test/kernel "$kernel_size" "$uppercase_sha"
                ;;
            wrong-sha)
                write_zynq_target_authority_manifest "$fixture_dir" sysupgrade-test/kernel "$kernel_size" '0000000000000000000000000000000000000000000000000000000000000000'
                ;;
        esac
        if run_zynq_target_payload_authority \
            br2_external_dcentos/board/zynq/rootfs-overlay/usr/sbin/sysupgrade \
            "$tmpdir" >/dev/null 2>&1; then
            rm -rf "$tmpdir"
            return 1
        fi
    done

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

toolbox_install_contract_selftest() {
    helper='scripts/lib/sysupgrade_package_common.sh'

    if ! BOARD_NAME=am1-s9 sh -c \
        '. "$1"; dcent_require_toolbox_install_contract "$2" "$3"' \
        sh "$helper" \
        'dcent install <ip> -f dcentos-sysupgrade-118.tar' \
        target_sysupgrade >/dev/null 2>&1; then
        return 1
    fi
    if ! BOARD_NAME=am2-s19j sh -c \
        '. "$1"; dcent_require_toolbox_install_contract "$2" "$3"' \
        sh "$helper" \
        'dcent install <ip> -f dcentos-sysupgrade-am2-s19jpro.tar --artifact-dir <restore_verified_dir> --accept-am2-persistent-lab --i-have-recovery' \
        target_sysupgrade >/dev/null 2>&1; then
        return 1
    fi
    if ! BOARD_NAME=am3-s21 sh -c \
        '. "$1"; dcent_require_toolbox_install_contract "$2" "$3"' \
        sh "$helper" \
        'dcent install <ip> -f dcentos-sysupgrade-am3-s21.tar --artifact-dir <restore_verified_dir>' \
        host_driven_rootfs_window_lab >/dev/null 2>&1; then
        return 1
    fi

    for bad_command in \
        'dcent install <ip> -f dcentos-sysupgrade-am3-s21.tar' \
        'dcent install <ip> -f dcentos-sysupgrade-am3-s21.tar --artifact-dir <restore_verified_dir> --yes' \
        'dcent install <ip> -f dcentos-sysupgrade-am3-s21.tar --artifact-dir <restore_verified_dir> --accept-vnish-aml-rootfs-window'
    do
        if BOARD_NAME=am3-s21 sh -c \
            '. "$1"; dcent_require_toolbox_install_contract "$2" "$3"' \
            sh "$helper" "$bad_command" \
            host_driven_rootfs_window_lab >/dev/null 2>&1; then
            return 1
        fi
    done

    for bad_command in \
        'dcent install <ip> -f dcentos-sysupgrade-am2-s19jpro.tar --accept-am2-persistent-lab --i-have-recovery' \
        'dcent install <ip> -f dcentos-sysupgrade-am2-s19jpro.tar --artifact-dir <restore_verified_dir> --i-have-recovery' \
        'dcent install <ip> -f dcentos-sysupgrade-am2-s19jpro.tar --artifact-dir <restore_verified_dir> --accept-am2-persistent-lab'
    do
        if BOARD_NAME=am2-s19j sh -c \
            '. "$1"; dcent_require_toolbox_install_contract "$2" "$3"' \
            sh "$helper" "$bad_command" target_sysupgrade >/dev/null 2>&1; then
            return 1
        fi
    done

    return 0
}

# CE-374: exercise AM2's strict plan validator here. AM1's stricter Python
# evidence suite is wired into the Python safety gate and Zynq integration gate.
nand_backup_identity_selftest() {
    python_bin=$(command -v python3 || command -v python || true)
    [ -n "$python_bin" ] || return 0
    "$python_bin" scripts/validate_am2_nand_backup_plan.py --self-test >/dev/null
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
ARCHIVE_ADMISSION='scripts/lib/sysupgrade_archive_admission.sh'
ZYNQ_GEOMETRY_HELPER='scripts/lib/sysupgrade_zynq_geometry.sh'
MANIFEST_JSON='scripts/lib/sysupgrade_manifest_json.py'
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
S9_POST_BUILD='br2_external_dcentos/board/zynq/post-build.sh'
AM2_POST_BUILD='br2_external_dcentos/board/zynq/am2-s19jpro/post-build.sh'
AM2_S19PRO_POST_BUILD='br2_external_dcentos/board/zynq/am2-s19pro/post-build.sh'
AM2_S17PRO_POST_BUILD='br2_external_dcentos/board/zynq/am2-s17pro/post-build.sh'
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

require_pattern "$ARCHIVE_ADMISSION" 'DCENT_SYSUPGRADE_ARCHIVE_MAX_MEMBERS=32' 'archive admission caps sysupgrade envelopes at 32 members'
require_pattern "$ARCHIVE_ADMISSION" 'duplicate archive member' 'archive admission rejects duplicate exact members'
require_pattern "$ARCHIVE_ADMISSION" 'duplicate logical archive member' 'archive admission rejects duplicate logical members'
require_pattern "$ARCHIVE_ADMISSION" 'expected exactly one canonical %s/ directory member' 'archive admission requires one exact target prefix directory'
require_pattern "$ARCHIVE_ADMISSION" 'nested or empty member path' 'archive admission rejects nested member paths'
require_pattern "$ARCHIVE_ADMISSION" 'unsafe type %s for member %s' 'archive admission rejects symlink, hardlink, and special members'
require_pattern "$ARCHIVE_ADMISSION" 'unknown member leaf' 'archive admission uses an explicit leaf allowlist'
require_pattern "$ARCHIVE_ADMISSION" 'archive payload is not declared exactly once in MANIFEST.json' 'archive admission rejects unmanifested optional payload leaves'
require_pattern "$ARCHIVE_ADMISSION" 'fpga_bitstream.bit' 'archive admission preserves the manifest-bound optional FPGA bitstream'
require_pattern "$ARCHIVE_ADMISSION" "grep -F '\\'" 'archive admission rejects all JSON escape aliases before byte-oriented authority readers'
require_pattern "$MANIFEST_JSON" 'object_pairs_hook=_unique_object' 'semantic manifest admission rejects decoded duplicate keys at every object depth'
require_pattern "$MANIFEST_JSON" 'compare-version' 'semantic manifest authority exposes the deterministic version comparator'

require_pattern "$PACKAGE" 'infer_package_version' 'packager infers package version'
require_pattern "$PACKAGE" 'Package version is required for fail-closed release manifests' 'packager fails closed if version cannot be set'
require_pattern "$PACKAGE" '"version": "$PACKAGE_VERSION"' 'packager writes a non-null manifest version'
reject_pattern "$PACKAGE" '"version": null' 'packager does not emit null manifest version'
require_pattern "$PRE_FLASH" 'AM3 kernel/root uImage magic valid' 'package-only validator has explicit AM3 uImage profile'
require_pattern "$PRE_FLASH" 'AM3 root payload fits am3 rootfs window' 'package-only validator keeps AM3 rootfs window gate'
require_pattern "$PRE_FLASH" 'squashfs-style root payload magic valid' 'package-only validator has squashfs-style board profile'
require_pattern "$PRE_FLASH" 'AM3 uImage/rootfs-window checks skipped for squashfs-style' 'package-only validator does not label S9/am2 packages as AM3 uImage'
require_pattern "$PRE_FLASH" 'sysupgrade_zynq_geometry.sh' 'package-only validator sources canonical real-target Zynq geometry'
require_pattern "$ZYNQ_GEOMETRY_HELPER" 'ZYNQ_UBI_LEB_SIZE_BYTES=126976' 'canonical geometry pins captured Xilinx UBI LEB bytes'
require_pattern "$ZYNQ_GEOMETRY_HELPER" 'AM2_ZYNQ_KERNEL_PACKAGE_LEBS=23' 'canonical geometry separates package fit from runtime layout tolerance'
require_pattern "$ZYNQ_GEOMETRY_HELPER" 'AM2_ZYNQ_ROOTFS_PACKAGE_LEBS=179' 'canonical geometry pins the live AM2 rootfs window'
require_pattern "$ZYNQ_GEOMETRY_HELPER" 'AM2_ZYNQ_KERNEL_TAR_BOUND_BYTES=' 'canonical geometry exposes a separate pre-extraction bound'
require_pattern "$PRE_FLASH" 'assert_payload_fits_window "$board kernel" "$kernel_size" "$ZYNQ_KERNEL_MAX_BYTES" "zynq kernel window"' 'package-only validator rejects oversized Zynq kernels'
require_pattern "$PRE_FLASH" 'assert_payload_fits_window "$board root" "$root_size" "$ZYNQ_ROOTFS_MAX_BYTES" "zynq rootfs window"' 'package-only validator rejects oversized Zynq rootfs payloads'
require_pattern "$PRE_FLASH" 'am1-s9|am2-s19j|am2-s19jpro|am2-s19pro|am2-s17p)' 'package-only validator covers all admitted Zynq board identities'
require_pattern "$PRE_FLASH" 'validate_package_only "$TARBALL" "am1-s9"' 'am1-s9 live pre-flash validates the package before declaring backup-floor success (CE-352)'
require_pattern "$BUILD_DOCKER" 'am3-s19kpro|am3-s21|am3-s21pro|am3-s21xp|am3-s19jpro-aml|am3-t21|am2-s19jpro|am2-s19pro|am2-s17pro)' 'build_in_docker package-validates every Amlogic and AM2 tarball lane when present'
require_pattern "$SYSUPGRADE_COMMON" 'dcent_require_toolbox_install_contract "$install_command" "$install_mode"' 'manifest writer validates its operator install command'
require_pattern "$SYSUPGRADE_COMMON" 'must preserve interactive confirmation' 'package metadata never pre-acknowledges --yes'
require_pattern "$SYSUPGRADE_COMMON" 'must not pre-acknowledge the VNish-source safety gate' 'generic Amlogic package metadata does not pre-acknowledge a source-specific route'
require_pattern "$BUILD_DOCKER" '--artifact-dir <restore_verified_dir> --plan' 'Amlogic build handoff supplies the restore-verified artifact directory'
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
    require_exact_line "$zynq_sysupgrade" 'manifest_key_count() {' "Zynq sysupgrade $(basename "$zynq_sysupgrade") defines the manifest cardinality helper under its callable name"
    require_pattern "$zynq_sysupgrade" 'dcentos.sysupgrade-authority/v1' "Zynq sysupgrade $(basename "$zynq_sysupgrade") requires the versioned mutation-authority profile"
    require_pattern "$zynq_sysupgrade" 'dcentos.sysupgrade-unsigned-lab/v1)' "Zynq sysupgrade $(basename "$zynq_sysupgrade") recognizes the explicit unsigned lab profile"
    require_pattern "$zynq_sysupgrade" 'Package authority-v1 forbids status=lab_unsigned' "Zynq sysupgrade $(basename "$zynq_sysupgrade") rejects the signed/unsigned status contradiction"
    require_pattern "$zynq_sysupgrade" "exactly one 'status' authority field" "Zynq sysupgrade $(basename "$zynq_sysupgrade") requires one unambiguous status claim"
    require_pattern "$zynq_sysupgrade" 'status must not contain surrounding whitespace' "Zynq sysupgrade $(basename "$zynq_sysupgrade") rejects whitespace-padded status"
    require_pattern "$zynq_sysupgrade" 'version must not contain surrounding whitespace' "Zynq sysupgrade $(basename "$zynq_sysupgrade") rejects whitespace-padded version"
    require_pattern "$zynq_sysupgrade" 'Package unsigned-lab/v1 requires exactly one status=lab_unsigned field' "Zynq sysupgrade $(basename "$zynq_sysupgrade") pins exact unsigned lab status"
    require_pattern "$zynq_sysupgrade" 'Package unsigned-lab/v1 forbids MANIFEST.sig' "Zynq sysupgrade $(basename "$zynq_sysupgrade") rejects signatures in the unsigned lab profile"
    require_pattern "$zynq_sysupgrade" 'Package unsigned-lab/v1 forbids release_ed25519.pub' "Zynq sysupgrade $(basename "$zynq_sysupgrade") rejects release keys in the unsigned lab profile"
    require_pattern "$zynq_sysupgrade" 'Package unsigned-lab/v1 forbids a verification_key payload' "Zynq sysupgrade $(basename "$zynq_sysupgrade") rejects verification authority in the unsigned lab profile"
    require_pattern "$zynq_sysupgrade" 'for payload_key in kernel rootfs metadata' "Zynq sysupgrade $(basename "$zynq_sysupgrade") bounds named payload duplicates"
    require_pattern "$zynq_sysupgrade" 'validate_extracted_package_leaves || exit 1' "Zynq sysupgrade $(basename "$zynq_sysupgrade") revalidates the extracted flat leaf envelope"
    require_pattern "$zynq_sysupgrade" 'Extracted package contains nested payload entry' "Zynq sysupgrade $(basename "$zynq_sysupgrade") rejects post-extraction nested entries"
    require_pattern "$zynq_sysupgrade" 'Extracted package contains unknown payload leaf' "Zynq sysupgrade $(basename "$zynq_sysupgrade") rejects post-extraction unknown leaves"
    require_pattern "$zynq_sysupgrade" "for _payload_field in path size sha256" "Zynq sysupgrade $(basename "$zynq_sysupgrade") requires one complete payload declaration tuple"
    require_pattern "$zynq_sysupgrade" "payload path must be exactly" "Zynq sysupgrade $(basename "$zynq_sysupgrade") binds payload kinds to canonical paths"
    require_pattern "$zynq_sysupgrade" 'payload size must be a positive JSON integer' "Zynq sysupgrade $(basename "$zynq_sysupgrade") requires typed positive payload sizes"
    require_pattern "$zynq_sysupgrade" 'payload size does not match signed manifest' "Zynq sysupgrade $(basename "$zynq_sysupgrade") binds signed payload sizes to regular files"
    require_pattern "$zynq_sysupgrade" 'sha256 must be exactly 64 lowercase hexadecimal characters' "Zynq sysupgrade $(basename "$zynq_sysupgrade") requires canonical signed payload digests"
    require_pattern "$zynq_sysupgrade" 'payload bytes do not match signed sha256' "Zynq sysupgrade $(basename "$zynq_sysupgrade") hashes the actual extracted payload bytes"
    require_pattern "$zynq_sysupgrade" 'contains fpga_bitstream.bit without exactly one signed bitstream declaration' "Zynq sysupgrade $(basename "$zynq_sysupgrade") rejects an undeclared optional FPGA payload"
    require_pattern "$zynq_sysupgrade" 'manifest declares bitstream but fpga_bitstream.bit is absent' "Zynq sysupgrade $(basename "$zynq_sysupgrade") rejects an absent declared FPGA payload"
    require_pattern "$zynq_sysupgrade" 'validate_manifest_payload_binding verification_key release_ed25519.pub' "Zynq sysupgrade $(basename "$zynq_sysupgrade") binds the signed verification key payload"
    require_pattern "$zynq_sysupgrade" 'validate_manifest_payload_binding bitstream fpga_bitstream.bit' "Zynq sysupgrade $(basename "$zynq_sysupgrade") binds the optional FPGA payload when declared"
    require_pattern "$zynq_sysupgrade" 'manifest_boolean_field "$PACKAGE_MANIFEST" installable' "Zynq sysupgrade $(basename "$zynq_sysupgrade") distinguishes JSON boolean installability from strings"
    require_pattern "$zynq_sysupgrade" 'Package manifest must explicitly declare installable=true' "Zynq sysupgrade $(basename "$zynq_sysupgrade") requires signed install authorization"
    require_pattern "$zynq_sysupgrade" 'Package manifest artifact_maturity must match this target' "Zynq sysupgrade $(basename "$zynq_sysupgrade") requires policy-aligned artifact maturity"
    require_pattern "$zynq_sysupgrade" 'Package manifest board and board_target must be present and identical' "Zynq sysupgrade $(basename "$zynq_sysupgrade") binds signed target aliases"
    require_pattern "$zynq_sysupgrade" 'does not match signed target' "Zynq sysupgrade $(basename "$zynq_sysupgrade") binds package directory to signed target"
    require_pattern "$zynq_sysupgrade" 'for unsupported_chain_key in ota_intermediate_cert ota_revoked_intermediates' "Zynq sysupgrade $(basename "$zynq_sysupgrade") restricts authority-v1 to direct release-root signatures"
    require_pattern "$zynq_sysupgrade" 'certificate validity has no trusted-time authority on Zynq' "Zynq sysupgrade $(basename "$zynq_sysupgrade") does not trust an unauthenticated recovery clock"
    require_pattern "$zynq_sysupgrade" 'enforce_sysupgrade_version_floor || exit 1' "Zynq sysupgrade $(basename "$zynq_sysupgrade") enforces downgrade floor before payload writes"
    require_pattern "$zynq_sysupgrade" 'DCENT_SYSUPGRADE_VERSION_PATH' "Zynq sysupgrade $(basename "$zynq_sysupgrade") exposes offline version-file seam"
    require_pattern "$zynq_sysupgrade" 'DCENT_ALLOW_DOWNGRADE' "Zynq sysupgrade $(basename "$zynq_sysupgrade") requires explicit downgrade override"
    require_pattern "$zynq_sysupgrade" '[ "$ALLOW_DOWNGRADE" = "1" ] && ! is_release_status "$PACKAGE_STATUS"' "Zynq sysupgrade $(basename "$zynq_sysupgrade") allows downgrade override only for non-release packages"
    require_pattern "$zynq_sysupgrade" 'Downgrade refused: package version' "Zynq sysupgrade $(basename "$zynq_sysupgrade") reports signed-package downgrade refusal"
    require_pattern "$zynq_sysupgrade" 'command -v jsonfilter' "Zynq sysupgrade $(basename "$zynq_sysupgrade") prefers jsonfilter for manifest parsing"
    require_pattern "$zynq_sysupgrade" 'command -v python3' "Zynq sysupgrade $(basename "$zynq_sysupgrade") checks python3 before manifest parsing"
    require_pattern "$zynq_sysupgrade" 'Error: manifest_field needs jsonfilter or python3' "Zynq sysupgrade $(basename "$zynq_sysupgrade") fails clearly when manifest parser is unavailable"
    require_pattern "$zynq_sysupgrade" 'ARCHIVE_ADMISSION_HELPER="/usr/libexec/dcentos/sysupgrade-archive-admission.sh"' "Zynq sysupgrade $(basename "$zynq_sysupgrade") binds the installed archive-admission helper"
    require_pattern "$zynq_sysupgrade" 'MANIFEST_JSON_HELPER="/usr/libexec/dcentos/sysupgrade-manifest-json.py"' "Zynq sysupgrade $(basename "$zynq_sysupgrade") binds semantic manifest/version admission"
    require_pattern "$zynq_sysupgrade" '. "$ARCHIVE_ADMISSION_HELPER"' "Zynq sysupgrade $(basename "$zynq_sysupgrade") sources canonical archive admission"
    require_pattern "$zynq_sysupgrade" 'python3 "$MANIFEST_JSON_HELPER" validate "$PACKAGE_MANIFEST"' "Zynq sysupgrade $(basename "$zynq_sysupgrade") semantically admits JSON before authority reads"
    require_pattern "$zynq_sysupgrade" 'python3 "$MANIFEST_JSON_HELPER" compare-version "$1" "$2"' "Zynq sysupgrade $(basename "$zynq_sysupgrade") compares versions without awk numeric coercion"
    require_pattern "$zynq_sysupgrade" 'python3 "$MANIFEST_JSON_HELPER" read-version-file "$VERSION_PATH"' "Zynq sysupgrade $(basename "$zynq_sysupgrade") requires one canonical current-version line"
    require_pattern "$zynq_sysupgrade" '"${DCENT_SYSUPGRADE_WORKSPACE:-${TMPDIR:-/tmp}}"' "Zynq sysupgrade $(basename "$zynq_sysupgrade") admits the exact board archive with transaction-private scratch when available and a standalone target fallback"
    require_ordered_patterns "$zynq_sysupgrade" \
        'validate_sysupgrade_tar_preextract "$ROOTFS"' \
        'validate_sysupgrade_tar_members "$ROOTFS"' \
        "Zynq sysupgrade $(basename "$zynq_sysupgrade") bounds then admits the archive before extraction"
done
for zynq_post_build in "$S9_POST_BUILD" "$AM2_POST_BUILD" "$AM2_S19PRO_POST_BUILD" "$AM2_S17PRO_POST_BUILD"; do
    require_pattern "$zynq_post_build" 'scripts/lib/sysupgrade_archive_admission.sh' "Zynq post-build $(basename "$(dirname "$zynq_post_build")") installs the canonical archive-admission source"
    require_pattern "$zynq_post_build" 'sysupgrade-archive-admission.sh' "Zynq post-build $(basename "$(dirname "$zynq_post_build")") stages the target helper path"
    require_pattern "$zynq_post_build" 'sysupgrade-manifest-json.py' "Zynq post-build $(basename "$(dirname "$zynq_post_build")") installs semantic manifest/version admission"
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
require_pattern "$BUILD_AMLOGIC_NATIVE" '--lab-unsigned' 'amlogic native extractor accepts explicit lab unsigned validation'
require_pattern "$BUILD_AMLOGIC_NATIVE" 'expected existing tarball missing' 'amlogic native extractor requires an existing package'
require_pattern "$BUILD_AMLOGIC_NATIVE" 'does not invoke the disabled non-S9 packaging lane' 'amlogic native extractor does not claim build authority'
reject_pattern "$BUILD_AMLOGIC_NATIVE" 'DCENT_ALLOW_UNSIGNED_SYSUPGRADE="${DCENT_ALLOW_UNSIGNED_SYSUPGRADE:-1}"' 'amlogic native extractor does not default to unsigned lab override'
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
require_pattern "$SYSUPGRADE_COMMON" 'package must never derive its trust root from the signing key' 'shared signing helper forbids self-derived generated-key trust'
require_pattern "$PACKAGE" 'Release-root signing requires --verify-pubkey or DCENT_RELEASE_PUBKEY_FILE' 'standalone packager requires trusted public key for release-root signing'
require_pattern "$PACKAGE" 'unsigned package generation' 'standalone packager gates unsigned package generation'
require_pattern "$AM3_S19K_POST_IMAGE" 'DCENT_TARGET_SIDE_SYSUPGRADE=false' 'am3-s19k metadata disables target-side sysupgrade'
require_pattern "$AM3_S21_POST_IMAGE" 'DCENT_TARGET_SIDE_SYSUPGRADE=false' 'am3-s21 metadata disables target-side sysupgrade'
require_pattern "$AM3_S19K_POST_IMAGE" 'host_driven_rootfs_window_lab' 'am3-s19k metadata marks host-driven lab install'
require_pattern "$AM3_S21_POST_IMAGE" 'host_driven_rootfs_window_lab' 'am3-s21 metadata marks host-driven lab install'
require_pattern "$AM3_GEOMETRY" 'DCENT_AM3_ROOTFS_OFFSET_HEX="${DCENT_AM3_ROOTFS_OFFSET_HEX:-0x05700000}"' 'am3 geometry centralizes rootfs offset'
require_pattern "$AM3_GEOMETRY" 'DCENT_AM3_ROOTFS_WINDOW_HEX="${DCENT_AM3_ROOTFS_WINDOW_HEX:-0x02800000}"' 'am3 geometry centralizes rootfs window'
require_pattern "$AM3_S21_REVERT" 'ROOTFS_OFFSET="$DCENT_AM3_ROOTFS_OFFSET_HEX"' 'am3-s21 revert uses shared rootfs offset'
require_pattern "$RESTORE_ROUTE" '.arg(&post_dwell_fp.sha256)' 'restore route passes post-dwell SHA into revert helper'
require_pattern "$S9_REVERT" 'S9 stock restore is disabled' 'S9 stock restore is an explicit containment boundary'
reject_executable_pattern "$S9_REVERT" 'EXPECTED_SHA256=' 'contained S9 helper does not inspect an image'
for revert_script in "$S17_REVERT" "$AM3_S19K_REVERT" "$AM3_S21_REVERT"; do
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
require_pattern "$BB_S82" '[ -e "$AM3_BB_COLDBOOT_PROOF_MARKER" ] && AM3_BB_MINING_REQUESTED=1' 'am3-bb-s19jpro S82dcentrald snapshots the proof marker before safety admission'
require_pattern "$BB_S82" 'if [ "$AM3_BB_MINING_REQUESTED" -eq 1 ]; then' 'am3-bb-s19jpro S82dcentrald gates mining start on the immutable posture snapshot'
require_pattern "$BB_S82" 'MANAGEMENT-ONLY until cold-boot proof' 'am3-bb-s19jpro S82dcentrald logs management-only posture when unproven'
require_pattern "$BB_S82" 'AM3_BB_MGMT_ONLY_CONFIG="/etc/dcentrald.management-only.toml"' 'am3-bb-s19jpro S82dcentrald points at the management-only config'
# The ONLY --am3-bb-mining invocation must be inside the snapshotted
# marker-present branch; a marker appearing during safe-off cannot upgrade the
# current invocation from management-only into mining.
# There must be exactly one --am3-bb-mining ARGS assignment and it must follow
# the marker test. Assert the marker test precedes the --am3-bb-mining ARGS.
if awk '
    /if \[ "\$AM3_BB_MINING_REQUESTED" -eq 1 \]; then/ { seen_marker = 1 }
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
require_pattern "$S9_SYSUPGRADE" 'PACKAGE_STATUS=$(manifest_string_field "$PACKAGE_MANIFEST" status' 'S9 target sysupgrade reads typed manifest package status'
require_pattern "$S9_SYSUPGRADE" 'Package authority-v1 is missing MANIFEST.sig' 'S9 target sysupgrade rejects unsigned authority packages even with lab override'
require_pattern "$S9_SYSUPGRADE" 'DCENT_PACKAGE_STATUS to be a non-release lab value' 'S9 target raw lab sysupgrade requires non-release status'
require_pattern "$AM2_SYSUPGRADE" 'PACKAGE_STATUS=$(manifest_string_field "$PACKAGE_MANIFEST" status' 'am2 target sysupgrade reads typed manifest package status'
require_pattern "$AM2_SYSUPGRADE" 'Package authority-v1 is missing MANIFEST.sig' 'am2 target sysupgrade rejects unsigned authority packages even with lab override'
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

require_pattern "$AM2_S19PRO_SYSUPGRADE" 'PACKAGE_STATUS=$(manifest_string_field "$PACKAGE_MANIFEST" status' 'am2-s19pro sysupgrade reads typed manifest package status'
require_pattern "$AM2_S17PRO_SYSUPGRADE" 'PACKAGE_STATUS=$(manifest_string_field "$PACKAGE_MANIFEST" status' 'am2-s17pro sysupgrade reads typed manifest package status'
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

# --- Legacy SD-to-NAND compatibility path has no mutation authority ---------
SD_NAND_INSTALL='scripts/sd_nand_install.sh'
require_pattern "$SD_NAND_INSTALL" 'NOT IMPLEMENTED (mutation denied)' 'sd_nand_install declares its zero-mutation compatibility boundary'
require_pattern "$SD_NAND_INSTALL" 'exit 78' 'sd_nand_install refuses legacy mutation-shaped calls with EX_CONFIG'
for forbidden_writer in fw_setenv fw_printenv flash_erase nandwrite nanddump ubiattach ubiformat ubiupdatevol; do
    reject_executable_pattern "$SD_NAND_INSTALL" "$forbidden_writer" "sd_nand_install excludes writer: $forbidden_writer"
done
# --- end legacy SD-to-NAND containment --------------------------------------
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

# --- CV1835 brick-vector retirement -----------------------------------------
# Held FIP/U-Boot evidence disproves the old persistent-environment premise.
# No artifact producer is admitted until exact-toolchain and final-rootfs
# containment closure are proven. Every historical build/update entry point
# must therefore remain an unconditional refusal.
CV1835_SAFE_SU='scripts/safe_sysupgrade_cv_emmc.sh'
CV1835_REVERT='scripts/revert_to_stock_cv1835.sh'
CV1835_BUILD='scripts/build_cv1835_s19jpro.sh'
CV1835_POST_IMAGE='br2_external_dcentos/board/cvitek/cv1835-s19jpro/post-image.sh'
CV1835_POST_BUILD='br2_external_dcentos/board/cvitek/cv1835-s19jpro/post-build.sh'
CV1835_FW_ENV='br2_external_dcentos/board/cvitek/cv1835-s19jpro/rootfs-overlay/etc/fw_env.config'

require_pattern "$CV1835_SAFE_SU" 'exit 78' 'cv1835 updater exits with unconditional unavailable status'
require_pattern "$CV1835_REVERT" 'exit 78' 'cv1835 stock-revert exits with unconditional unavailable status'
require_pattern "$CV1835_BUILD" 'exit 78' 'cv1835 standalone build entry point refuses every artifact lane'
require_pattern "$CV1835_POST_BUILD" 'exit 78' 'cv1835 post-build hook refuses direct Buildroot invocation'
require_pattern "$CV1835_POST_IMAGE" 'exit 78' 'cv1835 post-image hook refuses direct Buildroot invocation'
require_pattern "$CV1835_SAFE_SU" 'BuiltInVolatile/mutation-denied' 'cv1835 updater records the evidenced volatile environment backend'
reject_pattern "$CV1835_SAFE_SU" 'DCENT_ALLOW_UNSIGNED_SYSUPGRADE' 'cv1835 updater has no proof or unsigned-package override'
if [ ! -e "$CV1835_FW_ENV" ] && [ ! -L "$CV1835_FW_ENV" ]; then
    pass 'cv1835 guessed fw_env.config remains absent'
else
    fail 'cv1835 guessed fw_env.config was reintroduced'
fi
# --- end CV1835 brick-vector retirement -------------------------------------

# =============================================================================
# Bucket-A P0 blocker coverage (CE-105 / CE-056 / CE-153 / CE-408 / CE-341 /
# CE-126 / CE-374 / CE-382). Additive, fail-closed pins so the new guards
# cannot silently rot. All files are already offline/static-checkable here.
# =============================================================================

# --- CE-105 successor: inactive-slot work is denied until the shared engine --
require_pattern "$SD_NAND_INSTALL" 'no exact admitted hardware/update descriptor' 'sd_nand_install requires typed hardware/update admission before implementation'
require_pattern "$SD_NAND_INSTALL" 'legacy install-shaped arguments cannot authorize' 'sd_nand_install rejects legacy argument-based write authority'

# The standalone packager owns only the S9 lane. AM2 artifacts come from their
# dedicated target post-image scripts, so an S9 DTB can never be relabelled AM2.
reject_pattern "$PACKAGE" 'DCENT_ALLOW_AM2_S9_PLACEHOLDER' 'S9 packager has no AM2 placeholder override'
reject_pattern "$PACKAGE" 'DCENT_FORCE_AM2_UPLOAD' 'S9 packager has no AM2 live-upload override'
reject_pattern "$PACKAGE" 'am2-s19j)' 'S9 packager cannot dispatch an AM2 package lane'
require_pattern "$PACKAGE" 'accepted: am1-s9' 'S9 packager declares its single accepted board'
if retired_output=$(bash "$PACKAGE" \
    --board am2-s19j \
    --version 0.9.0 \
    --images-dir /dcentos-am2-retired-lane-must-not-be-read \
    --output /dcentos-am2-retired-lane-must-not-be-written.tar 2>&1); then
    fail "S9 packager accepted the retired AM2 package lane"
else
    case "$retired_output" in
        *"Unsupported board: am2-s19j (accepted: am1-s9)"*)
            pass "S9 packager refuses AM2 before reading build inputs"
            ;;
        *)
            fail "S9 packager AM2 refusal was not the canonical early board gate"
            ;;
    esac
fi

# --- CE-153: raw environment transformers are guarded host-only tools --------
HOST_SWITCH_FW='scripts/switch_firmware.py'
HOST_SWITCH_FW_SH='scripts/switch_firmware.sh'
TARGET_SWITCH_FW='br2_external_dcentos/board/zynq/rootfs-overlay/usr/sbin/switch_firmware.py'
TARGET_SWITCH_FW_SH='br2_external_dcentos/board/zynq/rootfs-overlay/usr/sbin/switch_firmware.sh'
RUNTIME_PRUNE='br2_external_dcentos/board/common/prune-runtime-research-tools.sh'
require_pattern "$HOST_SWITCH_FW" '--i-understand-this-is-not-fw-setenv' 'host switch_firmware.py requires the not-fw-setenv acknowledgement'
require_pattern "$HOST_SWITCH_FW" 'REFUSING: switch_firmware.py is DEPRECATED' 'host switch_firmware.py refuses to run without the ack'
require_pattern "$HOST_SWITCH_FW" 'DEPRECATED FOR THE OTA/SYSUPGRADE WRITE PATH' 'host switch_firmware.py carries the deprecation banner'
require_pattern "$HOST_SWITCH_FW" 'distinct flags' 'host switch_firmware.py writes redundant env copies with distinct flags'
require_pattern "$HOST_SWITCH_FW_SH" '--i-understand-this-is-not-fw-setenv' 'host switch_firmware.sh requires the not-fw-setenv acknowledgement'
require_pattern "$HOST_SWITCH_FW_SH" 'REFUSING: switch_firmware.sh is DEPRECATED' 'host switch_firmware.sh refuses to run without the ack'
for target_switch_fw in "$TARGET_SWITCH_FW" "$TARGET_SWITCH_FW_SH"; do
    if [ ! -e "$target_switch_fw" ] && [ ! -L "$target_switch_fw" ]; then
        pass "target overlay excludes host-only transformer: $target_switch_fw"
    else
        fail "target overlay contains forbidden raw environment transformer: $target_switch_fw"
    fi
done
require_pattern "$RUNTIME_PRUNE" 'usr/sbin/switch_firmware.py' 'final rootfs prune removes stale switch_firmware.py'
require_pattern "$RUNTIME_PRUNE" 'usr/sbin/switch_firmware.sh' 'final rootfs prune removes stale switch_firmware.sh'

# --- CE-408 successor: unverified S19 AM2 restore is fully contained ---------
require_pattern "$S19_AM2_REVERT" 'NOT IMPLEMENTED (mutation denied)' 'revert_to_stock_s19_am2 declares its zero-mutation compatibility boundary'
require_pattern "$S19_AM2_REVERT" 'exit 78' 'revert_to_stock_s19_am2 refuses legacy mutation-shaped calls with EX_CONFIG'
for forbidden_writer in fw_setenv fw_printenv flash_erase nandwrite nanddump wget curl; do
    reject_executable_pattern "$S19_AM2_REVERT" "$forbidden_writer" "revert_to_stock_s19_am2 excludes writer: $forbidden_writer"
done

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
AM1_NAND_PLAN=scripts/am1_nand_backup_plan.py
AM1_NAND_PLAN_WRAPPER=scripts/am1_nand_backup_plan.sh
require_pattern "$AM1_NAND_PLAN_WRAPPER" 'am1_nand_backup_plan.py' 'AM1 planner wrapper delegates to the strict Python implementation'
require_pattern "$AM1_NAND_PLAN" '"--restore-artifact"' 'AM1 planner requires the physical restore artifact bytes'
require_pattern "$AM1_NAND_PLAN" '"--expect-host-key-sha256"' 'AM1 planner binds the pinned SSH host key'
require_pattern "$AM1_NAND_PLAN" 'args.expect_target != AUTHORIZED_BOARD_TARGET' 'AM1 planner rejects an operator-selected board class'
require_ordered_patterns "$AM1_NAND_PLAN" \
    'atomic_publish(args.output, markdown)' \
    'atomic_publish(args.json_template, encoded)' \
    'AM1 planner publishes its executable validated JSON last'

for nand_plan in scripts/am2_nand_backup_plan.sh; do
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
if zynq_target_payload_authority_selftest; then
    pass "Zynq target payload authority validates canonical bindings and rejects malformed paths, sizes, hashes, and extracted leaves"
else
    fail "Zynq target payload authority behavioral selftest failed"
fi
if python3 scripts/test_sysupgrade_manifest_json.py >/dev/null; then
    pass "semantic manifest JSON and deterministic version contracts pass"
else
    fail "semantic manifest JSON/version contract test failed"
fi
if release_image_hardening_coupling_selftest; then
    pass "CE-183 release-status packaging fails closed without release-image hardening and no-ops for lab status"
else
    fail "CE-183 release-status hardening coupling selftest failed"
fi
if toolbox_install_contract_selftest; then
    pass "toolbox install metadata accepts complete contracts and rejects missing or pre-acknowledged gates"
else
    fail "toolbox install metadata contract selftest failed"
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
