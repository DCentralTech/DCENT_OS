#!/bin/sh
#
# Offline DCENT_OS CI gates. These checks are intentionally static: they do not
# contact miners, open SSH, upload packages, flash devices, or reboot hardware.

set -eu

SCRIPT_DIR=$(CDPATH= cd "$(dirname "$0")" && pwd)
PROJECT_DIR=$(CDPATH= cd "$SCRIPT_DIR/.." && pwd)

cd "$PROJECT_DIR"

STATIC_ONLY=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        --static-only)
            STATIC_ONLY=1
            ;;
        -h|--help)
            printf 'Usage: %s [--static-only]\n' "$0"
            printf '  --static-only  Run source/text gates only; no cargo, Docker, or hardware actions.\n'
            exit 0
            ;;
        *)
            printf 'ERROR: unknown argument: %s\n' "$1" >&2
            exit 2
            ;;
    esac
    shift
done

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
    file=$1
    pattern=$2
    label=$3

    if [ ! -f "$file" ]; then
        fail "$label: missing file $file"
        return
    fi

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

    if [ ! -f "$file" ]; then
        fail "$label: missing file $file"
        return
    fi

    if grep -F -- "$pattern" "$file" >/dev/null 2>&1; then
        fail "$label: forbidden pattern '$pattern' in $file"
    else
        pass "$label"
    fi
}

sysupgrade_am2_variant_parity_check() {
    for file in \
        br2_external_dcentos/board/zynq/am2-s19jpro/rootfs-overlay/usr/sbin/sysupgrade \
        br2_external_dcentos/board/zynq/am2-s19pro/rootfs-overlay/usr/sbin/sysupgrade \
        br2_external_dcentos/board/zynq/am2-s17pro/rootfs-overlay/usr/sbin/sysupgrade
    do
        if [ ! -f "$file" ]; then
            fail "AM2 sysupgrade variant parity: missing $file"
            continue
        fi

        missing=0
        while IFS= read -r marker; do
            [ -n "$marker" ] || continue
            if ! grep -F -- "$marker" "$file" >/dev/null 2>&1; then
                fail "AM2 sysupgrade variant parity: $file missing '$marker'"
                missing=$((missing + 1))
            fi
        done <<'EOF'
WRONG_BOARD_EXIT=78
DCENT_SYSUPGRADE_OFFLINE_HARNESS
PACKAGE_SIG="$PACKAGE_SUBDIR/MANIFEST.sig"
PACKAGE_RELEASE_KEY="$PACKAGE_SUBDIR/release_ed25519.pub"
openssl pkeyutl -verify -rawin -pubin
verify_sha256 "$ROOTFS" "$ROOTFS_SHA"
verify_sha256 "$PACKAGE_KERNEL" "$KERNEL_SHA"
payload_fits_ubi_volume
EXPECTED_KERNEL_LEBS=23
EXPECTED_ROOTFS_LEBS=179
EXPECTED_ROOTFS_DATA_LEBS=210
fw_setenv --script "$_FW_SETENV_SCRIPT"
upgrade_stage=0
REFUSING to fall back to raw dd/flash_erase/nandwrite
EOF

        if [ "$missing" -eq 0 ]; then
            pass "AM2 sysupgrade variant parity: $file keeps signing, hash, geometry, and env guards"
        fi
    done
}

make_release_verify_gate_check() {
    if [ ! -f Makefile ]; then
        fail "make release local verification gate: missing Makefile"
        return
    fi

    if awk '
        BEGIN { in_release = 0; verify_line = 0; capsule_line = 0; skip_line = 0 }
        /^release:/ { in_release = 1; next }
        in_release && /^[A-Za-z0-9_.-]+:/ { in_release = 0 }
        in_release && /\$\(MAKE\) verify/ { verify_line = NR }
        in_release && /scripts\/build_s9_release_capsule\.sh/ { capsule_line = NR }
        in_release && /DCENT_SKIP_VERIFY/ { skip_line = NR }
        END {
            ok = verify_line > 0 && capsule_line > 0 && verify_line < capsule_line && skip_line == 0
            exit ok ? 0 : 1
        }
    ' Makefile; then
        pass "make release runs make verify before building release artifacts"
    else
        fail "make release must run make verify before the S9 capsule driver and must not honor DCENT_SKIP_VERIFY"
    fi
}

precommit_skip_after_hygiene_check() {
    hook='scripts/git-hooks/pre-commit'

    if [ ! -f "$hook" ]; then
        fail "pre-commit skip ordering: missing $hook"
        return
    fi

    if awk '
        $0 == "reject_staged_repo_hygiene_violations" { hygiene_call = NR }
        index($0, "DCENT_SKIP_VERIFY:-0") { skip_line = NR }
        END { exit (hygiene_call > 0 && skip_line > 0 && hygiene_call < skip_line) ? 0 : 1 }
    ' "$hook"; then
        pass "pre-commit DCENT_SKIP_VERIFY bypass is after staged-path hygiene"
    else
        fail "pre-commit DCENT_SKIP_VERIFY bypass must remain after staged-path hygiene"
    fi
}

run_python_script() {
    script=$1
    shift

    if command -v python3 >/dev/null 2>&1 && python3 -c 'import sys' >/dev/null 2>&1; then
        python3 "$script" "$@"
    elif command -v py >/dev/null 2>&1 && py -3 -c 'import sys' >/dev/null 2>&1; then
        py -3 "$script" "$@"
    elif command -v python >/dev/null 2>&1 && python -c 'import sys' >/dev/null 2>&1; then
        python "$script" "$@"
    else
        return 127
    fi
}

require_identical() {
    a=$1
    b=$2
    label=$3

    if [ ! -f "$a" ]; then
        fail "$label: missing file $a"
        return
    fi
    if [ ! -f "$b" ]; then
        fail "$label: missing file $b"
        return
    fi
    if cmp -s "$a" "$b"; then
        pass "$label"
    else
        fail "$label: $a and $b differ (must be byte-identical)"
    fi
}

check_no_cr() {
    file=$1

    if [ ! -f "$file" ]; then
        fail "line endings: missing file $file"
        return
    fi

    if LC_ALL=C grep "$(printf '\r')" "$file" >/dev/null 2>&1; then
        fail "line endings: CR byte found in $file"
    else
        pass "line endings: LF-only $file"
    fi
}

check_ascii() {
    file=$1

    if [ ! -f "$file" ]; then
        fail "ascii: missing file $file"
        return
    fi

    non_ascii_bytes=$(LC_ALL=C tr -d '\000-\177' < "$file" | wc -c | tr -d ' ')
    if [ "$non_ascii_bytes" != "0" ]; then
        fail "ascii: non-ASCII byte found in $file"
    else
        pass "ascii: ASCII-only $file"
    fi
}

syntax_shell() {
    file=$1
    first_line=$(sed -n '1p' "$file" 2>/dev/null || true)

    case "$first_line" in
        *bash*) printf '%s\n' bash ;;
        *) printf '%s\n' sh ;;
    esac
}

check_shell_syntax() {
    file=$1

    if [ ! -f "$file" ]; then
        fail "syntax: missing file $file"
        return
    fi

    shell_bin=$(syntax_shell "$file")
    if ! command -v "$shell_bin" >/dev/null 2>&1; then
        fail "syntax: $shell_bin is unavailable for $file"
        return
    fi

    if "$shell_bin" -n "$file"; then
        pass "syntax: $shell_bin -n $file"
    else
        fail "syntax: $shell_bin -n $file"
    fi
}

tracked_shell_files() {
    find scripts br2_external_dcentos \
        -type f \
        \( -name '*.sh' -o -path '*/etc/init.d/S*' \) \
        -print
}

tracked_lf_files() {
    for root in \
        scripts \
        br2_external_dcentos \
        ../dcentos-esp \
        ../dcentos-avalon \
        ../dcentaxe-avalon \
        ../dcentos-whatsminer \
        ../dcentos-innosilicon
    do
        [ -d "$root" ] || continue
        find "$root" \
            -type f \
            \( -name '*.sh' -o -path '*/rootfs-overlay/etc/init.d/S*' -o -name 'fw_env.config' \) \
            -print
    done | sort -u
}

pre_flash_package_only_selftest() {
    tmpdir=$(mktemp -d 2>/dev/null || echo "/tmp/dcentos-package-selftest.$$")
    rm -rf "$tmpdir"

    write_test_sysupgrade_package() {
        pkgdir=$1
        board=$2
        kernel_kind=$3
        root_kind=$4
        status=${5:-lab_unsigned}

        if [ "$status" = "lab_unsigned" ]; then
            manifest_profile=dcentos.sysupgrade-unsigned-lab/v1
        else
            manifest_profile=dcentos.sysupgrade-authority/v1
        fi
        rm -rf "$pkgdir" || return 1
        mkdir -p "$pkgdir" || return 1

        case "$kernel_kind" in
            uimage) printf '\047\005\031\126kernel\n' > "$pkgdir/kernel" || return 1 ;;
            raw) printf 'raw-kernel\n' > "$pkgdir/kernel" || return 1 ;;
            *) return 1 ;;
        esac
        case "$root_kind" in
            uimage) printf '\047\005\031\126root\n' > "$pkgdir/root" || return 1 ;;
            squashfs) printf '\150\163\161\163root\n' > "$pkgdir/root" || return 1 ;;
            raw) printf 'raw-root\n' > "$pkgdir/root" || return 1 ;;
            *) return 1 ;;
        esac
        printf 'board=%s\n' "$board" > "$pkgdir/METADATA" || return 1

        kernel_size=$(wc -c < "$pkgdir/kernel" | tr -d ' ') || return 1
        root_size=$(wc -c < "$pkgdir/root" | tr -d ' ') || return 1
        metadata_size=$(wc -c < "$pkgdir/METADATA" | tr -d ' ') || return 1
        kernel_sha=$(sha256sum "$pkgdir/kernel" | awk '{ print $1 }') || return 1
        root_sha=$(sha256sum "$pkgdir/root" | awk '{ print $1 }') || return 1
        metadata_sha=$(sha256sum "$pkgdir/METADATA" | awk '{ print $1 }') || return 1

        cat > "$pkgdir/MANIFEST.json" <<EOF || return 1
{
  "board": "$board",
  "schema": 1,
  "manifest_profile": "$manifest_profile",
  "product": "DCENT_OS",
  "package_type": "sysupgrade",
  "installable": true,
  "artifact_maturity": "experimental",
  "board_target": "$board",
  "status": "$status",
  "version": "test",
  "payloads": {
    "kernel": { "path": "sysupgrade-$board/kernel", "size": $kernel_size, "sha256": "$kernel_sha" },
    "rootfs": { "path": "sysupgrade-$board/root", "size": $root_size, "sha256": "$root_sha" },
    "metadata": { "path": "sysupgrade-$board/METADATA", "size": $metadata_size, "sha256": "$metadata_sha" }
  }
}
EOF
        (cd "$pkgdir" && sha256sum kernel root METADATA > SHA256SUMS) || return 1
    }

    pkgdir="$tmpdir/sysupgrade-am3-s19k"
    write_test_sysupgrade_package "$pkgdir" am3-s19k uimage uimage || return 1
    (cd "$tmpdir" && tar cf valid.tar sysupgrade-am3-s19k) || return 1

    if ! DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1 sh scripts/pre_flash_validate.sh --package-only "$tmpdir/valid.tar" am3-s19k >/dev/null 2>&1; then
        rm -rf "$tmpdir"
        return 1
    fi

    write_test_sysupgrade_package "$tmpdir/sysupgrade-am1-s9" am1-s9 uimage squashfs || return 1
    # write_test_sysupgrade_package assigns $pkgdir in POSIX-sh global scope, so the
    # am1-s9 build above clobbered the caller's am3-s19k pkgdir. Re-pin it before the
    # empty-version negative case (and the am3 rebuilds that follow) operate on am3.
    pkgdir="$tmpdir/sysupgrade-am3-s19k"
    sed 's/"version": "test"/"version": ""/' \
        "$pkgdir/MANIFEST.json" > "$pkgdir/MANIFEST.json.tmp" || return 1
    mv "$pkgdir/MANIFEST.json.tmp" "$pkgdir/MANIFEST.json" || return 1
    (cd "$tmpdir" && tar cf empty-version.tar sysupgrade-am3-s19k) || return 1
    if DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1 sh scripts/pre_flash_validate.sh --package-only "$tmpdir/empty-version.tar" am3-s19k >/dev/null 2>&1; then
        rm -rf "$tmpdir"
        return 1
    fi
    write_test_sysupgrade_package "$pkgdir" am3-s19k uimage uimage || return 1

    (cd "$tmpdir" && tar cf valid-s9.tar sysupgrade-am1-s9) || return 1
    if ! DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1 sh scripts/pre_flash_validate.sh --package-only "$tmpdir/valid-s9.tar" am1-s9 >/dev/null 2>&1; then
        rm -rf "$tmpdir"
        return 1
    fi

    PY_TAR=
    PY_TAR_ARG=
    if command -v python3 >/dev/null 2>&1 && python3 -c 'import sys' >/dev/null 2>&1; then
        PY_TAR=python3
    elif command -v py >/dev/null 2>&1 && py -3 -c 'import sys' >/dev/null 2>&1; then
        PY_TAR=py
        PY_TAR_ARG=-3
    elif command -v python >/dev/null 2>&1 && python -c 'import sys' >/dev/null 2>&1; then
        PY_TAR=python
    else
        rm -rf "$tmpdir"
        return 1
    fi
    "$PY_TAR" ${PY_TAR_ARG:-} - "$tmpdir/traversal.tar" <<'PY' || return 1
import pathlib
import sys
import tarfile

tar_path = pathlib.Path(sys.argv[1])
payload = b"evil\n"
info = tarfile.TarInfo("../evil")
info.size = len(payload)
with tarfile.open(tar_path, "w") as tf:
    import io
    tf.addfile(info, io.BytesIO(payload))
PY
    if DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1 sh scripts/pre_flash_validate.sh --package-only "$tmpdir/traversal.tar" am3-s19k >/dev/null 2>&1; then
        rm -rf "$tmpdir"
        return 1
    fi

    "$PY_TAR" ${PY_TAR_ARG:-} - "$tmpdir/symlink.tar" <<'PY' || return 1
import pathlib
import sys
import tarfile

tar_path = pathlib.Path(sys.argv[1])
with tarfile.open(tar_path, "w") as tf:
    directory = tarfile.TarInfo("sysupgrade-am3-s19k/")
    directory.type = tarfile.DIRTYPE
    tf.addfile(directory)

    link = tarfile.TarInfo("sysupgrade-am3-s19k/link")
    link.type = tarfile.SYMTYPE
    link.linkname = "kernel"
    tf.addfile(link)
PY
    if DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1 sh scripts/pre_flash_validate.sh --package-only "$tmpdir/symlink.tar" am3-s19k >/dev/null 2>&1; then
        rm -rf "$tmpdir"
        return 1
    fi

    write_test_sysupgrade_package "$tmpdir/sysupgrade-am3-s19k-badroot" am3-s19k uimage squashfs || return 1
    rm -rf "$tmpdir/sysupgrade-am3-s19k" || return 1
    mv "$tmpdir/sysupgrade-am3-s19k-badroot" "$tmpdir/sysupgrade-am3-s19k" || return 1
    (cd "$tmpdir" && tar cf am3-squashfs-root.tar sysupgrade-am3-s19k) || return 1
    rm -rf "$tmpdir/sysupgrade-am3-s19k" || return 1
    if DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1 sh scripts/pre_flash_validate.sh --package-only "$tmpdir/am3-squashfs-root.tar" am3-s19k >/dev/null 2>&1; then
        rm -rf "$tmpdir"
        return 1
    fi

    write_test_sysupgrade_package "$tmpdir/sysupgrade-am3-s21" am3-s21 uimage uimage || return 1
    (cd "$tmpdir" && tar cf wrong-prefix.tar sysupgrade-am3-s21) || return 1
    if DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1 sh scripts/pre_flash_validate.sh --package-only "$tmpdir/wrong-prefix.tar" am3-s19k >/dev/null 2>&1; then
        rm -rf "$tmpdir"
        return 1
    fi

    write_test_sysupgrade_package "$tmpdir/sysupgrade-am3-s19k" am3-s19k uimage uimage || return 1
    root_size=$(wc -c < "$tmpdir/sysupgrade-am3-s19k/root" | tr -d ' ') || return 1
    sed 's/"size": '"$root_size"'/"size": 999999/' \
        "$tmpdir/sysupgrade-am3-s19k/MANIFEST.json" > "$tmpdir/sysupgrade-am3-s19k/MANIFEST.json.tmp" || return 1
    mv "$tmpdir/sysupgrade-am3-s19k/MANIFEST.json.tmp" "$tmpdir/sysupgrade-am3-s19k/MANIFEST.json" || return 1
    (cd "$tmpdir" && tar cf bad-manifest.tar sysupgrade-am3-s19k) || return 1
    if DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1 sh scripts/pre_flash_validate.sh --package-only "$tmpdir/bad-manifest.tar" am3-s19k >/dev/null 2>&1; then
        rm -rf "$tmpdir"
        return 1
    fi

    if command -v openssl >/dev/null 2>&1; then
        openssl genpkey -algorithm Ed25519 -out "$tmpdir/release.key" >/dev/null 2>&1 || return 1
        openssl pkey -in "$tmpdir/release.key" -pubout -out "$tmpdir/release.pub" >/dev/null 2>&1 || return 1
        write_test_sysupgrade_package "$tmpdir/sysupgrade-am3-s19k" am3-s19k uimage uimage || return 1
        kernel_size=$(wc -c < "$tmpdir/sysupgrade-am3-s19k/kernel" | tr -d ' ') || return 1
        root_size=$(wc -c < "$tmpdir/sysupgrade-am3-s19k/root" | tr -d ' ') || return 1
        metadata_size=$(wc -c < "$tmpdir/sysupgrade-am3-s19k/METADATA" | tr -d ' ') || return 1
        kernel_sha=$(sha256sum "$tmpdir/sysupgrade-am3-s19k/kernel" | awk '{ print $1 }') || return 1
        root_sha=$(sha256sum "$tmpdir/sysupgrade-am3-s19k/root" | awk '{ print $1 }') || return 1
        metadata_sha=$(sha256sum "$tmpdir/sysupgrade-am3-s19k/METADATA" | awk '{ print $1 }') || return 1
        cp -R "$pkgdir" "$tmpdir/sysupgrade-am3-s19k-signed" || return 1
        signed_dir="$tmpdir/sysupgrade-am3-s19k-signed"
        cp "$tmpdir/release.pub" "$signed_dir/release_ed25519.pub" || return 1
        pub_size=$(wc -c < "$signed_dir/release_ed25519.pub" | tr -d ' ') || return 1
        pub_sha=$(sha256sum "$signed_dir/release_ed25519.pub" | awk '{ print $1 }') || return 1
        cat > "$signed_dir/MANIFEST.json" <<EOF || return 1
{
  "product": "DCENT_OS",
  "schema": 1,
  "manifest_profile": "dcentos.sysupgrade-authority/v1",
  "package_type": "sysupgrade",
  "board": "am3-s19k",
  "installable": true,
  "artifact_maturity": "experimental",
  "board_target": "am3-s19k",
  "status": "release",
  "version": "test",
  "payloads": {
    "kernel": { "path": "sysupgrade-am3-s19k/kernel", "size": $kernel_size, "sha256": "$kernel_sha" },
    "rootfs": { "path": "sysupgrade-am3-s19k/root", "size": $root_size, "sha256": "$root_sha" },
    "metadata": { "path": "sysupgrade-am3-s19k/METADATA", "size": $metadata_size, "sha256": "$metadata_sha" },
    "verification_key": { "path": "sysupgrade-am3-s19k/release_ed25519.pub", "size": $pub_size, "sha256": "$pub_sha" }
  }
}
EOF
        (cd "$signed_dir" && sha256sum kernel root METADATA release_ed25519.pub > SHA256SUMS) || return 1
        openssl pkeyutl -sign -rawin -inkey "$tmpdir/release.key" -in "$signed_dir/MANIFEST.json" -out "$signed_dir/MANIFEST.sig" >/dev/null 2>&1 || return 1
        rm -rf "$tmpdir/sysupgrade-am3-s19k" || return 1
        mv "$signed_dir" "$tmpdir/sysupgrade-am3-s19k" || return 1
        (cd "$tmpdir" && tar cf signed.tar sysupgrade-am3-s19k) || return 1
        if ! DCENT_RELEASE_PUBKEY_FILE="$tmpdir/release.pub" sh scripts/pre_flash_validate.sh --package-only "$tmpdir/signed.tar" am3-s19k >/dev/null 2>&1; then
            rm -rf "$tmpdir"
            return 1
        fi
    fi

    rm -rf "$tmpdir"
    return 0
}

am3_geometry_static_selftest() {
    . scripts/lib/am3_geometry.sh || return 1
    [ "$DCENT_AM3_ROOTFS_MTD" = "/dev/mtd5" ] || return 1
    [ "$DCENT_AM3_ROOTFS_OFFSET_HEX" = "0x05700000" ] || return 1
    [ "$DCENT_AM3_ROOTFS_WINDOW_HEX" = "0x02800000" ] || return 1
    [ "$DCENT_AM3_ROOTFS_ERASE_COUNT" = "320" ] || return 1
    [ "$DCENT_AM3_ROOTFS_END_DEC" = "133169152" ] || return 1

    grep -F 'ROOTFS_OFFSET_HEX="$DCENT_AM3_ROOTFS_OFFSET_HEX"' scripts/install_amlogic_persistent.sh >/dev/null 2>&1 || return 1
    grep -F 'ROOTFS_WINDOW_HEX="$DCENT_AM3_ROOTFS_WINDOW_HEX"' scripts/install_amlogic_persistent.sh >/dev/null 2>&1 || return 1
    grep -F 'ROOTFS_OFFSET_HEX="$DCENT_AM3_ROOTFS_OFFSET_HEX"' scripts/amlogic_lab_rootfs.sh >/dev/null 2>&1 || return 1
    grep -F 'ROOTFS_WINDOW_HEX="$DCENT_AM3_ROOTFS_WINDOW_HEX"' scripts/amlogic_lab_rootfs.sh >/dev/null 2>&1 || return 1
    grep -F 'ROOTFS_OFFSET="$DCENT_AM3_ROOTFS_OFFSET_HEX"' scripts/revert_to_stock_am3_aml_s19k.sh >/dev/null 2>&1 || return 1
    grep -F 'ROOTFS_MTD="$DCENT_AM3_ROOTFS_MTD"' scripts/revert_to_stock_am3_aml_s19k.sh >/dev/null 2>&1 || return 1
    grep -F 'ROOTFS_OFFSET="$DCENT_AM3_ROOTFS_OFFSET_HEX"' scripts/revert_to_stock_am3_aml_s21.sh >/dev/null 2>&1 || return 1
    grep -F 'ROOTFS_MTD="$DCENT_AM3_ROOTFS_MTD"' scripts/revert_to_stock_am3_aml_s21.sh >/dev/null 2>&1 || return 1
    return 0
}

dcentrald_version_gate_selftest() {
    tmpdir=$(mktemp -d 2>/dev/null || echo "/tmp/dcentos-version-gate-selftest.$$")
    rm -rf "$tmpdir"
    mkdir -p "$tmpdir/target/etc" "$tmpdir/target/usr/local/bin" || return 1

    cat > "$tmpdir/Cargo.toml" <<EOF || return 1
[workspace.package]
version = "0.9.0"
EOF
    printf '0.9.0\n' > "$tmpdir/target/etc/dcentos-version" || return 1
    printf 'fixture dcentrald/0.9.0\n' > "$tmpdir/target/usr/local/bin/dcentrald" || return 1

    if ! sh -c '. scripts/lib/dcentrald_version_gate.sh; dcent_require_dcentrald_version_match "$1" "$2" selftest "$3"' sh "$tmpdir/target" "$tmpdir/target/usr/local/bin/dcentrald" "$tmpdir/Cargo.toml" >/dev/null 2>&1; then
        rm -rf "$tmpdir"
        return 1
    fi

    printf 'fixture dcentrald/0.5.0\n' > "$tmpdir/target/usr/local/bin/dcentrald" || return 1
    if sh -c '. scripts/lib/dcentrald_version_gate.sh; dcent_require_dcentrald_version_match "$1" "$2" selftest "$3"' sh "$tmpdir/target" "$tmpdir/target/usr/local/bin/dcentrald" "$tmpdir/Cargo.toml" >/dev/null 2>&1; then
        rm -rf "$tmpdir"
        return 1
    fi
    if ! DCENT_PACKAGE_STATUS=lab_stale_version DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1 \
        sh -c '. scripts/lib/dcentrald_version_gate.sh; dcent_require_dcentrald_version_match "$1" "$2" selftest "$3"' sh "$tmpdir/target" "$tmpdir/target/usr/local/bin/dcentrald" "$tmpdir/Cargo.toml" >/dev/null 2>&1; then
        rm -rf "$tmpdir"
        return 1
    fi

    printf '0.5.0\n' > "$tmpdir/target/etc/dcentos-version" || return 1
    printf 'fixture dcentrald/0.9.0\n' > "$tmpdir/target/usr/local/bin/dcentrald" || return 1
    if sh -c '. scripts/lib/dcentrald_version_gate.sh; dcent_require_dcentrald_version_match "$1" "$2" selftest "$3"' sh "$tmpdir/target" "$tmpdir/target/usr/local/bin/dcentrald" "$tmpdir/Cargo.toml" >/dev/null 2>&1; then
        rm -rf "$tmpdir"
        return 1
    fi

    rm -rf "$tmpdir"
    return 0
}

toml_watchdog_disabled() {
    awk '
        /^[[:space:]]*#/ { next }
        /^[[:space:]]*\[/ {
            in_watchdog = ($0 ~ /^[[:space:]]*\[watchdog\][[:space:]]*(#.*)?$/)
            next
        }
        in_watchdog && $0 ~ /^[[:space:]]*enabled[[:space:]]*=[[:space:]]*false[[:space:]]*(#.*)?$/ {
            found = 1
        }
        END { exit(found ? 0 : 1) }
    ' "$1"
}

manifest_watchdog_disabled() {
    awk '
        /"watchdog(_enabled|\.enabled)"[[:space:]]*:[[:space:]]*false/ { found = 1 }
        /watchdog[.]enabled[[:space:]]*=[[:space:]]*false/ { found = 1 }
        /"watchdog"[[:space:]]*:/ { in_watchdog = 1 }
        in_watchdog && /"enabled"[[:space:]]*:[[:space:]]*false/ { found = 1 }
        in_watchdog && /}/ { in_watchdog = 0 }
        END { exit(found ? 0 : 1) }
    ' "$1"
}

watchdog_config_path_exempt() {
    case "$1" in
        br2_external_dcentos/board/beaglebone/am3-bb/*|br2_external_dcentos/board/beaglebone/am3-bb-s19jpro/*|*/br2_external_dcentos/board/beaglebone/am3-bb/*|*/br2_external_dcentos/board/beaglebone/am3-bb-s19jpro/*)
            return 0
            ;;
    esac
    return 1
}

watchdog_shipped_configs_ok() {
    root=$1
    offenders=$2
    found_configs=0
    : > "$offenders"

    for cfg in $(find "$root" -type f -path '*/rootfs-overlay/etc/*.toml' 2>/dev/null | sort); do
        found_configs=$((found_configs + 1))
        if watchdog_config_path_exempt "$cfg"; then
            continue
        fi
        if toml_watchdog_disabled "$cfg"; then
            printf '%s\n' "$cfg" >> "$offenders"
        fi
    done

    for manifest in $(find "$root" -type f \( -iname '*manifest*.json' -o -iname '*release*.json' -o -iname '*manifest*.toml' -o -iname '*release*.toml' \) 2>/dev/null | sort); do
        if watchdog_config_path_exempt "$manifest"; then
            continue
        fi
        case "$manifest" in
            *.toml)
                if toml_watchdog_disabled "$manifest"; then
                    printf '%s\n' "$manifest" >> "$offenders"
                fi
                ;;
            *)
                if manifest_watchdog_disabled "$manifest"; then
                    printf '%s\n' "$manifest" >> "$offenders"
                fi
                ;;
        esac
    done

    if [ "$found_configs" -eq 0 ]; then
        printf 'NO_SHIPPED_TOML_CONFIGS_FOUND\n' >> "$offenders"
        return 2
    fi
    [ ! -s "$offenders" ]
}

watchdog_shipped_config_gate_check() {
    tmpfile=$(mktemp 2>/dev/null || echo "/tmp/dcentos-watchdog-shipped-configs.$$")
    rm -f "$tmpfile"
    if watchdog_shipped_configs_ok "br2_external_dcentos" "$tmpfile"; then
        pass "SAF-3 shipped configs: watchdog enabled in release overlays (am3-bb management-only lane exempt)"
    else
        rc=$?
        if [ "$rc" -eq 2 ]; then
            fail "SAF-3 shipped configs: no rootfs-overlay/etc/*.toml configs found under br2_external_dcentos (path drift?)"
        else
            fail "SAF-3 shipped configs: watchdog disabled outside the documented am3-bb management-only lane: $(tr '\n' ' ' < "$tmpfile")"
        fi
    fi
    rm -f "$tmpfile"
}

watchdog_shipped_config_gate_selftest() {
    tmpdir=$(mktemp -d 2>/dev/null || echo "/tmp/dcentos-watchdog-gate-selftest.$$")
    rm -rf "$tmpdir"
    root="$tmpdir/br2_external_dcentos"
    zynq_cfg="$root/board/zynq/rootfs-overlay/etc/dcentrald.toml"
    bb_cfg="$root/board/beaglebone/am3-bb/rootfs-overlay/etc/dcentrald.toml"
    manifest="$root/board/zynq/rootfs-overlay/etc/dcentos-release-manifest.json"
    offenders="$tmpdir/offenders.txt"

    mkdir -p "$(dirname "$zynq_cfg")" "$(dirname "$bb_cfg")" || return 1

    cat > "$zynq_cfg" <<'EOF' || return 1
[watchdog]
enabled = false
EOF
    if watchdog_shipped_configs_ok "$root" "$offenders"; then
        rm -rf "$tmpdir"
        return 1
    fi

    cat > "$zynq_cfg" <<'EOF' || return 1
[watchdog]
enabled = true
EOF
    cat > "$bb_cfg" <<'EOF' || return 1
[watchdog]
enabled = false
EOF
    if ! watchdog_shipped_configs_ok "$root" "$offenders"; then
        rm -rf "$tmpdir"
        return 1
    fi

    cat > "$manifest" <<'EOF' || return 1
{ "release_image": true, "watchdog": { "enabled": false } }
EOF
    if watchdog_shipped_configs_ok "$root" "$offenders"; then
        rm -rf "$tmpdir"
        return 1
    fi

    rm -rf "$tmpdir"
    return 0
}

require_file '.gitattributes'
require_file 'scripts/package_sysupgrade.sh'
require_file 'scripts/pre_flash_validate.sh'
require_file 'scripts/lib/am3_geometry.sh'
require_file 'scripts/lib/dcentrald_version_gate.sh'
require_file 'scripts/lib/sysupgrade_package_common.sh'
require_file 'scripts/run_wave_regressions.sh'
require_file 'dcentrald/dcentrald/tests/wave55i_phase0_full_ordering.rs'
require_file 'dcentrald/dcentrald/tests/i2c_eeprom_denylist_breadth.rs'
require_file 'scripts/validation_preflight.sh'
require_file 'scripts/validate_production_readiness.ps1'
require_file 'br2_external_dcentos/board/amlogic/am3-s19kpro/post-build.sh'
require_file 'br2_external_dcentos/board/amlogic/am3-s19kpro/post-image.sh'
require_file 'br2_external_dcentos/board/amlogic/am3-s21/post-build.sh'
require_file 'br2_external_dcentos/board/amlogic/am3-s21/post-image.sh'

require_pattern '.gitattributes' '*.sh text eol=lf' 'gitattributes keeps shell files LF-only'
require_pattern '.gitattributes' '/br2_external_dcentos/**/etc/init.d/* text eol=lf' 'gitattributes keeps init scripts LF-only'

for file in $(tracked_lf_files); do
    check_no_cr "$file"
done

for file in \
    scripts/pre_flash_validate.sh \
    scripts/lib/dcentrald_version_gate.sh \
    scripts/build_amlogic_native_install.sh \
    scripts/install_amlogic_persistent.sh \
    scripts/amlogic_lab_rootfs.sh
do
    check_ascii "$file"
done

for file in $(tracked_shell_files); do
    check_shell_syntax "$file"
done

if pre_flash_package_only_selftest; then
    pass "pre-flash package-only selftest accepts valid signed/lab packages and rejects malformed packages"
else
    fail "pre-flash package-only selftest failed"
fi

if am3_geometry_static_selftest; then
    pass "am3 geometry selftest proves install/lab/revert consume shared offsets"
else
    fail "am3 geometry selftest failed"
fi

if dcentrald_version_gate_selftest; then
    pass "dcentrald version gate selftest fails closed and accepts only explicit lab override"
else
    fail "dcentrald version gate selftest failed"
fi

if watchdog_shipped_config_gate_selftest; then
    pass "SAF-3 shipped configs selftest rejects watchdog-off overlays/manifests and permits am3-bb management-only"
else
    fail "SAF-3 shipped configs selftest failed"
fi
watchdog_shipped_config_gate_check

require_pattern 'scripts/package_sysupgrade.sh' 'Refusing live upload of an unsigned package' 'package_sysupgrade refuses unsigned live upload'
reject_pattern 'scripts/package_sysupgrade.sh' 'DCENT_FORCE_AM2_UPLOAD' 'S9 package_sysupgrade exposes no AM2 live-upload override'
require_pattern 'scripts/package_sysupgrade.sh' '"requires_inactive_slot": true' 'package manifest declares inactive-slot requirement'
require_file 'scripts/lib/sd_image_signing_gate.sh'
require_file 'scripts/test_sd_signing_gate_static.sh'
if sh scripts/test_sd_signing_gate_static.sh >/dev/null 2>&1; then
    pass "SD image signing gate selftest rejects incomplete, unbound, and missing manifests"
else
    fail "SD image signing gate selftest failed"
fi
require_file 'scripts/sign_sd_image.sh'
require_file 'scripts/sign_sd_image.py'
require_file 'scripts/test_sign_sd_image.sh'
if sh scripts/test_sign_sd_image.sh >/dev/null 2>&1; then
    pass "SD image signer selftest validates exact manifest-bound publication"
else
    fail "SD image signer selftest failed"
fi
# AM2 SD artifact staging helper + static self-test (CE-410 residual prep).
# Anti-orphan requires the basename of every scripts/**/test_*.sh to appear here.
require_file 'scripts/stage_am2_sd_artifacts.sh'
require_file 'scripts/stage_am2_sd_artifacts.py'
require_file 'scripts/test_stage_am2_sd_artifacts_static.sh'
# bash required: the selftest uses BASH_SOURCE and Bash test helpers.
if bash scripts/test_stage_am2_sd_artifacts_static.sh >/dev/null 2>&1; then
    pass "AM2 SD artifact staging selftest validates the exact atomic lifecycle"
else
    fail "AM2 SD artifact staging selftest failed"
fi
# Install-path honesty + aggregate GO guard (cannot claim public install GO
# without CAPSTONE_EVIDENCE; never run silent).
require_file 'scripts/check_install_path_honesty.py'
require_file 'scripts/check_install_path_go_guard.py'
require_file 'scripts/test_sd_common_mbr_static.sh'
if command -v python3 >/dev/null 2>&1; then
    if python3 scripts/check_install_path_honesty.py >/dev/null 2>&1; then
        pass "install-path honesty check"
    else
        fail "install-path honesty check failed"
    fi
    if python3 scripts/check_install_path_go_guard.py >/dev/null 2>&1; then
        pass "install-path GO guard (aggregate GO stays NO without CAPSTONE)"
    else
        fail "install-path GO guard failed"
    fi
elif command -v py >/dev/null 2>&1; then
    if py -3 scripts/check_install_path_honesty.py >/dev/null 2>&1; then
        pass "install-path honesty check"
    else
        fail "install-path honesty check failed"
    fi
    if py -3 scripts/check_install_path_go_guard.py >/dev/null 2>&1; then
        pass "install-path GO guard"
    else
        fail "install-path GO guard failed"
    fi
else
    fail "python3/py required for install-path honesty/GO guards"
fi
if bash scripts/test_sd_common_mbr_static.sh >/dev/null 2>&1; then
    pass "sd_common pure-Python three-part MBR write selftest"
else
    fail "sd_common MBR selftest failed"
fi
require_pattern 'scripts/sign_sd_image.py' 'validate_manifest_binding' 'sign_sd_image binds completeness evidence to exact image bytes'
require_pattern 'scripts/sign_sd_image.py' 'trusted release public key is required' 'sign_sd_image fails closed without pinned public authority'
require_pattern 'scripts/sign_sd_image.py' 'sign_release_receipt' 'sign_sd_image reuses exact no-replace durable signing lifecycle'
require_pattern 'scripts/sign_sd_image.py' 'durable_input=True' 'sign_sd_image flushes pinned image bytes before signature commit'
reject_pattern 'scripts/sign_sd_image.py' 'dcentos.am3_bb_vnish_sd_image_manifest' 'sign_sd_image denies release authority to the open-gate VNish prototype'
require_pattern 'scripts/build_am2_s19jpro_sd_disk_image.sh' 'boot_artifacts_complete' 'am2 SD builder emits boot artifact completeness manifest'
require_pattern 'scripts/build_am2_s19jpro_sd_disk_image.sh' 'image_sha256' 'am2 SD manifest binds exact image digest'
require_pattern 'scripts/build_am2_s19jpro_sd_disk_image.sh' 'stale signature' 'am2 SD builder refuses stale sibling signatures before rewrite'
require_pattern 'scripts/build_am2_s19jpro_sd_disk_image.sh' '"BOOT.bin"' 'am2 SD manifest records BOOT.bin presence'
require_pattern 'scripts/build_am2_s19jpro_sd_disk_image.sh' '"uEnv.txt"' 'am2 SD manifest records uEnv presence'
require_pattern 'scripts/build_am3_bb_sd_vnish_bootbin_image.sh' 'not eligible for DCENT_OS release signing' 'VNish builder refuses release signing while vendor and RSA gates remain open'
reject_pattern 'scripts/build_am3_bb_sd_vnish_bootbin_image.sh' 'release_ed25519.pub' 'VNish builder does not reopen and copy a mutable public-key sidecar'
require_pattern 'scripts/build_in_docker.sh' 'dcent_sd_require_complete_manifest_for_signing' 'docker SD signing path requires complete AM2 manifest before signing'
require_pattern 'scripts/build_in_docker.sh' 'UNSIGNED-LAB-ROOTFS-ONLY' 'docker SD path relabels unsigned incomplete AM2 images'
require_pattern 'scripts/build_in_docker.sh' 'BOARD_POST_IMAGE" = "vnish-bootbin-sd"' 'docker SD path keeps VNish bootbin SD branch separate'
# CI-GATE-CE271-SIGNER-PUBKEY-MOUNT: the late Docker signer stages (Phase 8b
# am3-bb tarball, Phase 8c SD .img) must mount the trusted release pubkey into
# the signer container and pass the CONTAINER path, so verify-after-sign checks
# the PINNED trusted key rather than a pubkey self-derived from the signing key
# (which verifies any key). A raw host-path env leak makes the -f test always
# fail inside the container -> silent self-derived fallback (fail-open).
reject_pattern 'scripts/build_in_docker.sh' '-e DCENT_RELEASE_PUBKEY_FILE="${DCENT_RELEASE_PUBKEY_FILE:-}"' 'CE-271: signer containers must not receive a raw host pubkey path'
require_pattern 'scripts/sign_release_artifact.py' 'release artifact signing requires a trusted public key' 'CE-271: Phase 8b exact signer fails closed without a trusted pubkey'
require_pattern 'scripts/build_in_docker.sh' '--pubkey "${DCENT_RELEASE_PUBKEY_FILE}"' 'CE-271: Phase 8b passes the mounted trusted pubkey to the exact signer'
if [ "$(grep -cF -- 'PUBKEY_MOUNT_ARGS[@]' scripts/build_in_docker.sh)" -ge 3 ]; then
    pass "CE-271: Phase 8/8b/8c docker stages mount the trusted release pubkey"
else
    fail "CE-271: late Docker signer stages missing PUBKEY_MOUNT_ARGS pubkey mounts"
fi
require_pattern 'br2_external_dcentos/board/zynq/rootfs-overlay/etc/init.d/S99upgrade' 'upgrade_stage' 'rollback init script still keys off upgrade_stage'
# W8 parity: the A/B commit gate must require a REAL health endpoint + a
# sustained boot-success window, not just a socket-bind probe (a daemon that
# binds then crash-loops must NOT be committed as good -> the auto-rollback the
# inactive-slot write armed stays effective). Brick-safe: stricter only.
require_pattern 'br2_external_dcentos/board/zynq/rootfs-overlay/etc/init.d/S99upgrade' '/api/system/health' 'A/B commit gate checks the real health endpoint, not just socket bind'
require_pattern 'br2_external_dcentos/board/zynq/rootfs-overlay/etc/init.d/S99upgrade' 'MIN_HEALTHY_UPTIME_S' 'A/B commit gate enforces a sustained boot-success window'
require_pattern 'br2_external_dcentos/board/zynq/rootfs-overlay/etc/init.d/S99verify' 'report-only proof consumer' 'S99verify remains a non-mutating consumer of the S99upgrade boot-success decision'
require_pattern 'scripts/pre_flash_validate.sh' 'inactive NAND slot' 'pre-flash gate still validates inactive NAND slot'
require_pattern 'scripts/pre_flash_validate.sh' '--package-only' 'pre-flash validator keeps local package-only mode'
require_pattern 'scripts/pre_flash_validate.sh' 'tar entry paths are relative and traversal-free' 'pre-flash package-only mode rejects unsafe tar paths'
require_pattern 'scripts/pre_flash_validate.sh' 'tar entry types are regular files/directories only' 'pre-flash package-only mode rejects links/devices'
require_pattern 'scripts/pre_flash_validate.sh' 'MANIFEST.json board_target' 'pre-flash package-only mode validates manifest board_target'
require_pattern 'scripts/pre_flash_validate.sh' 'SHA256SUMS verifies kernel/root/METADATA' 'pre-flash package-only mode validates package hashes'
require_pattern 'scripts/pre_flash_validate.sh' 'MANIFEST.json payload paths/sizes/hashes match actual files' 'pre-flash package-only mode cross-checks manifest payloads'
require_pattern 'scripts/pre_flash_validate.sh' 'AM3 kernel/root uImage magic valid' 'pre-flash package-only mode validates AM3 uImage magic'
require_pattern 'scripts/pre_flash_validate.sh' 'squashfs-style root payload magic valid' 'pre-flash package-only mode validates squashfs-style root payloads'
require_pattern 'scripts/pre_flash_validate.sh' 'root payload fits am3 rootfs window' 'pre-flash package-only mode bounds am3 rootfs payload'
require_pattern 'scripts/pre_flash_validate.sh' 'assert_payload_fits_window "$board root" "$root_size" "$ZYNQ_ROOTFS_MAX_BYTES" "zynq rootfs window"' 'pre-flash package-only mode bounds zynq rootfs payloads'
require_pattern 'scripts/pre_flash_validate.sh' 'assert_payload_fits_window "$board kernel" "$kernel_size" "$ZYNQ_KERNEL_MAX_BYTES" "zynq kernel window"' 'pre-flash package-only mode bounds zynq kernel payloads'
require_pattern 'br2_external_dcentos/board/zynq/rootfs-overlay/usr/sbin/sysupgrade' 'payload_fits_ubi_volume' 'S9 sysupgrade checks payload byte fit before ubiupdatevol'
require_pattern 'br2_external_dcentos/board/zynq/am2-s19jpro/rootfs-overlay/usr/sbin/sysupgrade' 'payload_fits_ubi_volume' 'am2-s19j sysupgrade checks payload byte fit before ubiupdatevol'
require_pattern 'br2_external_dcentos/board/zynq/am2-s19pro/rootfs-overlay/usr/sbin/sysupgrade' 'payload_fits_ubi_volume' 'am2-s19pro sysupgrade checks payload byte fit before ubiupdatevol'
require_pattern 'br2_external_dcentos/board/zynq/am2-s17pro/rootfs-overlay/usr/sbin/sysupgrade' 'payload_fits_ubi_volume' 'am2-s17p sysupgrade checks payload byte fit before ubiupdatevol'
sysupgrade_am2_variant_parity_check
require_pattern 'scripts/pre_flash_validate.sh' 'DCENT_RELEASE_PUBKEY_FILE is required for authority-v1 package validation' 'pre-flash package-only mode requires trusted release key for signed authority'
require_pattern 'scripts/pre_flash_validate.sh' 'DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1' 'pre-flash package-only mode exposes explicit unsigned lab override'
require_pattern 'scripts/verify_sysupgrade_signature.sh' 'Public key is not a valid PEM public key' 'shell sysupgrade verifier rejects malformed placeholder public keys'
require_pattern 'scripts/pre_flash_validate.sh' 'dcentos.sysupgrade-unsigned-lab/v1)' 'pre-flash package-only mode recognizes the explicit unsigned lab profile'
require_pattern 'scripts/pre_flash_validate.sh' 'unsigned-lab/v1 requires exactly one status=lab_unsigned field' 'pre-flash package-only mode pins exact unsigned lab status'
require_pattern 'scripts/pre_flash_validate.sh' "exactly one 'status' authority field" 'pre-flash package-only mode requires one unambiguous status claim'
require_pattern 'scripts/pre_flash_validate.sh' 'status must not contain surrounding whitespace' 'pre-flash package-only mode rejects whitespace-padded status'
require_pattern 'scripts/pre_flash_validate.sh' 'version must not contain surrounding whitespace' 'pre-flash package-only mode rejects whitespace-padded version'
require_pattern 'scripts/pre_flash_validate.sh' 'unsigned-lab/v1 forbids MANIFEST.sig' 'pre-flash package-only mode rejects signatures in unsigned lab packages'
require_pattern 'scripts/build_in_docker.sh' 'release/verified builds fail closed on missing or mismatched toolchain' 'release docker builds make toolchain SHA verification mandatory'
require_file 'scripts/lib/sysupgrade_archive_admission.sh'
require_pattern 'scripts/lib/sysupgrade_archive_admission.sh' 'DCENT_SYSUPGRADE_ARCHIVE_MAX_MEMBERS=32' 'shell sysupgrade admission caps archive members before extraction'
require_pattern 'scripts/lib/sysupgrade_archive_admission.sh' 'unknown member leaf' 'shell sysupgrade admission rejects leaves outside the explicit package contract'
require_pattern 'scripts/verify_sysupgrade_signature.sh' 'dcent_sysupgrade_archive_admit "$PACKAGE" "$EXPECTED_BOARD" "$TMPDIR"' 'shell verifier runs shared archive admission before extraction/signature checks'
require_pattern 'scripts/verify_sysupgrade_signature.sh' 'for unsupported_chain_key in ota_intermediate_cert ota_revoked_intermediates' 'authority-v1 verifier restricts mutation authority to direct release-root signatures'
require_pattern 'scripts/verify_sysupgrade_signature.sh' 'certificate validity has no trusted-time authority on Zynq' 'authority-v1 verifier does not trust unauthenticated recovery wall time'
require_pattern 'scripts/verify_sysupgrade_signature.sh' 'Manifest authority-v1 forbids status=lab_unsigned' 'authority-v1 verifier rejects the signed/unsigned status contradiction'
require_pattern 'scripts/verify_sysupgrade_signature.sh' "exactly one 'status' field" 'authority-v1 verifier requires one unambiguous status claim'
require_pattern 'scripts/verify_sysupgrade_signature.sh' 'version must not contain surrounding whitespace' 'authority-v1 verifier rejects whitespace-padded version'
require_pattern 'scripts/build_in_docker.sh' 'ERROR (DEVOPS-002): no expected SHA256 pinned' 'release docker builds fail closed when no toolchain SHA pin exists'
require_file 'scripts/test_firmware_release_name.sh'
if sh scripts/test_firmware_release_name.sh >/dev/null 2>&1; then
    pass "firmware release-name helper self-test covers Antminer, ESP, H616, K230, and reject-unknown"
else
    fail "firmware release-name helper self-test failed"
fi
require_file 'scripts/check_stratum_contract_drift.py'
if run_python_script scripts/check_stratum_contract_drift.py >/dev/null 2>&1; then
    pass "stratum contract drift: current Whatsminer-first and Avalon-adapter assumptions are pinned"
else
    fail "stratum contract drift check failed"
fi
require_file 'scripts/check_family_docs_honesty.py'
if run_python_script scripts/check_family_docs_honesty.py >/dev/null 2>&1; then
    pass "family docs honesty: every support-matrix family has tier-boundary docs"
else
    fail "family docs honesty check failed"
fi
require_pattern 'Makefile' 'dcentrald-asic' 'make verify host tests execute dcentrald-asic behavioral safety tests'
require_pattern 'Makefile' 'dcentrald-thermal --no-default-features' 'make verify host tests execute dcentrald-thermal safety tests'
require_pattern '../../.github/workflows/dcentos-offline-gates.yml' 'cargo test -p dcentrald-asic --lib' 'offline workflow executes dcentrald-asic lib safety pins'
if grep -Eq 'dcentrald-hal --lib( --)? am2_' \
    '../../.github/workflows/dcentos-offline-gates.yml'; then
    pass 'offline workflow executes AM2 board-control UIO register-map pins'
else
    fail 'offline workflow is missing AM2 board-control UIO register-map pins'
fi
require_pattern '../../.github/workflows/dcentos-offline-gates.yml' 'cargo test -p dcentrald --test fan_safety_override_pin' 'offline workflow executes exact dcentrald fan safety override pin'
require_pattern 'Makefile' 'test-waves' 'make verify executes wave regression tests locally'
require_pattern 'Makefile' 'run_wave_regressions.sh' 'make test-waves delegates to the local wave regression runner'
require_pattern 'scripts/run_wave_regressions.sh' "name 'wave*.rs'" 'local wave regression runner discovers all dcentrald wave pins, including Wave-55i'
require_pattern 'dcentrald/dcentrald/tests/wave55i_phase0_full_ordering.rs' 'wave55i_phase0_full_ordering_rejects_swapped_phase0_markers' 'Wave-55i Phase-0 ordering pin carries a source-parse negative control'
require_pattern 'dcentrald/dcentrald/tests/i2c_eeprom_denylist_breadth.rs' 'am2_serial_pic_service' 'I2C EEPROM denylist breadth pin covers the AM2 serial PIC service'
require_pattern 'dcentrald/dcentrald/tests/i2c_eeprom_denylist_breadth.rs' 'denylist_breadth_helper_rejects_plain_i2c_service_constructor' 'I2C EEPROM denylist breadth pin carries a negative control'
require_pattern 'scripts/run_wave_regressions.sh' 'wave55l_loki_inter_txn_gap' 'local wave regression runner executes HAL wave55l Loki gap pin'
require_pattern 'scripts/run_wave_regressions.sh' 'watchdog::tests' 'local wave regression runner executes HAL watchdog Drop fail-closed pins'
require_pattern 'scripts/run_wave_regressions.sh' 'fan::tests' 'local wave regression runner executes HAL fan topology pins'
require_pattern 'scripts/run_wave_regressions.sh' 'xadc::tests' 'local wave regression runner executes HAL XADC non-finite fail-closed pins'
require_pattern 'scripts/run_wave_regressions.sh' 'safety_pwm_cap' 'local wave regression runner executes thermal safety_pwm_cap pin'
require_pattern 'dcentrald/dcentrald-asic/src/drivers/mod.rs' 'every_driver_core_count_matches_miner_profile_driver_semantics' 'ASIC driver core counts are pinned to MinerProfile driver-facing semantics'
require_pattern 'dcentrald/dcentrald-hal/src/fan.rs' 'fan_variant_topology_pins_physical_tach_and_pwm_channels' 'HAL fan variant topology pins physical/tach/PWM channel counts'
require_pattern 'dcentrald/dcentrald-thermal/src/supervisor.rs' 'hydro_configured_non_finite_inlet_fails_closed' 'thermal supervisor hydro NaN path fails closed'
require_pattern 'dcentrald/dcentrald-thermal/src/heater.rs' 'non_finite_power_never_yields_nan_or_boost' 'space-heater power loop rejects non-finite power'
require_pattern 'dcentrald/dcentrald-thermal/src/offgrid.rs' 'non_finite_power_and_current_do_not_poison_energy_or_telemetry' 'off-grid telemetry rejects non-finite current and power'
require_pattern 'dcentrald/dcentrald-thermal/src/curtailment.rs' 'sleep_controller_has_no_float_sensor_surface' 'curtailment sleep controller has no float sensor surface'
require_pattern 'dcentrald/dcentrald-hal/src/xadc.rs' 'iio_float_parser_rejects_non_finite_values' 'XADC parser rejects non-finite sysfs values'
require_file 'scripts/check_safety_clamp_manifest.py'
# BoardDesc install matrix (ADR-0011) — living product/lab/A/B SSOT for packaging.
require_file 'docs/architecture/install_matrix.tsv'
require_file 'docs/architecture/hardware_enablement_matrix.json'
require_file '../dcent-toolbox/src/dcent_toolbox/data/hardware_enablement_matrix.json'
require_file 'scripts/export_install_matrix.ps1'
require_file 'scripts/check_install_matrix_drift.ps1'
if awk -F '\t' '
    NR == 1 {
        for (i = 1; i <= NF; i++) {
            if ($i == "board_target") board_target_col = i
            if ($i == "install_authorization") install_authorization_col = i
            if ($i == "public_beta") public_beta_col = i
        }
        next
    }
    board_target_col && install_authorization_col && public_beta_col && \
        $install_authorization_col == "public_beta" && $public_beta_col == "1" {
        if ($board_target_col == "am1-s9") am1_s9 = 1
        if ($board_target_col == "am2-s19j") am2_s19j = 1
    }
    END {
        exit !(board_target_col && install_authorization_col && public_beta_col && \
            am1_s9 && am2_s19j)
    }
' docs/architecture/install_matrix.tsv; then
    pass 'install_matrix.tsv lists public-beta runtime board targets am1-s9 and am2-s19j'
else
    fail 'install_matrix.tsv missing public-beta runtime rows am1-s9 / am2-s19j'
fi
if awk -F'\t' '
    NR == 1 {
        for (i = 1; i <= NF; i++) if ($i == "public_beta") public_beta_col = i
        next
    }
    public_beta_col && $public_beta_col == "1" { count++ }
    END { exit !(public_beta_col && count == 2) }
' docs/architecture/install_matrix.tsv; then
    pass 'install_matrix.tsv has exactly two public_beta=1 rows'
else
    fail 'install_matrix.tsv public_beta=1 row count is not exactly 2'
fi
if cmp -s \
    docs/architecture/hardware_enablement_matrix.json \
    ../dcent-toolbox/src/dcent_toolbox/data/hardware_enablement_matrix.json; then
    pass 'Toolbox bundles the exact generated hardware enablement matrix'
else
    fail 'Toolbox hardware enablement matrix drifted from the Rust-generated canonical JSON'
fi
if run_python_script scripts/check_safety_clamp_manifest.py --self-test; then
    pass "safety clamp manifest: classified thermal/voltage/frequency/PWM clamp set is pinned with negative control"
else
    fail "safety clamp manifest: classified clamp set drifted or negative control failed"
fi
require_pattern 'scripts/run_all_gates.sh' 'dcentrald-asic' 'run_all_gates host tests execute dcentrald-asic behavioral safety tests'
require_pattern 'scripts/run_all_gates.sh' 'dcentrald-thermal --no-default-features' 'run_all_gates host tests execute dcentrald-thermal safety tests'
require_pattern 'Makefile' 'test-clippy-input' 'make verify runs input-crate clippy deny gate'
require_pattern 'Makefile' 'clippy --no-deps -p dcentrald-stratum --lib -- -D clippy::unwrap_used -D clippy::expect_used -D clippy::panic' 'stratum clippy deny command is pinned in make verify'
require_pattern 'Makefile' 'clippy --no-deps -p dcentrald-asic --lib -- -D clippy::unwrap_used -D clippy::expect_used -D clippy::panic' 'asic clippy deny command is pinned in make verify'
make_release_verify_gate_check
precommit_skip_after_hygiene_check
require_pattern 'Makefile' 'hook-only emergency bypass' 'install-hooks documents DCENT_SKIP_VERIFY as hook-only'
require_pattern 'README.md' 'make install-hooks' 'source onboarding documents local hook installation'
require_pattern 'scripts/run_all_gates.sh' 'rust-input-clippy' 'run_all_gates runs input-crate clippy deny gate'
require_pattern 'dcentrald/dcentrald-thermal/src/controller.rs' 'safe_pwm_clamp' 'thermal controller uses adjacent safe PWM clamp helper'
require_pattern 'dcentrald/dcentrald/src/stock_mining.rs' 'spawn_watchdog_kicker' 'stock-fpga mining path arms the watchdog'
require_pattern 'dcentrald/dcentrald/src/main.rs' 'stock-fpga' 'watchdog source-pin list includes stock-fpga mining path'
require_pattern 'dcentrald/Cargo.toml' 'proptest = "1"' 'workspace declares proptest for untrusted-input property tests'
require_pattern 'dcentrald/dcentrald-stratum/src/v2/channel.rs' 'handle_frame_never_panics_on_bounded_payloads' 'SV2 channel handle_frame property coverage is present'
require_pattern 'dcentrald/dcentrald-stratum/src/v2/noise.rs' 'initiator_handshake_finish_never_panics_on_arbitrary_response' 'SV2 Noise handshake property coverage is present'
require_pattern 'dcentrald/dcentrald-stratum/src/v2/jd.rs' 'jd_message_decoders_never_panic_on_arbitrary_bytes' 'SV2 JD parser property coverage is present'
require_pattern 'dcentrald/fuzz/Cargo.toml' 'ota_sysupgrade_tar' 'cargo-fuzz OTA sysupgrade tar target is declared'
require_pattern 'dcentrald/fuzz/Cargo.toml' 'sv2_frame_decoder' 'cargo-fuzz SV2 frame decoder target is declared'
require_pattern 'dcentrald/fuzz/Cargo.toml' 'v1_pool_message_parser' 'cargo-fuzz V1 pool-message parser target is declared'
require_pattern '../../.github/workflows/dcentos-fuzz-smoke.yml' 'cargo fuzz run ota_sysupgrade_tar -- -runs=256' 'scheduled fuzz smoke runs OTA tar parser'
require_pattern '../../.github/workflows/dcentos-fuzz-smoke.yml' 'cargo fuzz run sv2_frame_decoder -- -runs=256' 'scheduled fuzz smoke runs SV2 frame decoder'
require_pattern '../../.github/workflows/dcentos-fuzz-smoke.yml' 'cargo fuzz run v1_pool_message_parser -- -runs=256' 'scheduled fuzz smoke runs V1 pool-message parser'
require_pattern '../../.github/workflows/dcentos-fuzz-smoke.yml' 'dcentos_receipt_parser_fuzz.c' 'scheduled fuzz smoke builds the compiled receipt ABI1 target'
require_pattern '../../.github/workflows/dcentos-fuzz-smoke.yml' 'dcentos-receipt-parser-fuzz' 'scheduled fuzz smoke runs compiled receipt ABI1 parsers'
require_pattern '../../.github/workflows/dcentos-fuzz-smoke.yml' 'dcentos_receipt_chain_fuzz.c' 'scheduled fuzz smoke builds the compiled receipt chain accumulator target'
require_pattern '../../.github/workflows/dcentos-fuzz-smoke.yml' 'dcentos-receipt-chain-fuzz' 'scheduled fuzz smoke runs compiled receipt chain accumulators'
require_pattern '../../.github/workflows/dcentos-fuzz-smoke.yml' 'scripts/fuzz/corpus/dcentos-receipt-chain' 'scheduled chain fuzz smoke uses the canonical valid-chain seed corpus'
require_pattern '../../.github/workflows/dcentos-fuzz-smoke.yml' 'dcentos_receipt_storage_fuzz.c' 'scheduled fuzz smoke builds the compiled receipt ABI2 storage target'
require_pattern '../../.github/workflows/dcentos-fuzz-smoke.yml' 'dcentos-receipt-storage-fuzz' 'scheduled fuzz smoke runs compiled receipt ABI2 storage validation'
require_pattern '../../.github/workflows/dcentos-fuzz-smoke.yml' 'scripts/fuzz/corpus/dcentos-receipt-storage' 'scheduled storage fuzz smoke uses the canonical valid-genesis seed corpus'
require_pattern '../../.github/workflows/dcentos-fuzz-smoke.yml' '-max_len=20498' 'scheduled storage fuzz smoke can reach the full framed ABI2 record boundary'
require_pattern 'scripts/test_dcentos_receipt_cross_compile.sh' 'receipt_projection.c' 'exact Zynq cross proof compiles the global receipt projection engine'
require_pattern 'scripts/test_dcentos_receipt_cross_compile.sh' 'projection=${projection_bytes}B' 'exact Zynq cross proof measures the stripped projection target'

# The entropy lifecycle is a boot-security boundary, not an implementation
# detail of one updater. Keep its host state-machine proof independent of the
# larger Experimental sysupgrade transaction. The full offline gate and the
# restricted-input image workflow own exact-target compilation; --static-only
# retains wiring checks without requiring the nonredistributable toolchain.
entropy_seed_lifecycle_check() {
    if [ ! -e br2_external_dcentos/board/zynq/rootfs-overlay/usr/sbin/seed-entropy ] && \
       [ -f br2_external_dcentos/packages/seed-entropy/src/seed-entropy.c ] && \
       grep -Fq 'BR2_PACKAGE_SEED_ENTROPY=y' \
           br2_external_dcentos/configs/dcentos-common.fragment; then
        pass 'entropy lifecycle: package source is enabled globally and has no overlay shadow'
    else
        fail 'entropy lifecycle: package wiring is absent or a legacy overlay implementation shadows the package binary'
    fi
    if [ "$STATIC_ONLY" -eq 0 ]; then
        if sh scripts/test_seed_entropy_lifecycle.sh >/dev/null 2>&1; then
            pass 'entropy lifecycle: native consume/mix/credit/rotation state machine passes'
        else
            fail 'entropy lifecycle: native state-machine suite failed'
        fi
        if sh scripts/test_seed_entropy_cross_compile.sh >/dev/null 2>&1; then
            pass 'entropy lifecycle: exact pinned Zynq ABI cross proof passes'
        else
            fail 'entropy lifecycle: exact pinned Zynq ABI cross proof failed or its restricted input is absent'
        fi
    fi
    if awk '
        /- name: Provision and admit restricted inputs/ { provision = NR }
        /- name: Exact Zynq entropy lifecycle cross-compile proof/ { proof = NR }
        /- name: Build and atomically publish S9 release set/ { build = NR }
        END { exit !(provision && proof && build && provision < proof && proof < build) }
    ' ../../.github/workflows/dcentos-image-smoke.yml && \
       grep -Eq '^[[:space:]]*run:[[:space:]]+sh[[:space:]]+scripts/test_seed_entropy_cross_compile\.sh[[:space:]]*$' \
           ../../.github/workflows/dcentos-image-smoke.yml; then
        pass 'entropy lifecycle: exact Zynq proof is ordered after input admission and before image build'
    else
        fail 'entropy lifecycle: restricted-input image smoke lost the ordered exact Zynq proof'
    fi
}
entropy_seed_lifecycle_check

# Release-workflow admission anti-orphan. `build_in_docker.sh` and
# `build-dcentrald.sh` are capsule-internal drivers: a workflow calling either
# one directly can no longer satisfy receipt-v4 invocation ownership. S9 CI
# must consume the atomic public directory and verify it after private cleanup.
# AM2 has no capsule yet, so its former package/nandsim claims stay explicit and
# fail closed instead of falling back to the mutable inner driver.
release_workflow_capsule_admission_check() {
    workflow_dir='../../.github/workflows'
    image_workflow="$workflow_dir/dcentos-image-smoke.yml"
    nandsim_workflow="$workflow_dir/dcentos-offline-nandsim.yml"

    require_file "$image_workflow"
    require_file "$nandsim_workflow"

    direct_call_pattern='(^|[[:space:]])(bash|sh)[[:space:]]+(\./)?scripts/(build_in_docker|build-dcentrald)\.sh([[:space:]\\]|$)'
    direct_calls=$(grep -REn -- "$direct_call_pattern" "$workflow_dir" 2>/dev/null || true)
    if [ -n "$direct_calls" ]; then
        fail "release workflows must not invoke capsule-internal build drivers directly: $direct_calls"
    else
        pass 'release workflows cannot orphan the outer capsule by invoking inner build drivers'
    fi

    for workflow in "$image_workflow" "$nandsim_workflow"; do
        require_pattern "$workflow" \
            'bash scripts/build_s9_release_capsule.sh' \
            "$workflow uses the admitted S9 outer capsule"
        require_pattern "$workflow" \
            'python3 scripts/portable_release_evidence.py verify' \
            "$workflow verifies the exact published directory after cleanup"
        require_pattern "$workflow" \
            'vars.DCENT_RUST_BUILDER_BASE' \
            "$workflow obtains the builder digest from repository/dispatch authority"
        require_pattern "$workflow" \
            "grep -Eq '^.+@sha256:[0-9a-f]{64}$'" \
            "$workflow rejects missing or mutable builder references"
        require_pattern "$workflow" \
            "DCENT_TOOLCHAIN_SHA256_VERIFIED: '1'" \
            "$workflow explicitly admits the ratified S9 toolchain checksum"
        require_pattern "$workflow" \
            'vars.DCENT_BUILD_INPUTS_DIR' \
            "$workflow obtains an explicit restricted-input channel authority"
        require_pattern "$workflow" \
            'runs-on: [self-hosted, linux, x64, dcentos-restricted-inputs' \
            "$workflow reserves capsule execution for an operator-managed restricted-input runner"
        require_pattern "$workflow" \
            'sh scripts/provision_build_inputs.sh --source "$INPUT_ROOT"' \
            "$workflow provisions restricted bytes only from the external authority"
        require_pattern "$workflow" \
            'sh scripts/provision_build_inputs.sh --check' \
            "$workflow hash-verifies every restricted input before build"
        require_pattern "$workflow" \
            'DCENT_BUILD_INPUTS_DIR must be outside the Actions checkout.' \
            "$workflow rejects a checkout-local pseudo-provisioning channel"
        require_pattern "$workflow" \
            'DCENT_AM2_CAPSULE_STATUS: unavailable-fail-closed' \
            "$workflow exposes the missing AM2 capsule as a coverage disposition"
        require_pattern "$workflow" \
            '[ -e scripts/build_am2_release_capsule.sh ]' \
            "$workflow forces review when an AM2 capsule becomes available"
        reject_pattern "$workflow" \
            'bash scripts/build_am2_release_capsule.sh' \
            "$workflow does not claim an unimplemented AM2 capsule"
        reject_pattern "$workflow" \
            '  push:' \
            "$workflow does not schedule an always-blocked hosted push build"
        reject_pattern "$workflow" \
            '  pull_request:' \
            "$workflow does not schedule an always-blocked hosted pull-request build"
    done

    require_pattern "$image_workflow" \
        '--test release_artifact_contract' \
        'image-smoke executes the public OTA artifact verifier on the capsule artifact'
    require_pattern "$image_workflow" \
        'built_release_artifact_passes_public_ota_contract' \
        'image-smoke invokes the ignored real-artifact contract test explicitly'
    reject_pattern "$image_workflow" \
        'target: am2-s19jpro' \
        'image-smoke does not claim AM2 package coverage without an admitted capsule'
    reject_pattern "$image_workflow" \
        'target: cv1835-s19jpro' \
        'image-smoke does not advertise the unpinned CV1835 release lane'
    require_pattern "$nandsim_workflow" \
        '--target am1-s9' \
        'nandsim runs only the capsule-backed S9 target'
    reject_pattern "$nandsim_workflow" \
        '--target both' \
        'nandsim does not claim AM2 coverage without an admitted package producer'
}
release_workflow_capsule_admission_check

require_pattern 'scripts/build_in_docker.sh' 'dcent_prepare_git_release_provenance' 'container build validates provenance against the source worktree'
require_pattern 'scripts/build_in_docker.sh' '-e SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH"' 'container build passes the canonical epoch into image packaging'
require_file 'scripts/release_envelope_archive.py'
require_pattern 'scripts/release_envelope_archive.py' 'atomic_publish(' 'release envelope archive uses exact no-replace durable publication'
require_pattern 'scripts/release_envelope_archive.py' 'snapshot_without_mutation(root) != before' 'release envelope archive detects source mutation while tar reads'
reject_pattern 'scripts/lib/release_envelope.sh' 'rm -f --' 'release envelope library has no pathname-only deletion authority'
reject_pattern 'scripts/build_in_docker.sh' 'dcent_release_remove_publication' 'capsule inner driver never deletes unowned publication pathnames'
require_pattern 'scripts/lib/release_envelope.sh' 'DCENT_CAPSULE_PROVENANCE_VERIFIED' 'exact snapshot provenance requires verified capsule context'
require_pattern 'scripts/lib/release_envelope.sh' 'verify-against-git' 'exact snapshot provenance is reverified against mounted Git objects'
require_pattern 'scripts/lib/release_envelope.sh' 'release-root signing requires DCENT_RELEASE_IMAGE=1 hardening' 'release-root signing implies release-image hardening'
require_file 'scripts/sign_release_artifact.py'
require_file 'scripts/test_sign_release_artifact.sh'
if sh scripts/test_sign_release_artifact.sh >/dev/null 2>&1; then
    pass 'release artifact signer pins, verifies, and durably publishes exact bytes'
else
    fail 'release artifact signer selftest failed'
fi
require_pattern 'scripts/sign_release_artifact.py' 'durable_input=True' 'release artifact signer flushes exact artifact bytes before signature commit'
require_pattern 'scripts/build_in_docker.sh' 'sign_release_artifact.py' 'AM3 tar signing reuses the exact durable signing lifecycle'
require_pattern 'scripts/build_s9_release_capsule.sh' 'sign_release_artifact.py" "$PORTABLE_EVIDENCE_PATH"' 'portable evidence signing reuses the exact durable signing lifecycle'
require_pattern 'scripts/lib/sysupgrade_package_common.sh' 'sign_release_artifact.py' 'shared sysupgrade manifest signing reuses the exact durable signing lifecycle'
require_pattern 'scripts/package_sysupgrade.sh' 'sign_release_artifact.py' 'standalone sysupgrade manifest signing reuses the exact durable signing lifecycle'
reject_pattern 'scripts/lib/sysupgrade_package_common.sh' 'openssl pkeyutl -sign -rawin' 'shared sysupgrade manifest signing never truncates its signature output'
reject_pattern 'scripts/package_sysupgrade.sh' 'openssl pkeyutl -sign -rawin' 'standalone sysupgrade manifest signing never truncates its signature output'
require_file 'scripts/test_sign_stock_manifest.sh'
if sh scripts/test_sign_stock_manifest.sh >/dev/null 2>&1; then
    pass 'stock manifest signer produces a trusted no-replace candidate'
else
    fail 'stock manifest signer selftest failed'
fi
require_pattern 'scripts/sign_stock_manifest.sh' 'sign_release_artifact.py' 'stock manifest signing reuses the exact durable signing lifecycle'
require_pattern 'scripts/sign_stock_manifest.sh' 'DCENT_RELEASE_PUBKEY_FILE' 'stock manifest signing requires a trusted public key'
reject_pattern 'scripts/sign_stock_manifest.sh' 'pkeyutl -sign' 'stock manifest signing never truncates a tracked signature placeholder'
require_file 'scripts/test_sign_release_dry_run.sh'
if sh scripts/test_sign_release_dry_run.sh >/dev/null 2>&1; then
    pass 'release signing rehearsal exercises the exact signer end to end'
else
    fail 'release signing rehearsal selftest failed'
fi
require_pattern 'scripts/sign_release_dry_run.sh' 'sign_release_artifact.py' 'release signing rehearsal reuses the exact durable signing lifecycle'
reject_pattern 'scripts/sign_release_dry_run.sh' 'pkeyutl -sign' 'release signing rehearsal never truncates its signature output'
require_pattern 'scripts/verify_sysupgrade_signature.sh' 'select_manifest_python' 'host verifier probes a runnable Python interpreter'
require_pattern 'scripts/verify_sysupgrade_signature.sh' 'run_manifest_python "$MANIFEST_JSON_HELPER" validate' 'host verifier uses the probed Python interpreter for semantic admission'
reject_pattern 'scripts/build_in_docker.sh' 'openssl pkey -in /signkey -pubout' 'AM3 tar signing never derives its own trust root'
require_pattern 'scripts/package_sysupgrade.sh' 'dcent_create_deterministic_tar' 'S9 sysupgrade uses the deterministic envelope archiver'
require_pattern 'br2_external_dcentos/board/zynq/am2-s19jpro/post-image.sh' 'dcent_create_deterministic_tar' 'AM2 sysupgrade uses the deterministic envelope archiver'
require_pattern 'br2_external_dcentos/board/cvitek/cv1835-s19jpro/post-image.sh' 'exit 78' 'CV1835 post-image hook refuses every artifact build'
if awk '
    /dcent_write_sysupgrade_manifest/ { rewrite = NR }
    /dcent_create_deterministic_tar/ { archive = NR }
    END { exit !(rewrite > 0 && archive > rewrite) }
' 'br2_external_dcentos/board/zynq/am2-s19jpro/post-image.sh'; then
    pass 'AM2 shared final-manifest rewrite occurs before deterministic packaging'
else
    fail 'AM2 must rewrite/sign the canonical shared manifest before deterministic packaging'
fi
require_pattern 'dcentrald/dcentrald-api/tests/release_artifact_contract.rs' 'verify_sysupgrade_bundle(&artifact, false, Some(&public_key))' 'real-artifact test calls the public fail-closed OTA verifier'
require_pattern 'dcentrald/dcentrald-stratum/src/v1/messages.rs' 'parse_pool_message_never_panics_on_arbitrary_or_malformed_input' 'V1 pool-message parser has malformed-input panic coverage'
require_pattern 'dcentrald/dcentrald-api/src/cgminer.rs' 'cgminer_shared_toolbox_contract_fixture_matches_dispatcher' 'cgminer dispatcher is pinned to shared toolbox contract fixture'
require_pattern '../dcent-toolbox/tests/test_cgminer_shape.py' 'test_dcentos_shared_cgminer_contract_fixture_parses_like_toolbox_expects' 'toolbox parses the shared dcentos cgminer contract fixture'
require_pattern 'dcentrald/dcentrald-api/tests/cgminer_luxos_routes.rs' 'api1_batch_with_mutation_is_invalid_even_loopback' 'cgminer API rejects mutating batch requests from loopback'
require_pattern 'dcentrald/dcentrald-api/tests/cgminer_luxos_routes.rs' 'api1_batch_restart_from_lan_peer_refused' 'cgminer API rejects mutating batch requests from LAN peers'
require_pattern 'dcentrald/dcentrald-stratum/src/v1/client.rs' 'fov6_config_drive_arm_advances_current_pool_index' 'failover simulation pins drive-mode arm transition'
require_pattern 'dcentrald/dcentrald-stratum/src/v1/client.rs' 'fov6_shadow_only_does_not_change_current_pool_index' 'failover simulation pins shadow-only no-op transition'
require_pattern 'dcentrald/dcentrald-stratum/src/v1/client.rs' 'fov6_production_triggers_do_not_advance_under_drive' 'failover simulation pins production-trigger guardrail under drive mode'
require_pattern 'dcentrald/dcentrald-common/src/wallet_mask.rs' 'wallet_mask_helpers_never_panic_on_arbitrary_text' 'wallet-mask helpers have arbitrary-text panic coverage'
require_pattern 'dcentrald/dcentrald-api/src/webhook.rs' 'redaction_is_applied_before_every_channel_render' 'webhook rendering applies redaction before every channel'
require_pattern 'dcentrald/dcentrald-api/src/websocket.rs' 'ws_stats_frame_masks_donation_url_and_worker' 'websocket stats frames mask donation URL and worker'
require_pattern 'dcentrald/dcentrald-stratum/src/v1/client.rs' 'failover_status_reports_active_pool_without_secrets' 'failover telemetry reports active pool without credentials'
require_pattern 'dcentrald/dcentrald/src/runtime/notifications.rs' 'mapping_then_redact_yields_clean_webhook_event' 'runtime notifications map then redact webhook events'
require_pattern 'dcentrald/dcentrald-asic/src/lib.rs' 'deterministic_mock_chain_mini_soak_covers_share_failover_and_ota_preflight' 'MockChain mini-soak executes in dcentrald-asic host tests'
require_pattern '../dcentos-esp/dcentaxe-hal/src/board.rs' 'board_version_deep_parity_pins_power_and_support_attributes' 'ESP board-version deep parity pins power/fan/temp/support attributes'
require_pattern '../dcentos-esp/dcentaxe-hal/src/board.rs' 'every_model_has_explicit_default_board_version' 'ESP every BitAxeModel has an explicit default board-version pin'
require_pattern '../dcentos-esp/knowledge-base/upstream/esp-miner/fixture_manifest.json' 'last_synced_on' 'ESP-Miner fixture manifest records last sync date'
require_pattern '../dcentos-esp/knowledge-base/upstream/esp-miner/fixture_manifest.json' 'no_network_fetch_in_ci' 'ESP-Miner fixture drift gate stays source-only'
require_pattern '../dcentos-esp/docs/DCENT_AXE_OPERATOR_BENCH_RUNBOOK.md' 'ESP-9: ESP-Miner Fixture Sync Review' 'operator runbook carries ESP-Miner fixture sync checklist'
require_pattern '../../.github/workflows/bitaxe-build-matrix.yml' './scripts/build-matrix.sh' 'root bitaxe build workflow runs the ESP public build matrix'
require_pattern '../../.github/workflows/bitaxe-build-matrix.yml' 'WAVE9D7_XTENSA_ICE_QUARANTINE.md' 'root bitaxe build workflow points to xtensa ICE quarantine policy'
require_pattern '../../.github/workflows/dcentos-esp-release.yml' 'esp-rs/xtensa-toolchain@v1' 'ESP release workflow uses the xtensa toolchain for public releases'
require_pattern '../../.github/workflows/dcentos-esp-release.yml' 'xtensa-esp32s3-espidf' 'ESP release workflow pins xtensa target environment'
require_pattern '../dcentos-esp/docs/WAVE9D7_XTENSA_ICE_QUARANTINE.md' 'Wave 9D7' 'Wave 9D7 xtensa ICE quarantine policy is documented'
require_pattern '../dcentos-esp/docs/WAVE9D7_XTENSA_ICE_QUARANTINE.md' 'Public targets must not be silently skipped' 'xtensa quarantine policy forbids silent public-target skips'
require_pattern '../dcentos-esp/dcentaxe-hal/src/power_convert.rs' 'ds4432u_operator_bench_measurements_accept_meter_log' 'DS4432U ignored operator bench harness is present'
require_pattern '../dcentos-esp/docs/DCENT_AXE_OPERATOR_BENCH_RUNBOOK.md' 'DCENT_DS4432U_BENCH_MV_CSV' 'operator runbook documents the DS4432U bench harness input'
require_pattern '../dcent-toolbox/src/dcent_toolbox/core/install_package.py' 'def signature_status' 'toolbox install package exposes a central signature_status'
require_pattern '../dcent-toolbox/src/dcent_toolbox/core/install_package.py' 'board_identity_signed' 'toolbox install package tracks board-bound signature identity'
require_pattern '../dcent-toolbox/src/dcent_toolbox/core/installer.py' '_sig_status != "signed"' 'toolbox target-sysupgrade executor refuses unsigned packages by default'
require_pattern '../dcent-toolbox/src/dcent_toolbox/core/installer.py' 'allow_unsigned_lab=_allow_unsigned_lab' 'toolbox target-sysupgrade writer receives only explicit unsigned-lab override'
require_pattern '../dcent-toolbox/tests/test_install_signing_gate.py' 'test_unsigned_package_blocks_plan' 'toolbox planner signature gate blocks unsigned packages'
require_pattern '../dcent-toolbox/tests/test_install_signing_gate.py' 'test_unverifiable_package_blocks_plan' 'toolbox planner signature gate blocks unverifiable packages'
require_pattern '../dcent-toolbox/tests/test_install_inline_signature_board_binding.py' 'test_detached_manifest_sig_is_board_bound_and_relabel_invalidates' 'toolbox package signature tests pin detached manifest board binding'
require_pattern '../dcent-toolbox/tests/test_install_inline_signature_board_binding.py' 'test_unsigned_package_is_not_board_bound' 'toolbox package signature tests pin unsigned packages as unbound'
require_pattern '../dcent-toolbox/tests/test_install_w15_am2_persistent_lab.py' 'test_target_sysupgrade_refuses_unsigned_package_by_default' 'toolbox executor test refuses unsigned target sysupgrade packages'
require_pattern '../dcent-toolbox/tests/test_install_w15_am2_persistent_lab.py' 'writer must NOT be called for an unsigned package (Gate 8a)' 'toolbox executor test pins writer not called for unsigned packages'
require_pattern '../dcent-toolbox/tests/test_adv04_write_path_review.py' 'test_toolbox_target_sysupgrade_keeps_restore_signature_recovery_gates' 'toolbox source-order review pins signature gate before target-sysupgrade writer'
require_pattern '../dcent-toolbox/tests/test_sysupgrade_guard_execution.py' 'AM2_SYSUPGRADE_VARIANTS' 'toolbox sysupgrade guard tests enumerate AM2 variant overlays'
require_pattern '../dcent-toolbox/tests/test_sysupgrade_guard_execution.py' 'test_sysupgrade_t_executes_wrong_board_brick_guard' 'toolbox sysupgrade tests pin wrong-board refusal'
require_pattern '../dcent-toolbox/tests/test_sysupgrade_guard_execution.py' 'test_sysupgrade_t_refuses_shorter_board_near_miss_prefix' 'toolbox sysupgrade tests pin near-miss board prefix refusal'
require_pattern '../dcent-toolbox/tests/test_sysupgrade_guard_execution.py' 'test_sysupgrade_t_accepts_am2_per_unit_board_prefix_variant' 'toolbox sysupgrade tests pin intended AM2 per-unit board variant acceptance'
require_pattern 'docs/reviews/2026-07-05-wave7-uncovered-surfaces-audit.md' 'W7-R1: Narrow Pre-setup Recovery GET Exposure' 'Wave 7 audit records recovery pre-setup GET exposure follow-up'
require_pattern 'docs/reviews/2026-07-05-wave7-uncovered-surfaces-audit.md' 'W7-R2: Stock Restore Archive Trust Boundary' 'Wave 7 audit records stock restore archive trust-boundary follow-up'
require_pattern 'docs/reviews/2026-07-05-wave7-uncovered-surfaces-audit.md' 'W7-D1: Fsync Metrics CSV Exports' 'Wave 7 audit records metrics CSV persistence follow-up'
require_pattern 'docs/reviews/2026-07-05-wave7-uncovered-surfaces-audit.md' 'W7-D2: Fix Persistent Log Ring Cursor Commit' 'Wave 7 audit records persistent log ring cursor follow-up'
require_pattern 'docs/reviews/2026-07-05-wave7-uncovered-surfaces-audit.md' 'W7-D3: Fsync Auto-recovery Ladder State' 'Wave 7 audit records auto-recovery ladder persistence follow-up'
require_pattern 'docs/reviews/2026-07-05-wave7-uncovered-surfaces-audit.md' 'W7-L1: Constrain `/data/logrotate.conf`' 'Wave 7 audit records logrotate override bounds follow-up'
require_pattern 'docs/reviews/2026-07-05-wave7-uncovered-surfaces-audit.md' 'W7-L2: Keep Runtime State on tmpfs' 'Wave 7 audit records runtime tmpfs layout follow-up'
require_pattern 'dcentrald/dcentrald-api/src/rest.rs' 'mod late;' 'Wave 8 rest.rs decomposition keeps late route handlers in a child module'
require_pattern 'dcentrald/dcentrald-api/src/rest/late.rs' 'mounted_router_path_snapshot_is_explicit' 'Wave 8 route-table snapshot test is present'
require_pattern 'dcentrald/dcentrald-api/src/rest/route_paths_snapshot.txt' '/api/system/restore-to-stock/preflight-checks' 'Wave 8 route snapshot includes recovery routes'

rest_rs_decomposition_shape_check() {
    f='dcentrald/dcentrald-api/src/rest.rs'
    require_file "$f"
    [ -f "$f" ] || return
    _lines=$(wc -l < "$f" | tr -d '[:space:]')
    if [ "$_lines" -lt 10000 ]; then
        pass "Wave 8 rest.rs decomposition: $f is below 10000 lines ($_lines)"
    else
        fail "Wave 8 rest.rs decomposition: $f has $_lines lines (must stay below 10000)"
    fi
}
rest_rs_decomposition_shape_check

browser_sysupgrade_upload_signature_gate() {
    _rest='dcentrald/dcentrald-api/src/rest/late.rs'
    require_file "$_rest"
    _line=$(grep -n 'verify_sysupgrade_bundle(' "$_rest" 2>/dev/null | tail -n 1 | cut -d: -f1 || true)
    if [ -z "$_line" ]; then
        fail "browser sysupgrade upload path calls verify_sysupgrade_bundle"
        return
    fi
    _end=$((_line + 10))
    _call=$(sed -n "${_line},${_end}p" "$_rest")
    case "$_call" in
        *'false,'*'SYSTEM_UPGRADE_RELEASE_PUBKEY'*)
            pass "browser sysupgrade upload path hardcodes allow_unsigned=false and the on-disk release key"
            ;;
        *)
            fail "browser sysupgrade upload path must call verify_sysupgrade_bundle(..., false, Some(SYSTEM_UPGRADE_RELEASE_PUBKEY))"
            ;;
    esac
}
browser_sysupgrade_upload_signature_gate

# PH-1 (): /api/stratum/protocol prose must not re-introduce the SV2
# overclaim. The test-pinned firmware_stratum_matrix sets V1=Default, SV2=OptIn,
# and the pool config default is sv1 — so SV2 is an opt-in client, not the
# default, and there is no live SV2 accepted-share proof yet. Ban the two
# unambiguous overclaim phrases so a future edit can't silently re-add them.
reject_pattern 'dcentrald/dcentrald-api/src/rest.rs' 'only flavor defaulting to SV2' 'stratum protocol prose does not claim SV2 is the default (V1 is)'
reject_pattern 'dcentrald/dcentrald-api/src/rest.rs' 'supports Stratum V2 end-to-end' 'stratum protocol prose does not claim SV2 end-to-end (live proof pending)'
reject_pattern 'dcentrald/dcentrald/src/daemon.rs' 'Universal Hash Board Compatibility ACTIVE' 'daemon runtime strings do not advertise universal hash-board compatibility'
reject_pattern 'dcentrald/dcentrald/src/daemon.rs' 'any hash board generation' 'daemon runtime strings do not claim any-generation hash-board support'
reject_pattern 'dcentrald/dcentrald/src/daemon.rs' 'No competitor does this' 'daemon runtime strings do not carry competitor overclaims'
require_pattern 'dcentrald/dcentrald/src/daemon.rs' 'hash board auto-detected by ChipID (broad Zynq-era support)' 'daemon runtime strings keep the neutral ChipID support wording'
open_core_override_scope_check() {
    _hits=$(
        grep -RIn 'fn send_open_core_work' dcentrald/dcentrald-asic/src/drivers 2>/dev/null \
            | grep -Ev 'drivers/(mod|bm1387|bm1398)\.rs:' || true
    )
    if [ -n "$_hits" ]; then
        echo "$_hits"
        fail "open-core override scope: only BM1387 and BM1398 may override send_open_core_work without a bench-backed allowlist update"
    else
        pass "open-core override scope: BM1362/BM1366/BM1368/BM1370 inherit the default no-op"
    fi
}
open_core_override_scope_check
require_pattern 'dcentrald/dcentrald-asic/src/drivers/bm1398.rs' 'bm139x_open_core_enable_value_matches_jig' 'BM1398 open-core value stays pinned to the BM1397 jig formula'
require_pattern 'dcentrald/dcentrald-asic/src/drivers/bm1398.rs' 'if !bm139x_open_core_enabled()' 'BM1398 open-core sweep remains default-off behind its env gate'
require_pattern 'dcentrald/dcentrald-api-types/src/lib.rs' 'pub mod api_error_codes' 'API REST error-code vocabulary is centralized and stable'
require_pattern 'dcentrald/dcentrald-api/src/rest/late.rs' 'api_error_mapper_wraps_bare_text_and_json_string_only' 'API error mapper pins machine-readable codes for legacy bodies'
require_pattern 'dcentrald/dcentrald-api/src/rest/late.rs' 'donation_config_route_rejects_bad_percent' 'config validation errors expose a stable CONFIG_VALIDATION code'
require_pattern 'dcentrald/dcentrald-api/src/rest/late.rs' 'config_update_validation_matrix_rejects_known_bad_inputs' 'config update validation matrix covers known bad inputs'
require_pattern 'dcentrald/dcentrald-api/src/rest/late.rs' 'config_update_merge_never_panics_on_bounded_json_patch' 'config update merge has bounded arbitrary-patch panic coverage'
require_pattern 'dcentrald/dcentrald-api/src/auth.rs' 'persisted_session_survives_daemon_restart_idle_map_reset' 'auth sessions survive daemon restart without reviving expired sessions'
require_pattern 'dcentrald/dcentrald/src/config.rs' 'http_bind = "miner.local"' 'api.http_bind validation rejects non-IP hostnames'
require_pattern 'dcentrald/dcentrald/src/config.rs' 'api_http_bind_defaults_to_existing_lan_visible_bind' 'api.http_bind default preserves LAN-visible dashboard bind'
require_pattern 'dcentrald/dcentrald-api/src/lib.rs' 'http_bind_addr_preserves_default_and_accepts_loopback_override' 'HTTP bind helper preserves default and loopback override contracts'
require_pattern 'dcentrald/dcentrald-api/src/lib.rs' 'state.config.websocket_tickets' 'API startup propagates the websocket ticket compatibility flag'
require_pattern 'dcentrald/dcentrald-api/src/rest.rs' '/api/auth/ws-ticket' 'one-time websocket ticket mint route is present'
require_pattern 'dcentrald/dcentrald-api/src/auth.rs' 'ws_ticket_flow_is_default_off_short_lived_and_one_time' 'websocket tickets are default-off, short-lived, and one-time'
require_pattern 'dcentrald/dcentrald-api/src/auth.rs' 'ticket=REDACTED' 'websocket ticket credentials are redacted from URI logs'
require_pattern 'scripts/lib/dcentrald_version_gate.sh' 'dcent_require_dcentrald_version_match' 'shared dcentrald version gate exposes fail-closed helper'
require_pattern 'scripts/lib/dcentrald_version_gate.sh' 'lab bypass requires non-release DCENT_PACKAGE_STATUS plus DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1' 'dcentrald version gate lab override is explicit'
require_pattern 'br2_external_dcentos/board/zynq/post-build.sh' 'dcent_require_dcentrald_version_match' 'zynq post-build enforces dcentrald version gate'
require_pattern 'br2_external_dcentos/board/zynq/am2-s19jpro/post-build.sh' 'dcent_require_dcentrald_version_match' 'am2 post-build enforces dcentrald version gate'
require_pattern 'br2_external_dcentos/board/amlogic/am3-s19kpro/post-build.sh' 'dcent_require_dcentrald_version_match' 'am3-s19k post-build enforces dcentrald version gate'
require_pattern 'br2_external_dcentos/board/amlogic/am3-s21/post-build.sh' 'dcent_require_dcentrald_version_match' 'am3-s21 post-build enforces dcentrald version gate'
require_pattern 'br2_external_dcentos/board/beaglebone/am3-bb/post-build.sh' 'dcent_require_dcentrald_version_match' 'am3-bb post-build enforces dcentrald version gate'
require_pattern 'scripts/build_in_docker.sh' 'build_in_docker Phase 5' 'build_in_docker validates staged dcentrald version before Buildroot'
require_pattern 'scripts/build_in_docker.sh' 'am3-s19kpro|am3-s21' 'build_in_docker applies am3 validation to both am3 tarball targets'
require_pattern 'scripts/build_in_docker.sh' 'pre_flash_validate.sh --package-only' 'build_in_docker runs am3 package-only validation'
require_pattern 'scripts/build_amlogic_native_install.sh' 'pre_flash_validate.sh" --package-only' 'amlogic native image builder validates sysupgrade package before extraction'
require_pattern 'scripts/build_amlogic_native_install.sh' 'OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"' 'amlogic native image builder canonicalizes output-dir before reuse'
require_pattern 'scripts/build_amlogic_native_install.sh' 'extracted rootfs exceeds Amlogic rootfs window' 'amlogic native image builder bounds extracted rootfs image'
require_pattern 'scripts/build_amlogic_native_install.sh' 'extracted rootfs is not a uImage payload' 'amlogic native image builder validates extracted rootfs magic'
require_pattern 'scripts/build_amlogic_native_install.sh' 'DCENT_AM3_ROOTFS_WINDOW_DEC' 'amlogic native image builder uses shared am3 rootfs window'
require_pattern 'scripts/install_amlogic_persistent.sh' 'Step 0/10: local package-only validation' 'amlogic persistent installer validates package before SSH'
require_pattern 'scripts/install_amlogic_persistent.sh' 'pre_flash_validate.sh" --package-only "$FIRMWARE" "$BOARD_PKG_NAME"' 'amlogic persistent installer reuses package-only validator'
require_pattern 'scripts/install_amlogic_persistent.sh' '--variant s19kpro|s21' 'amlogic persistent installer supports S19K and S21 variants'
require_pattern 'scripts/install_amlogic_persistent.sh' 'PACKAGE_PREFIX="sysupgrade-am3-s21"' 'amlogic persistent installer maps S21 package prefix'
require_pattern 'scripts/install_amlogic_persistent.sh' 'REMOTE_PREFIX="/data/sysupgrade/$PACKAGE_PREFIX"' 'amlogic persistent installer derives remote package prefix from variant'
require_pattern 'scripts/install_amlogic_persistent.sh' 'SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"' 'amlogic persistent installer resolves validator path from script dir'
require_pattern 'scripts/install_amlogic_persistent.sh' 'rm -rf /data/sysupgrade && mkdir -p /data/sysupgrade' 'amlogic persistent installer clears remote staging before extract'
require_pattern 'scripts/install_amlogic_persistent.sh' 'ROOTFS_END_DEC' 'amlogic persistent installer computes rootfs window end'
require_pattern 'scripts/install_amlogic_persistent.sh' 'mtd5 geometry OK' 'amlogic persistent installer validates target mtd5 geometry'
require_pattern 'scripts/install_amlogic_persistent.sh' 'root payload $ROOT_SIZE exceeds rootfs window' 'amlogic persistent installer bounds root payload size'
require_pattern 'scripts/install_amlogic_persistent.sh' 'fw_printenv backup is empty' 'amlogic persistent installer rejects empty fw_env backup'
require_pattern 'scripts/install_amlogic_persistent.sh' 'nand_env backup size $NAND_ENV_SIZE != 65536' 'amlogic persistent installer validates nand_env backup size'
require_pattern 'scripts/install_amlogic_persistent.sh' 'nand_env backup SHA mismatch' 'amlogic persistent installer verifies nand_env backup transfer'
require_pattern 'scripts/install_amlogic_persistent.sh' 'mtd5 backup SHA mismatch' 'amlogic persistent installer verifies mtd5 backup transfer'
require_pattern 'scripts/install_amlogic_persistent.sh' 'rootfs readback SHA mismatch' 'amlogic persistent installer verifies flashed rootfs readback'
require_pattern 'scripts/install_amlogic_persistent.sh' 'install_preflight_manifest.json' 'amlogic persistent installer writes recovery manifest'
require_pattern 'scripts/install_amlogic_persistent.sh' '"stage": "payload_verified"' 'amlogic persistent installer records recovery manifest stage'
require_pattern 'scripts/install_amlogic_persistent.sh' 'root_payload_sha256' 'amlogic persistent installer records root payload hash in manifest'
require_pattern 'scripts/install_amlogic_persistent.sh' 'remote_firmware_sha256' 'amlogic persistent installer records remote package hash in manifest'
require_pattern 'scripts/install_amlogic_persistent.sh' '"package_board": "$BOARD_PKG_NAME"' 'amlogic persistent installer records dynamic package board in manifest'
require_pattern 'scripts/install_amlogic_persistent.sh' 'root_write_readback.uimage' 'amlogic persistent installer preserves rootfs write readback artifact'
require_pattern 'scripts/install_amlogic_persistent.sh' 'fw_setenv firstboot verification failed' 'amlogic persistent installer verifies firstboot env before reboot'
require_pattern 'scripts/install_amlogic_persistent.sh' 'root_write_readback_sha256' 'amlogic persistent installer records write readback hash in manifest'
require_pattern 'scripts/install_amlogic_persistent.sh' 'firstboot_after_set' 'amlogic persistent installer records firstboot proof in manifest'
require_pattern 'scripts/install_amlogic_persistent.sh' 'mining services were not stopped' 'amlogic persistent installer dry-run does not stop mining services'
require_pattern 'scripts/install_amlogic_persistent.sh' 'after confirmation, graceful TERM' 'amlogic persistent installer stops services only after confirmation'
require_pattern 'scripts/install_amlogic_persistent.sh' 'Step 8/10: flash_erase $ROOTFS_MTD $ROOTFS_OFFSET_HEX $ROOTFS_ERASE_COUNT' 'amlogic persistent installer reports variable-driven flash geometry'
reject_pattern 'scripts/install_amlogic_persistent.sh' 'flash_erase /dev/mtd5 0x05700000 320' 'amlogic persistent installer does not hardcode flash geometry in operator output'
require_pattern 'scripts/install_amlogic_persistent.sh' '. "$SCRIPT_DIR/lib/am3_geometry.sh"' 'amlogic persistent installer sources shared am3 geometry'
require_pattern 'scripts/amlogic_lab_rootfs.sh' '. "$SCRIPT_DIR/lib/am3_geometry.sh"' 'amlogic lab rootfs sources shared am3 geometry'
require_pattern 'scripts/revert_to_stock_am3_aml_s19k.sh' '. "$SCRIPT_DIR/lib/am3_geometry.sh"' 'am3 s19k revert sources shared am3 geometry'
require_pattern 'scripts/revert_to_stock_am3_aml_s21.sh' '. "$SCRIPT_DIR/lib/am3_geometry.sh"' 'am3 s21 revert sources shared am3 geometry'
require_pattern 'scripts/lib/am3_geometry.sh' 'DCENT_AM3_ROOTFS_OFFSET_HEX="${DCENT_AM3_ROOTFS_OFFSET_HEX:-0x05700000}"' 'shared am3 geometry pins rootfs offset'
require_pattern 'scripts/lib/am3_geometry.sh' 'DCENT_AM3_ROOTFS_WINDOW_HEX="${DCENT_AM3_ROOTFS_WINDOW_HEX:-0x02800000}"' 'shared am3 geometry pins rootfs window'
require_pattern 'dcentrald/dcentrald-api/src/routes/restore_to_stock.rs' '.arg(&post_dwell_fp.sha256)' 'restore route passes post-dwell SHA into revert helper'
require_pattern 'br2_external_dcentos/board/zynq/am2-s19jpro/post-build.sh' 'revert_to_stock_s19_am2.sh' 'am2 post-build ships profile revert helper'
require_pattern 'br2_external_dcentos/board/zynq/am2-s19jpro/post-build.sh' 'stock-bitmain-manifest.json' 'am2 post-build ships stock Bitmain manifest'
# R-F1: two copies of the stock-Bitmain manifest exist and serve DIFFERENT
# runtime roles — dcentrald-api/assets/stock-bitmain-manifest.json is BAKED into
# the binary (include_str!, the restore-to-stock fallback used when
# /etc/dcentos/stock-bitmain-manifest.json is missing), while the 13 buildroot
# post-build.sh scripts SHIP
# to the target rootfs (the primary on-disk copy). They MUST stay byte-identical,
# but nothing enforced it ("identical by luck"): editing one (e.g. to populate a
# restore SHA) would silently desync the baked fallback from the shipped copy.
# sign_stock_manifest.sh now emits a no-replace candidate; promotion must copy
# those reviewed bytes into BOTH tracked signature locations. Assert both pairs
# remain byte-identical.
require_identical \
    'dcentrald/dcentrald-api/assets/stock-bitmain-manifest.json' \
    '../../knowledge-base/firmware-archive/stock-bitmain-manifest.json' \
    'baked (assets) and shipped (firmware-archive) stock-bitmain manifests are byte-identical'
require_identical \
    'dcentrald/dcentrald-api/assets/stock-bitmain-manifest.json.sig' \
    '../../knowledge-base/firmware-archive/stock-bitmain-manifest.json.sig' \
    'baked and shipped stock-bitmain manifest signatures are byte-identical'
# R-F6: the legacy SSH flashers write ACTIVE firmware paths (brick risk) and are
# DISABLED with an early error+exit. Pin the disable guard in each so it cannot be
# silently removed, which would re-enable a latent-brick path (e.g. flash_vnish's
# raw_nand+NEEDS_KERNEL branch that leaves a no-UIO stock kernel under the DCENT
# rootfs). Round-2 recon F6.
require_pattern 'scripts/flash_vnish.sh' 'is disabled' 'legacy flash_vnish.sh keeps its active-write disable guard'
require_pattern 'scripts/flash_braiinsos.sh' 'is disabled' 'legacy flash_braiinsos.sh keeps its active-write disable guard'
require_pattern 'scripts/flash_universal.sh' 'flashing is disabled' 'flash_universal.sh keeps its unsafe-flash disable guards'
require_pattern 'br2_external_dcentos/board/amlogic/am3-s19kpro/post-build.sh' 'usr/sbin/lib/am3_geometry.sh' 'am3-s19k post-build ships AM3 geometry helper for revert'
require_pattern 'br2_external_dcentos/board/amlogic/am3-s21/post-build.sh' 'usr/sbin/lib/am3_geometry.sh' 'am3-s21 post-build ships AM3 geometry helper for revert'
for revert_script in \
    scripts/revert_to_stock_s17.sh \
    scripts/revert_to_stock_am3_aml_s19k.sh \
    scripts/revert_to_stock_am3_aml_s21.sh
do
    require_pattern "$revert_script" 'EXPECTED_SHA256=' "stock revert helper $(basename "$revert_script") accepts expected SHA"
    require_pattern "$revert_script" 'Firmware SHA-256 verified at extraction time.' "stock revert helper $(basename "$revert_script") re-hashes before extraction"
    require_pattern "$revert_script" 'MAX_EXTRACTED_KB' "stock revert helper $(basename "$revert_script") caps extracted size"
    require_pattern "$revert_script" 'firmware archive contains hard-linked files' "stock revert helper $(basename "$revert_script") rejects hard-linked files"
done
require_pattern 'scripts/amlogic_lab_rootfs.sh' 'ROOTFS_END_DEC' 'amlogic lab rootfs computes rootfs window end'
require_pattern 'scripts/amlogic_lab_rootfs.sh' 'mtd5 geometry OK' 'amlogic lab rootfs validates target mtd5 geometry'
require_pattern 'scripts/amlogic_lab_rootfs.sh' 'require_uimage_file' 'amlogic lab rootfs validates uImage payloads before write/restore'
require_pattern 'scripts/amlogic_lab_rootfs.sh' 'magic=$magic' 'amlogic lab rootfs reports invalid uImage magic'
require_pattern 'scripts/amlogic_lab_rootfs.sh' 'require_recovery_artifact' 'amlogic lab rootfs requires local recovery artifact before write'
require_pattern 'scripts/amlogic_lab_rootfs.sh' 'Recovery manifest lacks backup_sha256' 'amlogic lab rootfs verifies recovery manifest before write'
require_pattern 'scripts/amlogic_lab_rootfs.sh' 'Recovery backup SHA mismatch' 'amlogic lab rootfs verifies recovery manifest hash before write'
require_pattern 'scripts/amlogic_lab_rootfs.sh' 'Backup transfer SHA mismatch' 'amlogic lab rootfs verifies backup transfer'
require_pattern 'scripts/amlogic_lab_rootfs.sh' 'remote_backup_sha256' 'amlogic lab rootfs records backup manifest proof'
require_pattern 'scripts/amlogic_lab_rootfs.sh' 'Candidate upload SHA mismatch' 'amlogic lab rootfs verifies candidate upload before flash'
require_pattern 'scripts/amlogic_lab_rootfs.sh' 'Post-write readback SHA mismatch' 'amlogic lab rootfs verifies written image readback'
require_pattern 'scripts/amlogic_lab_rootfs.sh' 'write_readback.uimage' 'amlogic lab rootfs preserves write readback artifact'
require_pattern 'scripts/amlogic_lab_rootfs.sh' 'write_manifest.json' 'amlogic lab rootfs writes flash proof manifest'
require_pattern 'scripts/amlogic_lab_rootfs.sh' 'readback_manifest.json' 'amlogic lab rootfs writes standalone readback proof manifest'
require_pattern 'scripts/amlogic_lab_rootfs.sh' 'Restore upload SHA mismatch' 'amlogic lab rootfs verifies restore upload before flash'
require_pattern 'scripts/amlogic_lab_rootfs.sh' 'RESTORE_REMOTE_READBACK_SHA' 'amlogic lab rootfs verifies restore readback on target'
require_pattern 'scripts/amlogic_lab_rootfs.sh' 'restore_manifest.json' 'amlogic lab rootfs writes restore proof manifest'
reject_pattern 'scripts/build_rootfs_s21.sh' 'flash_erase /dev/mtd5' 'legacy S21 rootfs builder does not print raw mtd5 erase commands'
reject_pattern 'scripts/build_rootfs_s21.sh' 'nandwrite -p -s $ROOTFS_OFFSET_HEX /dev/mtd5' 'legacy S21 rootfs builder does not print raw mtd5 write commands'
reject_pattern 'scripts/build_rootfs_s21.sh' '0x5100000' 'legacy S21 rootfs builder does not carry stale am3 rootfs offset'
require_pattern 'br2_external_dcentos/board/amlogic/am3-s19kpro/post-build.sh' 'usr/bin/telnet' 'am3 post-build removes telnet client tooling'
require_pattern 'br2_external_dcentos/board/amlogic/am3-s19kpro/post-build.sh' 'usr/sbin/telnetd' 'am3 post-build removes telnet daemon'
require_pattern 'br2_external_dcentos/board/amlogic/am3-s19kpro/post-image.sh' 'Rootfs audit: access services present; telnet paths absent' 'am3 post-image runs rootfs service-surface audit'
require_pattern 'br2_external_dcentos/board/amlogic/am3-s19kpro/post-image.sh' 'host_driven_rootfs_window_lab' 'am3-s19k manifest marks host-driven install mode'
require_pattern 'br2_external_dcentos/board/amlogic/am3-s19kpro/post-image.sh' 'DCENT_TARGET_SIDE_SYSUPGRADE=false' 'am3-s19k manifest disables target-side sysupgrade claim'
require_pattern 'br2_external_dcentos/board/amlogic/am3-s19kpro/post-image.sh' 'dcent_write_sysupgrade_manifest' 'am3-s19k post-image uses shared manifest helper'
require_pattern 'br2_external_dcentos/board/amlogic/am3-s19kpro/post-image.sh' 'require_rootfs_path "etc/init.d/S50dropbear"' 'am3 rootfs audit requires Dropbear init'
require_pattern 'br2_external_dcentos/board/amlogic/am3-s19kpro/post-image.sh' 'require_rootfs_path "root/web/mcp_server.py"' 'am3 rootfs audit requires MCP server'
require_pattern 'br2_external_dcentos/board/amlogic/am3-s19kpro/post-image.sh' 'require_rootfs_path "uninstall.sh"' 'am3 rootfs audit requires uninstall hook'
require_pattern 'br2_external_dcentos/board/amlogic/am3-s19kpro/post-image.sh' 'reject_rootfs_pattern' 'am3 rootfs audit rejects forbidden paths'
require_pattern 'br2_external_dcentos/board/amlogic/am3-s21/post-build.sh' 'usr/bin/telnet' 'am3-s21 post-build removes telnet client tooling'
require_pattern 'br2_external_dcentos/board/amlogic/am3-s21/post-build.sh' 'usr/sbin/telnetd' 'am3-s21 post-build removes telnet daemon'
require_pattern 'br2_external_dcentos/board/amlogic/am3-s21/post-image.sh' 'Rootfs audit: access services present; telnet paths absent' 'am3-s21 post-image runs rootfs service-surface audit'
require_pattern 'br2_external_dcentos/board/amlogic/am3-s21/post-image.sh' 'host_driven_rootfs_window_lab' 'am3-s21 manifest marks host-driven install mode'
require_pattern 'br2_external_dcentos/board/amlogic/am3-s21/post-image.sh' 'DCENT_TARGET_SIDE_SYSUPGRADE=false' 'am3-s21 manifest disables target-side sysupgrade claim'
require_pattern 'br2_external_dcentos/board/amlogic/am3-s21/post-image.sh' 'dcent_write_sysupgrade_manifest' 'am3-s21 post-image uses shared manifest helper'
require_pattern 'br2_external_dcentos/board/amlogic/am3-s21/post-image.sh' 'require_rootfs_path "etc/init.d/S50dropbear"' 'am3-s21 rootfs audit requires Dropbear init'
require_pattern 'br2_external_dcentos/board/amlogic/am3-s21/post-image.sh' 'require_rootfs_path "root/web/mcp_server.py"' 'am3-s21 rootfs audit requires MCP server'
require_pattern 'br2_external_dcentos/board/amlogic/am3-s21/post-image.sh' 'require_rootfs_path "uninstall.sh"' 'am3-s21 rootfs audit requires uninstall hook'
require_pattern 'br2_external_dcentos/board/amlogic/am3-s21/post-image.sh' 'reject_rootfs_pattern' 'am3-s21 rootfs audit rejects forbidden paths'
require_pattern 'scripts/revert_to_stock_am335x_bb.sh' 'AM335x BB NAND revert is disabled' 'am3-bb revert refuses unvalidated NAND path by default'
require_pattern 'scripts/revert_to_stock_am335x_bb.sh' 'DCENT_AM3_BB_PROC_MTD_EVIDENCE' 'am3-bb revert requires live proc-mtd evidence before future override'
require_pattern 'scripts/revert_to_stock_am335x_bb.sh' 'DCENT_AM3_BB_ENABLE_NAND_REVERT is not accepted as a bypass' 'am3-bb revert has no env-only bypass'
require_pattern 'br2_external_dcentos/board/beaglebone/am3-bb/post-build.sh' 'echo "am3-bb" > "${TARGET_DIR}/etc/dcentos/board_family"' 'am3-bb post-build stamps unambiguous board_family'
reject_pattern 'scripts/revert_to_stock_am335x_bb.sh' 'flash_erase' 'am3-bb revert contains no erase path'
reject_pattern 'scripts/revert_to_stock_am335x_bb.sh' 'nandwrite' 'am3-bb revert contains no NAND write path'
reject_pattern 'scripts/revert_to_stock_am335x_bb.sh' 'fw_setenv' 'am3-bb revert contains no env write path'
require_pattern 'br2_external_dcentos/board/beaglebone/am3-bb/post-build.sh' 'management-bringup-sdcard-only' 'am3-bb rootfs marks management bring-up only status'
require_pattern 'br2_external_dcentos/board/beaglebone/am3-bb/post-image.sh' 'NAND install/revert is disabled until dated live /proc/mtd evidence exists.' 'am3-bb post-image documents NAND disabled status'

#
# W2.3 single-I2C-owner lockdown: refuse `I2cBus::open(...)` outside the HAL
# `platform/` modules and inside-HAL legitimate owners (psu/adc/i2c.rs).
#
# Normal out-of-HAL callers MUST go through `I2cServiceHandle`
# (spawn_i2c_service*) or the fixed secondary-bus miner-identity bootstrap
# helper. Arbitrary raw EEPROM access is recovery-feature-only. Recovery tools
# may opt in to `I2cBus::open_for_recovery`; additive Cargo feature unification
# means this source gate remains necessary even when manifests look isolated. See
# `dcentrald/dcentrald-hal/src/i2c.rs::I2cBus::open` for the contract.
#
# Build-log artifacts (build_output.txt, *.log) are excluded so a stale
# warning copy from cargo never trips the gate.
#
i2c_open_check_dirs="
dcentrald/dcentrald
dcentrald/dcentrald-asic
dcentrald/dcentrald-thermal
dcentrald/dcentrald-api
dcentrald/dcentrald-autotuner
dcentrald/dcentrald-diagnostics
"

i2c_open_hits=""
for dir in $i2c_open_check_dirs; do
    if [ ! -d "$dir" ]; then
        continue
    fi
    # Match the normal raw constructor exactly. Recovery and bootstrap APIs
    # have distinct names and are reviewed by separate feature/source gates.
    found=$(grep -rn 'I2cBus::open(' "$dir" --include='*.rs' 2>/dev/null | \
        awk -F: '{
            line=$0
            sub(/^[^:]+:[0-9]+:/, "", line)
            sub(/^[[:space:]]+/, "", line)
            if (line ~ /^\/\//) next
            if (line ~ /^\/\*/) next
            if (line ~ /^\*/) next
            print
        }' || true)
    if [ -n "$found" ]; then
        if [ -z "$i2c_open_hits" ]; then
            i2c_open_hits="$found"
        else
            i2c_open_hits="$i2c_open_hits
$found"
        fi
    fi
done

if [ -n "$i2c_open_hits" ]; then
    fail "single-I2C-owner: I2cBus::open(...) called outside dcentrald-hal/platform"
    printf '%s\n' "$i2c_open_hits" >&2
else
    pass "single-I2C-owner: no out-of-HAL I2cBus::open(...) callers"
fi

reject_pattern 'dcentrald/dcentrald/src/serial_mining.rs' 'libc::ioctl' \
    'single-I2C-owner: serial runtime has no direct ioctl transport'
reject_pattern 'dcentrald/dcentrald/src/serial_mining.rs' 'std::fs::OpenOptions' \
    'single-I2C-owner: serial runtime has no direct device-file owner'

#
# Cross-process I2C fabric ownership. The process-local HAL registry remains
# responsible for allocation identity and quarantine, while this leaf crate is
# the one protocol shared by the daemon and standalone inspection tooling.
# These pins prevent a future refactor from quietly reverting to pidfile/TOCTOU
# ownership, releasing quarantine at token Drop, or bypassing a refused kernel
# adapter through GPIO bit-bang.
#
fabric_lease='dcentrald/dcentrald-fabric-lease/src/lib.rs'
require_file 'dcentrald/dcentrald-fabric-lease/Cargo.toml'
require_file "$fabric_lease"
require_pattern 'dcentrald/Cargo.toml' '"dcentrald-fabric-lease"' \
    'I2C fabric lease: shared leaf crate remains a workspace/default member'
require_pattern 'dcentrald/dcentrald-hal/Cargo.toml' 'dcentrald-fabric-lease = { path = "../dcentrald-fabric-lease" }' \
    'I2C fabric lease: HAL uses the shared cross-process protocol'
require_pattern 'dcentrald/pic-recovery/Cargo.toml' 'dcentrald-fabric-lease = { path = "../dcentrald-fabric-lease" }' \
    'I2C fabric lease: standalone inspection uses the shared protocol'
require_pattern "$fabric_lease" 'libc::LOCK_EX | libc::LOCK_NB' \
    'I2C fabric lease: ownership acquisition is exclusive and nonblocking'
require_pattern "$fabric_lease" 'libc::O_CLOEXEC | libc::O_NOFOLLOW' \
    'I2C fabric lease: opened paths are close-on-exec and do not follow symlinks'
require_pattern "$fabric_lease" 'stat.st_nlink != 1' \
    'I2C fabric lease: hard-linked lock targets fail closed'
require_pattern "$fabric_lease" 'exec_child_does_not_retain_parent_lease' \
    'I2C fabric lease: an actual exec child does not retain parent ownership'
require_pattern "$fabric_lease" 'copied_lease_state_rejects_a_different_process_identity' \
    'I2C fabric lease: copied fork-only state rejects a different process identity'
require_pattern "$fabric_lease" 'subprocess_contention_and_sigkill_release_are_kernel_proven' \
    'I2C fabric lease: subprocess exclusion and crash release execute in host tests'
require_pattern "$fabric_lease" 'pub mod topology' \
    'I2C fabric lease: topology-defined IDs have one canonical ABI ledger'
require_pattern "$fabric_lease" 'NAMED_PHYSICAL_I2C_FABRICS' \
    'I2C fabric lease: named topology IDs expose a collision-test registry'
reject_pattern "$fabric_lease" 'pub const fn topology_defined' \
    'I2C fabric lease: external crates cannot mint or relabel topology IDs'

fabric_lease_production=$(sed '/^#\[cfg.*test/,$d' "$fabric_lease")
if printf '%s\n' "$fabric_lease_production" | grep -Eq \
    'LOCK_UN|libc::unlink|std::fs::remove_file|std::fs::rename'; then
    fail 'I2C fabric lease: production code contains explicit unlock/unlink/rename'
else
    pass 'I2C fabric lease: production release is close-only and never replaces the stable inode'
fi

require_pattern 'dcentrald/dcentrald-hal/src/lib.rs' 'I2cFabricUnavailable' \
    'I2C fabric lease: ownership refusal has a typed non-transport HAL error'
require_pattern 'dcentrald/dcentrald-hal/src/i2c.rs' '_os_lease: Option<OsI2cFabricLease>' \
    'I2C fabric lease: registry entries retain OS ownership through quarantine'
require_pattern 'dcentrald/dcentrald-hal/src/i2c.rs' 'I2cServiceRegistryState::PreparingMutated' \
    'I2C fabric lease: preparation mutation has an explicit quarantine state'
require_pattern 'dcentrald/dcentrald-hal/src/i2c.rs' 'every_raw_wire_entry_revalidates_process_ownership' \
    'I2C fabric lease: raw kernel/devmem entry points pin fork-process validation'
require_pattern 'dcentrald/dcentrald-hal/src/i2c.rs' 'inherited_raw_handle_is_refused_before_simulated_wire_io' \
    'I2C fabric lease: inherited raw state is refused before executable wire behavior'
require_pattern 'dcentrald/dcentrald-hal/src/psu_gpio_i2c.rs' '_fabric_lease: I2cRawFabricLease' \
    'I2C fabric lease: GPIO bit-bang retains the canonical fabric reservation'
require_pattern 'dcentrald/dcentrald-hal/src/psu_gpio_i2c.rs' 'AM2_PSU_GPIO_I2C_FABRIC' \
    'I2C fabric lease: dedicated AM2 PSU wires use their named topology identity'
require_pattern 'dcentrald/dcentrald-hal/src/psu_gpio_i2c.rs' 'pub fn new_am2()' \
    'I2C fabric lease: fixed AM2 GPIO pins use a topology-specific constructor'
reject_pattern 'dcentrald/dcentrald-hal/src/psu_gpio_i2c.rs' 'pub fn new(' \
    'I2C fabric lease: arbitrary pins cannot be mislabeled as the AM2 PSU fabric'
reject_pattern 'dcentrald/dcentrald-hal/src/psu_gpio_i2c.rs' 'new_on_fabric' \
    'I2C fabric lease: fixed GPIO wires cannot be relabeled by a caller-selected bus'
require_pattern 'dcentrald/dcentrald-hal/src/i2c.rs' 'dedicated_am2_psu_fabric_coexists_with_bus_zero_but_self_conflicts' \
    'I2C fabric lease: dedicated PSU and adapter-zero coexistence/exclusion is pinned'
require_pattern 'dcentrald/dcentrald-hal/src/psu_gpio_i2c.rs' 'every_public_gpio_wire_entry_revalidates_process_ownership' \
    'I2C fabric lease: every GPIO/MMIO public wire entry pins fork validation'
gpio_process_checks=$(awk '/^#\[cfg\(test\)\]/{exit} {print}' \
    dcentrald/dcentrald-hal/src/psu_gpio_i2c.rs \
    | grep -c 'self\._fabric_lease\.validate_current_process()?;' || true)
if [ "$gpio_process_checks" -eq 6 ]; then
    pass 'I2C fabric lease: all six GPIO/MMIO wire entries validate process ownership'
else
    fail "I2C fabric lease: expected six GPIO/MMIO process guards, found $gpio_process_checks"
fi
require_pattern 'dcentrald/dcentrald-hal/src/psu.rs' 'kernel_i2c_absence_allows_gpio_fallback' \
    'I2C fabric lease: PSU GPIO fallback is limited to proven adapter absence'
require_pattern 'dcentrald/dcentrald-hal/src/psu.rs' 'Err(error @ HalError::I2cFabricUnavailable { .. }) => return Err(error)' \
    'I2C fabric lease: ownership refusal cannot fall through mmap-to-sysfs fallback'

require_pattern 'dcentrald/dcentrald-hal/src/i2c.rs' \
    'spawn_i2c_service_no_register_touch_with_denylist_and_reserved_preparation' \
    'I2C fabric lease: platform adapter preparation has a reserve-first factory'
require_pattern 'dcentrald/dcentrald-hal/src/i2c.rs' \
    'reserved_preparation_marks_mutated_before_callback_and_quarantines_error' \
    'I2C fabric lease: preparation callback ordering and failure quarantine execute'
hybrid='dcentrald/dcentrald/src/s19j_hybrid_mining.rs'
hybrid_reserve_line=$(grep -n '^[[:space:]]*spawn_i2c_service_no_register_touch_with_denylist_and_reserved_preparation(' "$hybrid" | head -n 1 | cut -d: -f1 || true)
hybrid_prepare_line=$(grep -n 'ensure_i2c0_kernel_bound().map_err' "$hybrid" | head -n 1 | cut -d: -f1 || true)
if [ -n "$hybrid_reserve_line" ] && [ -n "$hybrid_prepare_line" ] \
    && [ "$hybrid_reserve_line" -lt "$hybrid_prepare_line" ]; then
    pass 'I2C fabric lease: AM2 xiic bind/mknod callback is nested behind reservation'
else
    fail 'I2C fabric lease: AM2 xiic bind/mknod is not visibly behind reservation'
fi

pic_recovery='dcentrald/pic-recovery/src/main.rs'
lease_acquire_line=$(grep -n 'OsI2cFabricLease::acquire' "$pic_recovery" | head -n 1 | cut -d: -f1 || true)
device_open_line=$(grep -n 'let fd = unsafe { libc::open' "$pic_recovery" | head -n 1 | cut -d: -f1 || true)
if [ -n "$lease_acquire_line" ] && [ -n "$device_open_line" ] \
    && [ "$lease_acquire_line" -lt "$device_open_line" ]; then
    pass 'I2C fabric lease: pic-recovery acquires cross-process ownership before device open'
else
    fail 'I2C fabric lease: pic-recovery device open is not visibly preceded by shared ownership'
fi
pic_validate_line=$(grep -n '\.validate_current_process()' "$pic_recovery" | head -n 1 | cut -d: -f1 || true)
pic_ioctl_line=$(grep -n 'libc::ioctl' "$pic_recovery" | head -n 1 | cut -d: -f1 || true)
if [ -n "$pic_validate_line" ] && [ -n "$pic_ioctl_line" ] \
    && [ "$pic_validate_line" -lt "$pic_ioctl_line" ]; then
    pass 'I2C fabric lease: pic-recovery revalidates process identity before ioctl/read'
else
    fail 'I2C fabric lease: pic-recovery wire access lacks a visible process-identity guard'
fi

require_pattern 'scripts/run_all_gates.sh' 'dcentrald-fabric-lease' \
    'I2C fabric lease: comprehensive local gate executes subprocess ownership tests'
require_pattern 'scripts/run_all_gates.sh' 'test -p pic-recovery' \
    'I2C fabric lease: comprehensive local gate explicitly tests diagnostic boundary'
require_pattern '../../.github/workflows/dcentos-offline-gates.yml' 'cargo test -p dcentrald-fabric-lease' \
    'I2C fabric lease: hosted CI executes subprocess ownership tests'
require_pattern '../../.github/workflows/dcentos-offline-gates.yml' 'cargo test -p pic-recovery' \
    'I2C fabric lease: hosted CI explicitly tests diagnostic boundary'

#
# W4.7 panic-discipline static gates (DCENT_DevOps + DCENT_QA, 2026-05-07).
#
# Three checks:
#   1. panic = "abort" must remain in [profile.release] (S9 squashfs gate;
#).
#   2. ASIC drivers must not regrow `.swap_bytes()` adjacent to `midstate`
#      tokens in non-comment Rust code (regression-pin for the 2026-03-17
#      first-accepted-shares fix in bm1387.rs:1460-1483; CE-agent analysis
#      that re-suggested this swap was wrong, all shares rejected
#      "Above target").
#   3. dev_deploy.sh must keep `kill -9` of bosminer platform-conditional —
#      Zynq paths must use SIGTERM + 10s wait (see
#      ). Only the amlogic warm-takeover
#      branch may SIGKILL.
#
# Counterpart CI workflow: .github/workflows/lint-gates.yml.
# Grandfather doc:
#

# 1. panic = "abort" presence
panic_abort_check() {
    cargo_toml='dcentrald/Cargo.toml'
    if [ ! -f "$cargo_toml" ]; then
        fail "panic-abort: missing $cargo_toml"
        return
    fi
    block=$(awk '
        /^\[profile\.release\]/ { in_block=1; next }
        /^\[/ && in_block { exit }
        in_block { print }
    ' "$cargo_toml")
    if [ -z "$block" ]; then
        fail "panic-abort: no [profile.release] section in $cargo_toml"
        return
    fi
    if printf '%s\n' "$block" | grep -Eq '^[[:space:]]*panic[[:space:]]*=[[:space:]]*"abort"'; then
        pass "panic-abort: panic = \"abort\" pinned in $cargo_toml [profile.release]"
    else
        fail "panic-abort: panic = \"abort\" missing from [profile.release] in $cargo_toml — see feedback_panic_abort_required_s9.md"
    fi
}
panic_abort_check

# 2. swap_bytes near midstate token ban (Protocol expert).
#    Look only in ASIC driver source files and in the work_dispatcher
#    midstate-encode hot path. The ban is on `.swap_bytes()` appearing on
#    the same line as a `midstate` identifier in actual code (not comments).
#    Existing files contain MANY guard comments saying "NO .swap_bytes()" /
#    "DO NOT ADD .swap_bytes()" — those are documentation, not violations.
#    We strip leading whitespace then skip any line whose first
#    non-whitespace char is `//`. Block comments are rare in this context;
#    we accept a tiny false-negative risk in exchange for simplicity.
#
#    W6.2 (2026-05-07, DCENT_QA + DCENT_Protocol): the scan list was
#    extended to also cover the Stratum V1 submit path
#    (`dcentrald-stratum/src/v1/client.rs`) and the work_dispatcher
#    `submit_share` neighborhood. The original 2026-03-17 bm1387.rs
#    regression was an ASIC-driver bug, but the same byte-order class of
#    mistake on the submit boundary would silently produce "Above target"
#    rejects rather than a hardware-side miscount, so the Protocol
#    expert wants the gate to refuse `.swap_bytes()` adjacent to
#    `midstate` in the submit-path crates too. Counterpart e2e:
#    `dcentrald-api/tests/share_submission_e2e.rs` (mock pool +
#    per-chip-family golden midstates).
swap_bytes_midstate_check() {
    targets='
        dcentrald/dcentrald-asic/src/drivers/bm1387.rs
        dcentrald/dcentrald-asic/src/drivers/bm1397.rs
        dcentrald/dcentrald-asic/src/drivers/bm1366.rs
        dcentrald/dcentrald-asic/src/drivers/bm1368.rs
        dcentrald/dcentrald-asic/src/drivers/bm1362.rs
        dcentrald/dcentrald-asic/src/drivers/bm1398.rs
        dcentrald/dcentrald-asic/src/drivers/bm1370.rs
        dcentrald/dcentrald-asic/src/drivers/bm1391.rs
        dcentrald/dcentrald/src/work_dispatcher.rs
        dcentrald/dcentrald/src/chain.rs
        dcentrald/dcentrald-stratum/src/v1/client.rs
    '
    hits=''
    for f in $targets; do
        if [ ! -f "$f" ]; then
            continue
        fi
        # Find lines that mention BOTH `midstate` and `.swap_bytes(`,
        # then drop comment-only lines. The work_dispatcher.rs file
        # has a known-good `swapped_nonce = nonce_result.nonce.swap_bytes()`
        # that has nothing to do with midstate encoding — that line
        # mentions `swapped_nonce` not `midstate`, so the dual-mention
        # filter excludes it correctly.
        # `grep -n` on a single file emits `LINENO:CONTENT` (no filename prefix).
        # Strip the leading `LINENO:` and any whitespace, then drop pure
        # comment lines. Print the original `LINENO:CONTENT` for offender output.
        candidate=$(grep -nE '\.swap_bytes\(' "$f" 2>/dev/null \
            | grep -E 'midstate' \
            | awk '{
                orig = $0
                # Strip leading "LINENO:" prefix added by grep -n.
                sub(/^[0-9]+:/, "", $0)
                # Strip leading whitespace.
                sub(/^[[:space:]]+/, "", $0)
                # Skip comment-only lines.
                if ($0 ~ /^\/\//) next
                if ($0 ~ /^\/\*/) next
                if ($0 ~ /^\*/) next
                print orig
            }' \
            || true)
        if [ -n "$candidate" ]; then
            if [ -z "$hits" ]; then
                hits=$candidate
            else
                hits="$hits
$candidate"
            fi
        fi
    done
    if [ -n "$hits" ]; then
        fail "swap_bytes-midstate: .swap_bytes() found adjacent to midstate token in non-comment code (regression of 2026-03-17 bm1387.rs first-accepted-shares fix)"
        printf '%s\n' "$hits" >&2
    else
        pass "swap_bytes-midstate: no non-comment .swap_bytes() near midstate identifiers in ASIC drivers"
    fi
}
swap_bytes_midstate_check

# 3. dev_deploy.sh: kill -9 bosminer must be platform-conditional.
#    Walk the file looking for any line that calls SIGKILL on bosminer
#    (covers `kill -9 bosminer`, `kill -9 $BOSMINER_PID`, and the for-loop
#    pidof construct on amlogic). Each hit must be inside a context that
#    already gates on PLATFORM_FAMILY = "amlogic" within the surrounding
#    20-line window. The non-amlogic deploy path uses
#    `kill -TERM ...; sleep 10; kill -9 ... 2>/dev/null` which is allowed
#    because the SIGTERM precedes it with a 10-second drain — that is a
#    fallback after graceful shutdown attempt, not an unconditional SIGKILL.
kill9_bosminer_check() {
    f='scripts/dev_deploy.sh'
    if [ ! -f "$f" ]; then
        fail "kill9-bosminer: missing $f"
        return
    fi

    # Find every line that mentions `kill -9` AND `bosminer` (or a bosminer-pid
    # variable). For each hit, check whether the prior 20 lines contain either
    # `PLATFORM_FAMILY = "amlogic"` (amlogic warm-takeover branch) or
    # `kill -TERM` (graceful-shutdown fallback path).
    line_numbers=$(grep -nE 'kill[[:space:]]+-9' "$f" 2>/dev/null \
        | grep -E 'bosminer|BOSMINER' \
        | awk -F: '{ print $1 }' \
        || true)

    bad=''
    for ln in $line_numbers; do
        # Window: 20 lines before this hit.
        start=$((ln - 20))
        if [ "$start" -lt 1 ]; then
            start=1
        fi
        window=$(sed -n "${start},${ln}p" "$f")
        if printf '%s\n' "$window" | grep -qE 'PLATFORM_FAMILY[[:space:]]*=[[:space:]]*"amlogic"'; then
            continue
        fi
        if printf '%s\n' "$window" | grep -qE 'kill[[:space:]]+-TERM'; then
            continue
        fi
        # Neither gate found — this is a regression.
        if [ -z "$bad" ]; then
            bad="line $ln: $(sed -n "${ln}p" "$f")"
        else
            bad="$bad
line $ln: $(sed -n "${ln}p" "$f")"
        fi
    done

    if [ -n "$bad" ]; then
        fail "kill9-bosminer: unconditional kill -9 of bosminer in $f (must be amlogic-only or follow kill -TERM + 10s wait — see feedback_xiic_stuck_state_recovery.md)"
        printf '%s\n' "$bad" >&2
    else
        pass "kill9-bosminer: every kill -9 of bosminer in $f is platform-conditional or graceful-fallback"
    fi
}
kill9_bosminer_check

#
# W4.2 stale-tarball advisory gate (DCENT_DevOps).
#
# This is an INFORMATIONAL gate -- it warns but never fails. Goal: surface
# the case where someone edited a Buildroot defconfig (e.g. enabled a new
# package, bumped a kernel arg, swapped a board overlay path) but the
# matching `output/dcentos-*.tar` was last produced before that edit. A
# stale tarball is silent in build_in_docker.sh because the Docker volume
# happily picks up the new defconfig but the operator may flash the old
# tarball from `output/`.
#
# We deliberately stay non-fatal because:
#   1. `output/` is gitignored and may be empty on a fresh clone -- no
#      tarball at all is the common case, not a failure.
#   2. Defconfig edits frequently land before the next release rebuild
#      (commit -> rebuild -> commit pin). Failing CI here would block
#      every routine defconfig edit.
#   3. Non-S9 packaging has no authenticated outer capsule. The retained
#      `rebuild_all_non_s9.sh --list` inventory explains that blocked state;
#      a target-specific capsule is the fix.
#
# Mechanism (pure POSIX sh + git plumbing):
#   For each (target, defconfig, tarball) triple:
#     - skip if the tarball isn't present in output/
#     - read the last-commit unix-timestamp of the defconfig via
#       `git log -1 --format=%ct -- <path>`
#     - read the tarball's mtime via `stat -c %Y` (GNU coreutils, available
#       on every Buildroot/Docker host we run on; macOS dev shells are
#       not the CI target)
#     - warn if defconfig mtime > tarball mtime
#
stale_tarball_advisory_gate() {
    output_dir="$PROJECT_DIR/output"

    if [ ! -d "$output_dir" ]; then
        pass "stale-tarball advisory: no output/ directory yet (skipping; nothing to compare)"
        return 0
    fi

    if ! command -v git >/dev/null 2>&1; then
        pass "stale-tarball advisory: git unavailable (skipping; gate is informational only)"
        return 0
    fi

    if ! git -C "$PROJECT_DIR" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        pass "stale-tarball advisory: not inside a git work tree (skipping)"
        return 0
    fi

    # Triples are space-separated: target|defconfig|tarball. Using `|` so
    # the inner `set` parser handles tokens with predictable boundaries.
    triples='
am2-s19jpro|br2_external_dcentos/configs/dcentos_am2_s19jpro_defconfig|output/dcentos-sysupgrade-am2-s19jpro.tar
am2-s19pro|br2_external_dcentos/configs/dcentos_am2_s19pro_defconfig|output/dcentos-sysupgrade-am2-s19pro.tar
am2-s17pro|br2_external_dcentos/configs/dcentos_am2_s17pro_zynq_defconfig|output/dcentos-sysupgrade-am2-s17pro.tar
am3-s19kpro|br2_external_dcentos/configs/dcentos_am3_s19kpro_defconfig|output/dcentos-sysupgrade-am3-s19kpro.tar
am3-s21|br2_external_dcentos/configs/dcentos_am3_s21_defconfig|output/dcentos-sysupgrade-am3-s21.tar
am3-s21pro|br2_external_dcentos/configs/dcentos_am3_s21pro_defconfig|output/dcentos-sysupgrade-am3-s21pro.tar
am3-s21xp|br2_external_dcentos/configs/dcentos_am3_s21xp_defconfig|output/dcentos-sysupgrade-am3-s21xp.tar
am3-s19jpro-aml|br2_external_dcentos/configs/dcentos_am3_s19jpro_aml_defconfig|output/dcentos-sysupgrade-am3-s19jpro-aml.tar
am3-t21|br2_external_dcentos/configs/dcentos_am3_t21_defconfig|output/dcentos-sysupgrade-am3-t21.tar
am3-bb|br2_external_dcentos/configs/dcentos_am3_bb_defconfig|output/dcentos-am3-bb-sdcard.tar
am3-bb-s19jpro|br2_external_dcentos/configs/dcentos_am3_bb_s19jpro_defconfig|output/dcentos-am3-bb-s19jpro-sdcard.tar
'

    # Per-target status accounting via a tempfile because `printf | while`
    # runs the loop body in a pipeline subshell (POSIX), so we cannot
    # mutate counter variables in place. Tempfile is read once after the
    # loop completes for the final pass/warn summary.
    tmp_status=$(mktemp 2>/dev/null || echo "/tmp/dcentos-stale-tarball.$$")
    : > "$tmp_status"

    printf '%s\n' "$triples" | while IFS='|' read -r target defconfig tarball; do
        [ -n "$target" ] || continue

        if [ ! -f "$PROJECT_DIR/$tarball" ]; then
            printf 'SKIP %s\n' "$target" >> "$tmp_status"
            continue
        fi

        if [ ! -f "$PROJECT_DIR/$defconfig" ]; then
            printf 'MISSING_DEFCONFIG %s %s\n' "$target" "$defconfig" >> "$tmp_status"
            continue
        fi

        defconfig_commit_ts=$(git -C "$PROJECT_DIR" log -1 --format=%ct -- "$defconfig" 2>/dev/null || echo "")
        if [ -z "$defconfig_commit_ts" ]; then
            # Defconfig is staged/untracked; no commit anchor to compare
            # against. Treat as informational skip rather than warning --
            # the operator already knows they have local changes.
            printf 'NO_COMMIT %s\n' "$target" >> "$tmp_status"
            continue
        fi

        tarball_mtime=$(stat -c %Y "$PROJECT_DIR/$tarball" 2>/dev/null || echo "")
        if [ -z "$tarball_mtime" ]; then
            printf 'NO_MTIME %s\n' "$target" >> "$tmp_status"
            continue
        fi

        if [ "$defconfig_commit_ts" -gt "$tarball_mtime" ]; then
            printf 'STALE %s %s %s\n' "$target" "$defconfig_commit_ts" "$tarball_mtime" >> "$tmp_status"
        else
            printf 'FRESH %s\n' "$target" >> "$tmp_status"
        fi
    done

    # `grep -c` with zero matches exits non-zero AND prints `0` -- the
    # naive `|| echo 0` then concatenates two `0`s into a multi-line value
    # that truncates the downstream `pass` line. Use `awk` for a robust
    # single-line count instead.
    stale_count=$(awk '/^STALE /{n++} END{print n+0}' "$tmp_status")
    fresh_count=$(awk '/^FRESH /{n++} END{print n+0}' "$tmp_status")
    skip_count=$(awk '/^SKIP /{n++} END{print n+0}' "$tmp_status")

    if [ "$stale_count" -gt 0 ]; then
        printf 'WARN: stale-tarball advisory: %s tarball(s) older than their defconfig commit\n' "$stale_count" >&2
        grep '^STALE ' "$tmp_status" | while read -r marker target def_ts tar_ts; do
            def_iso=$(date -u -d "@$def_ts" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo "@$def_ts")
            tar_iso=$(date -u -d "@$tar_ts" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo "@$tar_ts")
            printf '  WARN: %s tarball mtime=%s < defconfig commit=%s -- non-S9 rebuild unavailable; see scripts/rebuild_all_non_s9.sh --list\n' \
                "$target" "$tar_iso" "$def_iso" >&2
        done
    fi

    pass "stale-tarball advisory: stale=$stale_count fresh=$fresh_count skip=$skip_count (informational; never fails)"

    rm -f "$tmp_status"
    return 0
}

stale_tarball_advisory_gate

#
# W6.5 hardware-rule grep guards (DCENT_QA, 2026-05-07).
#
# Five static checks that pin memory-rule invariants into the build pipeline.
# Each gate is grep-only (no compilation, no live hardware); each cites the
# memory rule it backstops so a future regressor sees the link from CI fail
# to the durable rationale.
#
# 1. `set_enabled(false)` may only appear in comments/docstrings or inside
#    sanctioned cold-boot/passthrough init paths. The HAL exposes the
#    method (it is the literal CTRL_REG-zero primitive) but the daemon
#    must NEVER call it on a chain that has had UART traffic — see
#    . Today there are zero in-code
#    call sites; we pin that.
#
# 2. The 3-byte SHORT-form RESET `[0x55, 0xAA, 0x07]` byte literal is
#    banned from `dspic.rs` and from any `s19j_*` source file. It is the
#    proven non-bootloader-entry form on .139 fw=0x89 dsPICs (-4
#    synthesis 2026-04-27). Only the 6-byte FRAMED RESET
#    `[55 AA 04 07 00 0B]` is permitted, and only in the recovery-tool
#    binary..
#
# 3. `JUMP_TO_APP` (0x06) must always be preceded by a raw-byte check
#    that rejects 0x60 (already-in-app) — sending JUMP to an app-mode
#    PIC drops it back into bootloader and corrupts its state machine
#    (S9 stock PIC bug, 2026-03-12). We grep for `JUMP_TO_APP` /
#    `jump_to_app` call sites and require a prior `read_pic_raw` /
#    `i2c_read_byte` / `read_byte` within 20 lines OR the call must be
#    inside the recovery-tool binary (where bootloader-only invocation
#    is the explicit intent).
#
# 4. Each `set_voltage` call site outside the HAL primitive layer must
#    be inside a stable-heartbeat gate. The architectural pattern is a
#    `stable_heartbeat_ticks >= 5` (or `< 5` early-return) check in the
#    enclosing function, OR the call happens inside `stock_mining.rs`
#    cold-boot init OR inside `cold_boot_sequence` / `*_init_bypass`
#    paths..
#
# 5. The voltage-command channel architecture itself must remain wired:
#    `daemon.rs` must contain BOTH the `stable_heartbeat_ticks` counter
#    AND the `< 5` early-block branch. Removing either silently
#    bypasses the gate.
#

# Gate W6.5-1: set_enabled(false) must be comment-only or HAL-impl.
set_enabled_false_check() {
    targets='
        dcentrald/dcentrald
        dcentrald/dcentrald-asic
        dcentrald/dcentrald-thermal
        dcentrald/dcentrald-api
        dcentrald/dcentrald-autotuner
        dcentrald/dcentrald-diagnostics
    '
    hits=''
    for d in $targets; do
        [ -d "$d" ] || continue
        # Find lines with `.set_enabled(false)` or `set_enabled(false)`,
        # then drop comment-only lines (// ... or /// ...).
        candidate=$(grep -rnE '\.set_enabled\(false\)|fn set_enabled\(' "$d" --include='*.rs' 2>/dev/null \
            | awk -F: '{
                orig=$0
                line=$0
                # strip "FILE:LINENO:" prefix (two leading colon-fields)
                sub(/^[^:]+:[0-9]+:/, "", line)
                sub(/^[[:space:]]+/, "", line)
                # skip pure-comment lines
                if (line ~ /^\/\//) next
                if (line ~ /^\/\*/) next
                if (line ~ /^\*/) next
                # The HAL primitive itself (`pub fn set_enabled`) is allowed.
                if (line ~ /^pub fn set_enabled/) next
                if (line ~ /fn set_enabled\(/) next
                print orig
            }' \
            || true)
        # Filter the .set_enabled(false) hit list down to actual call sites.
        callers=$(printf '%s\n' "$candidate" | grep -F '.set_enabled(false)' || true)
        if [ -n "$callers" ]; then
            if [ -z "$hits" ]; then
                hits=$callers
            else
                hits="$hits
$callers"
            fi
        fi
    done
    if [ -n "$hits" ]; then
        fail "W6.5-1 set_enabled(false): forbidden call site detected (regression of feedback_never_set_enabled_false.md)"
        printf '%s\n' "$hits" >&2
    else
        pass "W6.5-1 set_enabled(false): zero non-comment call sites in daemon/asic/thermal/api/autotuner/diagnostics"
    fi
}
set_enabled_false_check

# Gate W6.5-2: 0x07 SHORT-form RESET banned from dspic.rs / dspic/*.rs / s19j_*.
#
#  W24-CI-2 (BD-1): the original `-name 'dspic.rs'` glob was BLIND to
# the entire `dspic/` directory module — the dsPIC sources moved from a flat
# `dspic.rs` into `dcentrald-asic/src/dspic/{mod,fw82,fw86,fw89,fw8a,
# recovery_fw86,bosminer_warmup}.rs`, so this gate (the tripwire for the
# `a lab unit`/`a lab unit` bare-RESET corruption class) was silently green and never
# inspected the dsPIC code at all. The find now also walks `*/dspic/*.rs`.
#
# `dspic/bosminer_warmup.rs` LEGITIMATELY contains the `[0x55, 0xAA, 0x07]`
# RESET literal (lines ~150, ~427), but there it is SAFE BY CONSTRUCTION: the
# wrapper always emits a 19-byte parser-flush transaction immediately BEFORE
# the RESET in the same `build_prelude_transactions()` call (the `a lab unit`
# corruption was a BARE RESET to a chip in unknown parser state — structurally
# impossible here). So `bosminer_warmup.rs` is allow-listed by name; to make
# sure that allow-list never masks a future genuinely-unsafe BARE RESET added
# to that file, the flush-before-RESET shape is independently PINNED below
# (`bosminer_warmup_flush_before_reset_pin`). A bare RESET added to any OTHER
# `dspic/*.rs` (mod/fw82/fw86/fw89/fw8a/recovery_fw86) still trips the gate.
short_form_reset_check() {
    targets=$(find dcentrald -type f \
        \( -path '*/dspic/*.rs' -o -name 'dspic.rs' -o -name 's19j_*.rs' \) \
        2>/dev/null)
    inspected_dspic_dir_files=0
    hits=''
    for f in $targets; do
        [ -f "$f" ] || continue
        case "$f" in
            */dspic/*) inspected_dspic_dir_files=$((inspected_dspic_dir_files + 1)) ;;
        esac
        # `dspic/bosminer_warmup.rs` is the sanctioned flush-prefixed RESET
        # site — allow-listed by name (its safe-by-construction shape is
        # separately pinned by bosminer_warmup_flush_before_reset_pin).
        case "$f" in
            */dspic/bosminer_warmup.rs) continue ;;
        esac
        # Match the 3-byte literal `[0x55, 0xAA, 0x07]` (with optional
        # whitespace + commas around the bytes). Skip comment lines.
        candidate=$(grep -nE '\[[[:space:]]*0x55[[:space:]]*,?[[:space:]]*0xAA[[:space:]]*,?[[:space:]]*0x07[[:space:]]*[,\]]' "$f" 2>/dev/null \
            | awk '{
                orig=$0
                line=$0
                sub(/^[0-9]+:/, "", line)
                sub(/^[[:space:]]+/, "", line)
                if (line ~ /^\/\//) next
                if (line ~ /^\/\*/) next
                if (line ~ /^\*/) next
                print FILENAME ":" orig
            }' FILENAME="$f" \
            || true)
        if [ -n "$candidate" ]; then
            if [ -z "$hits" ]; then
                hits=$candidate
            else
                hits="$hits
$candidate"
            fi
        fi
    done
    if [ -n "$hits" ]; then
        fail "W6.5-2 short-form-reset: 0x07 SHORT-form RESET literal [0x55,0xAA,0x07] found in dspic.rs / dspic/*.rs / s19j_* (regression of feedback_pic_no_reset_s19j.md — Wave 1-4 banned this form)"
        printf '%s\n' "$hits" >&2
    else
        pass "W6.5-2 short-form-reset: no [0x55,0xAA,0x07] literal in dspic.rs / dspic/*.rs (except sanctioned bosminer_warmup.rs) / s19j_* sources"
    fi
    # Self-test: the gate MUST actually inspect the dspic/ directory module.
    # If the find ever stops matching dspic/*.rs (path rename, build-tree
    # reshuffle), this catches the glob-hole that W24-CI-2 just closed instead
    # of silently going green again.
    if [ "$inspected_dspic_dir_files" -ge 1 ]; then
        pass "W6.5-2 short-form-reset selftest: gate inspected $inspected_dspic_dir_files file(s) under dspic/ directory module"
    else
        fail "W6.5-2 short-form-reset selftest: gate inspected ZERO files under */dspic/*.rs — the dsPIC module is unscanned (glob hole reopened; see W24-CI-2 / BD-1)"
    fi
}
short_form_reset_check

# Gate W6.5-2b: pin the flush-before-RESET shape in dspic/bosminer_warmup.rs.
#
# `bosminer_warmup.rs` is allow-listed in W6.5-2 because its RESET is
# safe-by-construction: the 19-byte parser flush (`[0x55, 0xAA, 0x00] + 16×00`)
# is emitted in the same call, immediately before the `[0x55, 0xAA, 0x07]`
# RESET. This gate makes that allow-list HONEST — it asserts the three
# structural invariants that keep the RESET safe, so a future edit that strips
# the flush (turning the sanctioned site into a bare RESET) FAILS CI here even
# though the file stays allow-listed in W6.5-2. Pins:
#   1. the 19-byte flush payload [0x55, 0xAA, 0x00, 0x00*16] exists,
#   2. the RESET frame [0x55, 0xAA, 0x07] exists,
#   3. the structural unit tests that order flush(tx0) -> reset(tx1) are present
#      (`step_0_is_per_byte_parser_flush` + `step_1_is_reset_opcode_*`).
bosminer_warmup_flush_before_reset_pin() {
    f=$(find dcentrald -type f -path '*/dspic/bosminer_warmup.rs' 2>/dev/null | head -n 1)
    if [ -z "$f" ] || [ ! -f "$f" ]; then
        # File absent => the W6.5-2 allow-list has nothing to mask. Not a
        # failure: a checkout without the bosminer warmup module simply has no
        # sanctioned flush-prefixed RESET site to protect.
        pass "W6.5-2b warmup-flush-pin: dspic/bosminer_warmup.rs not present (nothing to allow-list; skip)"
        return
    fi
    missing=''
    # 1. 19-byte parser-flush payload header [0x55, 0xAA, 0x00].
    if ! grep -qE 'bytes\.push\(0x55\)' "$f" || ! grep -qE 'bytes\.push\(0x00\)' "$f"; then
        missing="$missing parser-flush-payload"
    fi
    # 2. RESET frame literal must still be present (proves we are pinning the
    #    real site, not a renamed/empty file).
    if ! grep -qE '\[[[:space:]]*0x55[[:space:]]*,[[:space:]]*0xAA[[:space:]]*,[[:space:]]*0x07[[:space:]]*\]' "$f"; then
        missing="$missing reset-frame-literal"
    fi
    # 3. The structural ordering tests that prove flush(tx[0]) precedes
    #    reset(tx[1]) must remain.
    if ! grep -qE 'fn step_0_is_per_byte_parser_flush' "$f"; then
        missing="$missing flush-is-tx0-test"
    fi
    if ! grep -qE 'fn step_1_is_reset_opcode' "$f"; then
        missing="$missing reset-is-tx1-test"
    fi
    if [ -n "$missing" ]; then
        fail "W6.5-2b warmup-flush-pin: dspic/bosminer_warmup.rs lost flush-before-RESET invariant(s):$missing — the W6.5-2 allow-list for this file is no longer safe-by-construction (regression of feedback_pic_no_reset_s19j.md)"
    else
        pass "W6.5-2b warmup-flush-pin: dspic/bosminer_warmup.rs keeps the 19-byte flush before [0x55,0xAA,0x07] RESET (flush=tx0, reset=tx1 ordering tests present) — allow-list stays safe-by-construction"
    fi
}
bosminer_warmup_flush_before_reset_pin

# Gate W6.5-3: JUMP_TO_APP / jump_to_app must be preceded by a raw-byte check.
jump_to_app_check() {
    targets='
        dcentrald/dcentrald
        dcentrald/dcentrald-asic
        dcentrald/dcentrald-hal
    '
    hits=''
    for d in $targets; do
        [ -d "$d" ] || continue
        # Match call sites: `JUMP_TO_APP`, `JUMP_FROM_LOADER`, `jump_to_app(`.
        # Skip docstring/comment lines and skip `const`/`pub const` definitions.
        files=$(grep -rlE 'JUMP_TO_APP|JUMP_FROM_LOADER|jump_to_app\(' "$d" --include='*.rs' 2>/dev/null \
            | grep -vE 'pic-recovery|dspic_flash\.rs|dspic_frame\.rs|stock_fpga_iic\.rs|i2c\.rs|/pic/mod\.rs|/dspic/mod\.rs|/dspic/recovery_fw86\.rs|pic\.rs|dspic\.rs' \
            || true)
        # NOTE on the file-name exclusion list above:
        #   - pic.rs / dspic.rs both DEFINE the `jump_to_app` primitive AND
        #     contain its single sanctioned call site, which is gated by an
        #     extensive raw-byte (`raw_state == 0xCC`, `needs_jump`,
        #     `pre_detect_raw`) check chain that lives in a parent `if !needs_jump`
        #     block far outside the 20-line window. We trust those two files
        #     by-construction and audit any new call site OUTSIDE of them.
        #   - Their internal call site is independently locked down by the
        #     existing dspic.rs `jump_to_app banned` panic test (line ~2741)
        #     which W6.5-3 does not need to re-prove.
        for f in $files; do
            [ -f "$f" ] || continue
            # Find call-site line numbers (skip comments, skip const definitions).
            lines=$(grep -nE 'JUMP_TO_APP|JUMP_FROM_LOADER|jump_to_app\(' "$f" 2>/dev/null \
                | awk -F: '{
                    line=$0
                    ln=$1
                    sub(/^[0-9]+:/, "", line)
                    sub(/^[[:space:]]+/, "", line)
                    if (line ~ /^\/\//) next
                    if (line ~ /^\/\*/) next
                    if (line ~ /^\*/) next
                    if (line ~ /^pub const/) next
                    if (line ~ /^const/) next
                    if (line ~ /^use /) next
                    print ln
                }' \
                || true)
            for ln in $lines; do
                start=$((ln - 60))
                if [ "$start" -lt 1 ]; then start=1; fi
                window=$(sed -n "${start},${ln}p" "$f")
                # Acceptable preceding patterns: any raw-byte / version read
                # OR an explicit BootloaderOnly / cold-boot context.
                if printf '%s\n' "$window" | grep -qE 'read_pic_raw|read_raw_byte|i2c_read_byte|read_byte|pic_raw|raw_read|raw == 0xCC|== 0xCC|GET_VERSION|get_version|detect_firmware|in_bootloader|is_bootloader|BootloaderOnly|in_app_mode|needs_jump|pre_detect_raw|raw_state|cold_boot|cold-boot|COLD BOOT'; then
                    continue
                fi
                offender="$f:$ln: $(sed -n "${ln}p" "$f")"
                if [ -z "$hits" ]; then
                    hits=$offender
                else
                    hits="$hits
$offender"
                fi
            done
        done
    done
    if [ -n "$hits" ]; then
        fail "W6.5-3 jump_to_app: JUMP_TO_APP/jump_to_app call site without preceding raw-byte/version check within 20 lines (regression of feedback_pic_no_reset_s19j.md / S9 stock PIC 0xCC-vs-0x60 bug)"
        printf '%s\n' "$hits" >&2
    else
        pass "W6.5-3 jump_to_app: every JUMP_TO_APP / jump_to_app call site is preceded by a raw-byte / version check (or lives in the recovery-tool binary)"
    fi
}
jump_to_app_check

# Gate W6.5-4: set_voltage call sites must be heartbeat-stability-gated or
# inside cold-boot init / bypass paths.
set_voltage_gate_check() {
    # Sanctioned bypass paths: cold-boot init voltage, set_voltage_init_bypass,
    # set_voltage_min (panic-safe rail collapse), and HAL primitive impls.
    sanctioned_files='
        dcentrald/dcentrald-hal/src/psu.rs
        dcentrald/dcentrald-hal/src/i2c.rs
        dcentrald/dcentrald-asic/src/pic.rs
        dcentrald/dcentrald-asic/src/dspic.rs
        dcentrald/dcentrald-asic/src/dspic_flash.rs
        dcentrald/dcentrald-asic/src/i2c_service.rs
        dcentrald/dcentrald-asic/src/dspic_service.rs
        dcentrald/dcentrald-asic/src/pic0x89_service.rs
        dcentrald/pic-recovery/src/main.rs
    '
    # Files where set_voltage calls must each be inside a stability gate.
    callers='
        dcentrald/dcentrald/src/daemon.rs
        dcentrald/dcentrald/src/work_dispatcher.rs
        dcentrald/dcentrald/src/s19j_hybrid_mining.rs
        dcentrald/dcentrald/src/serial_mining.rs
        dcentrald/dcentrald/src/stock_mining.rs
        dcentrald/dcentrald-autotuner
    '
    hits=''
    for d in $callers; do
        [ -e "$d" ] || continue
        files=''
        if [ -d "$d" ]; then
            files=$(find "$d" -type f -name '*.rs' 2>/dev/null)
        else
            files="$d"
        fi
        for f in $files; do
            [ -f "$f" ] || continue
            # Find set_voltage call sites (skip definitions, comments, and
            # the panic-safe set_voltage_min rail-collapse path).
            lines=$(grep -nE '\.set_voltage\(' "$f" 2>/dev/null \
                | awk -F: '{
                    line=$0
                    ln=$1
                    sub(/^[0-9]+:/, "", line)
                    sub(/^[[:space:]]+/, "", line)
                    if (line ~ /^\/\//) next
                    if (line ~ /^\/\*/) next
                    if (line ~ /^\*/) next
                    if (line ~ /^pub fn set_voltage/) next
                    if (line ~ /^fn set_voltage/) next
                    # set_voltage_min/_init_bypass are sanctioned bypass paths
                    if (line ~ /\.set_voltage_min\(/) next
                    if (line ~ /\.set_voltage_init_bypass\(/) next
                    if (line ~ /\.set_voltage_max_safe\(/) next
                    print ln
                }' \
                || true)
            for ln in $lines; do
                start=$((ln - 60))
                if [ "$start" -lt 1 ]; then start=1; fi
                end=$((ln + 5))
                window=$(sed -n "${start},${end}p" "$f")
                # Acceptable enclosing patterns:
                #   - stable_heartbeat_ticks gate
                #   - cold_boot / cold-boot init context
                #   - INIT_VOLTAGE_DAC / DEFAULT_VOLTAGE_DAC (cold-boot init)
                #   - LAB-ONLY / TRUST-RAIL fallback
                #   - am2_safe_teardown_sequence (2026-05-19 reconciliation):
                #     the deferred-voltage-stability rule
                # protects
                #     the COLD-BOOT/STARTUP regime — a SET_VOLTAGE NACK before
                #     the PIC heartbeat is stable corrupts the MSSP parser.
                #     The orderly/fail-closed TEARDOWN coast-down (walk rail
                #     to floor so chips coast down before HBx_RESET drain) is
                #     the OPPOSITE phase: the PIC has heartbeated throughout
                #     the run, and on a fail-closed teardown you CANNOT and
                #     MUST NOT wait for "5 stable heartbeat ticks" (the PIC
                #     may already be dead — that's why teardown is
                #     best-effort, errors logged-not-propagated, with the
                #     run-scope hard-stop guard as the final net). This is a
                #     NARROW carve-out for the single named teardown function
                #     only — it cannot mask a cold-boot-init regression (a
                #     different function/context). NO firmware change; this
                #     reconciles the gate allowlist to the structurally-
                #     sanctioned teardown context (gate-vs-code drift since
                #     the teardown sequence landed ~2026-05-15).
                if printf '%s\n' "$window" | grep -qE 'stable_heartbeat_ticks|stable_heartbeats|heartbeat_stable|cold_boot|cold-boot|INIT_VOLTAGE_DAC|DEFAULT_VOLTAGE_DAC|init_voltage|init voltage|set_voltage_init_bypass|am2_safe_teardown_sequence|safe-teardown sequence|TRUST-RAIL|trust_rail|LAB-ONLY|voltage_stability|deferred_voltage|pending_voltage'; then
                    continue
                fi
                offender="$f:$ln: $(sed -n "${ln}p" "$f")"
                if [ -z "$hits" ]; then
                    hits=$offender
                else
                    hits="$hits
$offender"
                fi
            done
        done
    done
    if [ -n "$hits" ]; then
        fail "W6.5-4 set_voltage-gate: set_voltage call site without stable_heartbeat_ticks gate / cold-boot init context (regression of feedback_deferred_voltage_stability_gate.md)"
        printf '%s\n' "$hits" >&2
    else
        pass "W6.5-4 set_voltage-gate: every set_voltage call site is inside a stable_heartbeat_ticks gate or a sanctioned cold-boot init / bypass path"
    fi
    # Note: sanctioned_files list documents which files own the primitive
    # implementations. They are intentionally excluded from the caller scan.
    : "$sanctioned_files"
}
set_voltage_gate_check

# Gate W6.5-5: deferred-voltage architecture must remain wired in daemon.rs.
deferred_voltage_arch_check() {
    f='dcentrald/dcentrald/src/daemon.rs'
    if [ ! -f "$f" ]; then
        fail "W6.5-5 deferred-voltage-arch: missing $f"
        return
    fi
    if ! grep -q 'stable_heartbeat_ticks' "$f"; then
        fail "W6.5-5 deferred-voltage-arch: stable_heartbeat_ticks counter missing from $f"
        return
    fi
    if ! grep -qE 'stable_heartbeat_ticks[[:space:]]*<[[:space:]]*5' "$f"; then
        fail "W6.5-5 deferred-voltage-arch: '< 5' early-block branch missing from $f (regression of feedback_deferred_voltage_stability_gate.md)"
        return
    fi
    pass "W6.5-5 deferred-voltage-arch: stable_heartbeat_ticks counter + '< 5' early-block branch wired in daemon.rs"
}
deferred_voltage_arch_check

#
# Gate W6.5-6: BIP320 `version_bits_raw != 0` rejection-guard ban
# (DCENT_Protocol + DCENT_QA, 2026-05-15).
#
# The single most load-bearing mining-correctness contract on the
# BM1362-family chip-side BIP320 paths is that the share-submit loop must
# NEVER pre-filter parsed nonces with the form
#
#     if nr.version_bits_raw != 0 { continue; }
#
# That guard discarded ~95% of valid hashing work on AM2 XIL `a lab unit`
# (the 4655-RX-frames-0-nonces failure of 2026-05-15 morning) and cost
# the fifth-platform milestone an entire diagnostic session before it was
# deleted in the cross-platform Protocol fix sweep (post-`2b6d46f3`).
# `validate_full_header(header_with_rolled_version, share_target)` is the
# SOLE local gate; the rolled version is reconstructed via the shared
# `bm1362::bip320_reconstruct_rolled_version` helper. See memory rules
# ,
# ,
# and .
#
# Sibling W6.5 gates already grep-ban `.set_enabled(false)` and the
# 0x07 SHORT-form RESET literal; until now this contract was pinned only
# by the Rust `bip320_tests` module (a `cargo test -p dcentrald-asic`
# gate) plus prose regression-pins, with NO automated grep gate. This
# closes that gap.
#
# Scope: the four BM1362-family share-submit modules. The match is
# deliberately narrow — it fires on the actual *rejection guard*
# (a conditional on `version_bits_raw` being non-zero whose body is
# `continue`), in both the compact one-line form and the
# `if ... != 0 {` / `continue;` two-line form. It must NOT fire on:
#   * the obsolete-rejection PROSE block at
#     `s19j_hybrid_mining.rs` (a comment-only regression-pin that
#     legitimately quotes the banned pattern),
#   * the `bm1362::uart_transport` docstring that names
#     `version_bits_raw` + "rejection guard" in `///` comment lines,
#   * legitimate non-guard uses such as
#     `let distinct_midstates = ... version_bits_raw != 0;`
#     (a dedup boolean — no `continue`),
#   * test names / comments that mention the contract.
# Comment-only lines are stripped with the same awk pass the sibling
# W6.5 gates use.
#
bip320_rejection_guard_check() {
    if [ -n "${DCENT_BIP320_REJECTION_GUARD_TARGETS:-}" ]; then
        targets=$DCENT_BIP320_REJECTION_GUARD_TARGETS
    else
        targets='
            dcentrald/dcentrald/src/s19j_hybrid_mining.rs
            dcentrald/dcentrald/src/am3_bb_mining.rs
            dcentrald/dcentrald/src/serial_mining.rs
            dcentrald/dcentrald/src/work_dispatcher.rs
        '
    fi
    hits=''
    for f in $targets; do
        [ -f "$f" ] || continue

        # --- One-line compact form -------------------------------------
        # Matches `if <expr>version_bits_raw<expr> != 0 <expr> { ...
        # continue ... }` on a single line. The `[._a-zA-Z0-9 ]*`
        # tolerance around the identifier accepts `nr.version_bits_raw`,
        # `entry .version_bits_raw`, `version_bits_raw as u32`, etc.
        # Requires `continue` after the `{` so a non-guard boolean use
        # (`let x = ... version_bits_raw != 0;`) never matches.
        oneline=$(grep -nE \
            'if[[:space:]].*version_bits_raw[[:space:]._a-zA-Z0-9()]*!=[[:space:]]*0[^{]*\{[^}]*continue' \
            "$f" 2>/dev/null \
            | awk -v fn="$f" '{
                orig=$0
                line=$0
                sub(/^[0-9]+:/, "", line)
                sub(/^[[:space:]]+/, "", line)
                if (line ~ /^\/\//) next
                if (line ~ /^\/\*/) next
                if (line ~ /^\*/) next
                print fn ":" orig
            }' \
            || true)

        # --- Two-line form ---------------------------------------------
        # `if <...>version_bits_raw<...> != 0 <...> {` on one line, then
        # the very next non-blank source line is `continue;` (the
        # classic guard split across two lines). Use awk to carry the
        # candidate-open across lines, skipping comment-only lines so the
        # prose regression-pins do not match.
        twoline=$(awk '
            function strip(s) {
                sub(/^[[:space:]]+/, "", s)
                return s
            }
            {
                raw=$0
                code=strip($0)
                is_comment = (code ~ /^\/\//) || (code ~ /^\/\*/) || (code ~ /^\*/)
                if (is_comment) { next }
                if (code == "") { next }

                if (pending_open) {
                    if (code ~ /^continue[[:space:]]*;/) {
                        print FILENAME ":" open_lineno ": " open_text
                    }
                    pending_open = 0
                }

                if (code ~ /if[[:space:]].*version_bits_raw[[:space:]._a-zA-Z0-9()]*!=[[:space:]]*0[^{]*\{[[:space:]]*$/) {
                    pending_open = 1
                    open_lineno = FNR
                    open_text = code
                }
            }
        ' "$f" 2>/dev/null || true)

        for chunk in "$oneline" "$twoline"; do
            if [ -n "$chunk" ]; then
                if [ -z "$hits" ]; then
                    hits=$chunk
                else
                    hits="$hits
$chunk"
                fi
            fi
        done
    done
    if [ -n "$hits" ]; then
        fail "W6.5-6 bip320-rejection-guard: 'if ... version_bits_raw != 0 { continue }' rejection guard found in a BM1362 share-submit module (regression of feedback_am2_serial_dispatch_bip320_version_rolling_required.md — discards ~95% of valid AM2 hashing work; validate_full_header is the SOLE gate)"
        printf '%s\n' "$hits" >&2
    else
        pass "W6.5-6 bip320-rejection-guard: no version_bits_raw!=0 rejection guard in s19j_hybrid/am3_bb/serial_mining/work_dispatcher (rolled-version reconstruction + validate_full_header remain the only gate)"
    fi
}
bip320_rejection_guard_check

# W6.8 chip_geometry drift gate (DCENT_Perf, 2026-05-07).
#
# The legacy `chip_geometry::*_CORES` constants in
# `dcentrald-autotuner/src/lib.rs` drifted 30% out of sync with
# `dcentrald-asic::drivers::MinerProfile::cores_per_chip` (autotuner had
# 894 for BM1368 while MinerProfile carried the corrected 1280 from the
# S21 fixture RE). The autotuner now consumes
# `MinerProfile::nonce_attribution_cores` directly. This gate refuses any
# regression that reintroduces a per-chip `chip_geometry::BM*_CORES`
# constant — single source of truth lives in `dcentrald-asic`, not in
# the autotuner. See module docs in `dcentrald-asic/src/drivers/mod.rs`
# for the engine-vs-slot distinction.
#
chip_geometry_drift_check() {
    autotuner_dir='dcentrald/dcentrald-autotuner/src'
    if [ ! -d "$autotuner_dir" ]; then
        pass "chip_geometry-drift: autotuner dir not present (skipping)"
        return
    fi
    hits=$(grep -rEn 'chip_geometry::[A-Z0-9_]*_CORES' "$autotuner_dir" 2>/dev/null || true)
    if [ -n "$hits" ]; then
        fail "chip_geometry::*_CORES drift regression — autotuner must consume MinerProfile::nonce_attribution_cores"
        printf '%s\n' "$hits" >&2
    else
        pass "chip_geometry-drift: autotuner uses MinerProfile single-source-of-truth"
    fi
}
chip_geometry_drift_check

# Phase 4J regression slice (2026-05-15): offline log-replay of platform
# milestone runs. Catches schema drift / counter rename in `dcentrald`'s
# structured-log surface. Pure-text -- the script does NOT contact any
# miner, re-run the binary, or simulate hashing.
#
# Each platform replay asserts that the captured milestone log's
# `am2_serial_status` counters (`total_work`, `total_rx_frames`,
# `total_nonces`, `shares_submitted`) plus an independent count of
# "share accepted" lines still match the per-platform floor profile in
# `tools/replay_milestone_log.py` PLATFORM_PROFILES.
#
# Adding a new platform: drop the milestone log under
# , add a row below, add (or amend) the platform
# entry in PLATFORM_PROFILES. New platforms shipped without a milestone
# log emit an explicit SKIP line so the absence is documented rather than
# counted as a green proof.
regression_am2_xil_check() {
    if ! command -v python >/dev/null 2>&1 && ! command -v python3 >/dev/null 2>&1; then
        pass "Phase 4J regression-am2-xil: python interpreter unavailable (skip)"
        return
    fi
    PY=python
    command -v python >/dev/null 2>&1 || PY=python3

    REPLAY_SCRIPT="$PROJECT_DIR/../../tools/replay_milestone_log.py"
    if [ ! -f "$REPLAY_SCRIPT" ]; then
        fail "Phase 4J regression-am2-xil: missing $REPLAY_SCRIPT"
        return
    fi

    # platform_short_name : milestone_log_relative_path
    set -- \
        "am2-xil:../../docs/dev/2026-05-14-xil-s19jpro-resume/logs/2026-05-15-dcentrald-xil-FIRST-ACCEPTED-SHARES.log" \
        "am3-bb:../../docs/dev/2026-05-13-am3-bb-blocker-fix/live-captures/dcentos-publicpool-share-79-20260513T2200Z.log"

    any_ran=0
    for entry in "$@"; do
        plat=$(printf '%s' "$entry" | cut -d: -f1)
        rel=$(printf '%s' "$entry" | cut -d: -f2-)
        log_path="$PROJECT_DIR/$rel"
        if [ ! -f "$log_path" ]; then
            # Not every checkout ships every milestone log. Emit SKIP, not PASS.
            printf 'SKIP: Phase 4J regression-%s: milestone log not present\n' "$plat"
            continue
        fi
        any_ran=1
        out=$("$PY" "$REPLAY_SCRIPT" --log "$log_path" --platform "$plat" 2>&1)
        rc=$?
        if [ "$rc" -eq 0 ]; then
            pass "Phase 4J regression-$plat: counters within floor for $log_path"
        else
            fail "Phase 4J regression-$plat: replay assertion failed for $log_path"
            printf '%s\n' "$out" >&2
        fi
    done

    if [ "$any_ran" -eq 0 ]; then
        # If NO milestone log was found, emit SKIP rather than a green PASS.
        printf 'SKIP: Phase 4J regression-am2-xil: no milestone logs found in this checkout\n'
    fi
}
regression_am2_xil_check

# Phase 4G regression slice (2026-05-15): cross-family mining-proof
# regression. Synthesizes s99verify-equivalent state.json + s99verify.json
# fixtures for the five proven mining milestones (am1-s9, am2-s17,
# am2-XIL, am3-bb, am3-aml) and runs the Phase 4H verifier
# (`dcent_toolbox.core.verifier`) offline against each.
#
# This complements the Phase 4J log-replay slice. Phase 4J catches
# schema drift in the running binary's structured log surface; Phase 4G
# catches regressions in the post-install verifier itself — the
# host-side classifier that gates whether "install completed" implies
# "install produces hashrate".
#
# The slice runs in two modes:
#   1. main slice — all five milestones must classify PROVEN
#   2. self-test  — tamper the first milestone four ways (drop chain
#                   count, NULL first nonce, push share past budget,
#                   yield below floor) and verify each tamper flips
#                   the verifier verdict away from PROVEN
#
# Silent skip when Python is unavailable (same convention as Phase 4J).
regression_cross_family_check() {
    if ! command -v python >/dev/null 2>&1 && ! command -v python3 >/dev/null 2>&1; then
        pass "Phase 4G regression-cross-family: python interpreter unavailable (skip)"
        return
    fi
    PY=python
    command -v python >/dev/null 2>&1 || PY=python3

    SLICE_SCRIPT="$PROJECT_DIR/../../tools/regression_cross_family.py"
    if [ ! -f "$SLICE_SCRIPT" ]; then
        fail "Phase 4G regression-cross-family: missing $SLICE_SCRIPT"
        return
    fi

    # Main slice — all five milestones must classify PROVEN.
    out=$("$PY" "$SLICE_SCRIPT" 2>&1)
    rc=$?
    if [ "$rc" -eq 0 ]; then
        pass "Phase 4G regression-cross-family: 5 of 5 milestones classified PROVEN"
        printf '%s\n' "$out" | sed -n 's/^OK    /  /p'
    else
        fail "Phase 4G regression-cross-family: at least one milestone not PROVEN"
        printf '%s\n' "$out" >&2
        return
    fi

    # Self-test — tamper detection must fire.
    out=$("$PY" "$SLICE_SCRIPT" --self-test 2>&1)
    rc=$?
    if [ "$rc" -eq 0 ]; then
        pass "Phase 4G regression-cross-family self-test: 4 of 4 tamper modes detected"
    else
        fail "Phase 4G regression-cross-family self-test: tamper detection regression"
        printf '%s\n' "$out" >&2
    fi
}
regression_cross_family_check

# =====================================================================
# DEVOPS/QA/SW/RE supply-chain + release + safety gates (2026-06-02).
# Static, offline, source-only. These close audit findings:
#   SW-05 / DEVOPS-003 / DEVOPS-004 — release-image hardening wiring
#   QA-004 / QA-009                 — workspace test gate wired into CI
#   QA-007                          — BIP320 rejection-guard ban-gate present
#   QA-006                          — devmem PWM writes <= 30 in all overlays
#   QA-002                          — BM1387 triple-write MiscCtrl count=3/5ms
#   RE-007                          — unconfirmed scaffold drivers fail-closed
# =====================================================================

#
# SW-05 / DEVOPS-003 / DEVOPS-004: release-image trust-boundary wiring.
#
# A PRODUCTION/RELEASE image (DCENT_RELEASE_IMAGE=1 at Buildroot time) MUST:
#   (a) have the release-image stamp hook wired into every board post-build.sh
#       (scripts/lib/release_image_provision.sh → dcent_provision_release_image)
#       so /etc/dcentos/release-image is stamped;
#   (b) gate the raw-HW MCP endpoint (S81mcp, port 3000) on that marker so it
#       does NOT auto-start on a release unit;
#   (c) have dcentrald-api::auth consume the marker (is_release_image →
#       password required, passwordless opt-out disabled).
# DEV-open (no marker, MCP localhost, root:dcentral) is intentional. The
# blocker this gate closes is a SHIPPED release image silently MISSING the
# marker/gate — which would leave REST/MCP/dashboard open with the shared
# root cred. This gate proves the wiring exists in source so a release build
# cannot regress to dev-open posture unnoticed.
#
release_image_hardening_check() {
    # (a) the provisioning helper exists and stamps the marker.
    require_pattern \
        "scripts/lib/release_image_provision.sh" \
        "/etc/dcentos/release-image" \
        "SW-05a release-image: provision helper stamps /etc/dcentos/release-image"
    require_pattern \
        "scripts/lib/release_image_provision.sh" \
        "DCENT_RELEASE_IMAGE" \
        "SW-05a release-image: provision helper keys off DCENT_RELEASE_IMAGE"
    require_file 'scripts/test_release_image_provision.sh'
    if sh scripts/test_release_image_provision.sh >/dev/null 2>&1; then
        pass 'SW-05a release-image: adversarial provisioning lifecycle tests pass'
    else
        fail 'SW-05a release-image: adversarial provisioning lifecycle tests FAILED'
    fi

    # (a cont.) every activating board post-build.sh that ships a rootfs must
    # call the provisioning hook. A typed NOT_IMPLEMENTED compatibility hook
    # may be excluded only when its executable grammar is shell builtins that
    # print a refusal and terminate with EX_UNAVAILABLE (78).
    pb_missing=''
    pb_found=0
    pb_refused=0
    for pb in br2_external_dcentos/board/*/post-build.sh \
              br2_external_dcentos/board/*/*/post-build.sh; do
        [ -f "$pb" ] || continue
        pb_found=$((pb_found + 1))
        if grep -Fxq '# DCENT_BUILD_POLICY=not-implemented-refusal' "$pb"; then
            invalid_refusal=$(awk '
                /^[[:space:]]*#/ || /^[[:space:]]*$/ { next }
                $1 == "printf" { next }
                $0 == "exit 78" { exits++; next }
                { print NR ":" $0 }
                END { if (exits != 1) print "exit-count=" exits }
            ' "$pb")
            if [ -z "$invalid_refusal" ]; then
                pb_refused=$((pb_refused + 1))
                continue
            fi
            fail "SW-05a release-image: typed build refusal has activating or ambiguous grammar: $pb"
            printf '%s\n' "$invalid_refusal" >&2
            continue
        fi
        if ! grep -F -- 'dcent_provision_release_image' "$pb" >/dev/null 2>&1; then
            pb_missing="$pb_missing $pb"
        fi
    done
    if [ "$pb_found" -eq 0 ]; then
        fail "SW-05a release-image: no board post-build.sh files found (path drift?)"
    elif [ -n "$pb_missing" ]; then
        fail "SW-05a release-image: board post-build.sh missing dcent_provision_release_image call:$pb_missing"
    else
        pb_active=$((pb_found - pb_refused))
        pass "SW-05a release-image: all $pb_active activating post-build hooks call dcent_provision_release_image; $pb_refused typed refusal hook(s) excluded"
    fi

    # (b) S81mcp gates on the marker (does NOT auto-start on a release image).
    mcp_missing=''
    mcp_found=0
    for mcp in br2_external_dcentos/board/zynq/rootfs-overlay/etc/init.d/S81mcp \
               br2_external_dcentos/board/amlogic/rootfs-overlay/etc/init.d/S81mcp; do
        [ -f "$mcp" ] || continue
        mcp_found=$((mcp_found + 1))
        if ! grep -F -- '/etc/dcentos/release-image' "$mcp" >/dev/null 2>&1; then
            mcp_missing="$mcp_missing $mcp"
        fi
    done
    if [ "$mcp_found" -eq 0 ]; then
        fail "SW-05b release-image: no S81mcp init scripts found (path drift?)"
    elif [ -n "$mcp_missing" ]; then
        fail "DEVOPS-004 S81mcp: raw-HW MCP endpoint NOT gated on the release-image marker in:$mcp_missing (a release image would auto-start an unauthenticated localhost raw-HW endpoint)"
    else
        pass "DEVOPS-004 S81mcp: raw-HW MCP endpoint gated on /etc/dcentos/release-image in all $mcp_found S81mcp scripts"
    fi

    # (c) dcentrald-api::auth consumes the marker.
    auth_rs='dcentrald/dcentrald-api/src/auth.rs'
    if [ -f "$auth_rs" ]; then
        require_pattern "$auth_rs" "/etc/dcentos/release-image" \
            "SW-05c release-image: dcentrald-api auth.rs consumes the release-image marker"
    else
        # auth.rs is owned by another group; absence here is path drift, not a
        # hard release-blocker for this gate — warn via a soft pass.
        pass "SW-05c release-image: auth.rs not present at expected path (skipping marker-consume check)"
    fi
}
release_image_hardening_check

# CE-183: a release-status sysupgrade package must not decouple from
# release-image hardening (root SSH lockdown + /etc/dcentos/release-image
# marker). Pins the producer-side coupling (packaging lib + docker + standalone
# packager) and the daemon accept-side rejection of unsigned release bundles.
release_status_hardening_coupling_check() {
    require_pattern \
        'scripts/lib/sysupgrade_package_common.sh' \
        'dcent_require_release_image_hardening' \
        'CE-183a: packaging lib defines the release-status->release-image coupling'

    # The manifest writer must call the coupling gate BEFORE writing the
    # MANIFEST.json (which carries the release status). Function-body pin in the
    # same style as make_release_verify_gate_check.
    if awk '
        BEGIN { in_fn = 0; gate_line = 0; manifest_line = 0 }
        /^dcent_write_sysupgrade_manifest\(\) \{/ { in_fn = 1; next }
        in_fn && /^\}/ { in_fn = 0 }
        in_fn && /dcent_require_release_image_hardening/ { if (gate_line == 0) gate_line = NR }
        in_fn && /MANIFEST\.json/ { if (manifest_line == 0) manifest_line = NR }
        END {
            ok = gate_line > 0 && manifest_line > 0 && gate_line < manifest_line
            exit ok ? 0 : 1
        }
    ' scripts/lib/sysupgrade_package_common.sh; then
        pass 'CE-183b: manifest writer calls the coupling gate before writing status'
    else
        fail 'CE-183b: dcent_write_sysupgrade_manifest must call dcent_require_release_image_hardening before the MANIFEST.json heredoc'
    fi

    require_pattern \
        'scripts/build_in_docker.sh' \
        'Release status must not decouple from release-image hardening' \
        'CE-183c: docker producer fails fast on release-status without DCENT_RELEASE_IMAGE=1'

    if [ "$(grep -c -- '-e DCENT_RELEASE_IMAGE=' scripts/build_in_docker.sh)" -ge 2 ]; then
        pass 'CE-183d: Phase 7 S9 packaging container receives DCENT_RELEASE_IMAGE'
    else
        fail 'CE-183d: Phase 7 S9 packaging container must pass -e DCENT_RELEASE_IMAGE'
    fi

    require_pattern \
        'scripts/package_sysupgrade.sh' \
        'requires DCENT_RELEASE_IMAGE=1' \
        'CE-183e: standalone S9 packager enforces the coupling'

    require_pattern \
        'dcentrald/dcentrald-api/src/ota_signature.rs' \
        'allow_unsigned lab override does not apply to release-status' \
        'CE-183f: daemon rejects unsigned release-status bundles even in lab mode'
}
release_status_hardening_coupling_check

#
# QA-004 / QA-009: the workspace test compile-gate (run_dcentrald_tests.sh,
# `cargo test --no-run` for the real musl target) must be REFERENCED by an
# actual CI workflow. The script existed but was orphaned — no workflow ran
# it — which is exactly how SB-3 (a test with a broken include_str! path)
# shipped uncompiled for weeks. This gate proves a workflow invokes it.
#
test_gate_wired_check() {
    script='scripts/run_dcentrald_tests.sh'
    require_file "$script"

    # Workflows live at repo root .github/workflows/ (PROJECT_DIR/../../).
    wf_dir='../../.github/workflows'
    if [ ! -d "$wf_dir" ]; then
        fail "QA-004 test-gate: workflows dir $wf_dir not found (path drift?)"
        return
    fi
    if grep -rF -- 'run_dcentrald_tests.sh' "$wf_dir" >/dev/null 2>&1; then
        ref=$(grep -rl -- 'run_dcentrald_tests.sh' "$wf_dir" 2>/dev/null | tr '\n' ' ')
        pass "QA-004 test-gate: run_dcentrald_tests.sh referenced by CI workflow(s): $ref"
    else
        fail "QA-004/QA-009 test-gate: run_dcentrald_tests.sh exists but is NOT referenced by any .github/workflows/* — the musl 'cargo test --no-run' compile-gate never runs in CI (this is how an uncompiled test silently shipped). Add a workflow step that calls it."
    fi
}
test_gate_wired_check

#
# MCP-PROFILE-DRIFT: author-once / emit-twice / VALIDATE durability self-presence.
#
# The Python `:3000` control server (board/{zynq,amlogic}/.../web/mcp_server.py)
# carries a HAND-MIRRORED minimal_profile() dict + WRITE_TOOLS set copied from the
# Rust source of truth dcent-schema::mcp::minimal_profile(). That hand-mirror can
# silently re-drift (the MCP analog of the stale theme.ts the token contract calls
# out). The DURABILITY MECHANISM that closes it is the drift test
# projects/dcent-schema/tests/python_overlay_drift.rs, which drives its assertions
# FROM the Rust registry against BOTH overlay files (token-contract §0,
# UIVIS-RENDER-1, step 3 "does each emission match the contract").
#
# This gate is the self-presence meta-check (same shape as the bip320 ban-gate +
# test_gate_wired self-presence assertions): it proves the drift mechanism itself
# is not silently deleted, that it still include_str!s BOTH overlays and drives
# from the registry, and that the overlays still carry the "MUST stay byte-aligned"
# comment the test makes enforceable. Offline + fail-soft on path drift.
#
mcp_profile_drift_gate_present_check() {
    drift='../dcent-schema/tests/python_overlay_drift.rs'
    if [ ! -f "$drift" ]; then
        # dcent-schema is a sibling crate owned in the same workspace tree;
        # absence here is path drift, not a hard release-blocker for THIS gate.
        pass "MCP-PROFILE-DRIFT: drift test not present at expected sibling path (skipping — sibling crate path drift, not a release blocker)"
        return
    fi
    require_pattern "$drift" 'minimal_profile' \
        'MCP-PROFILE-DRIFT: drift test drives assertions from the Rust minimal_profile() registry'
    require_pattern "$drift" 'board/zynq/rootfs-overlay/root/web/mcp_server.py' \
        'MCP-PROFILE-DRIFT: drift test include_str!s the zynq overlay'
    require_pattern "$drift" 'board/amlogic/rootfs-overlay/root/web/mcp_server.py' \
        'MCP-PROFILE-DRIFT: drift test include_str!s the amlogic overlay (both overlays guarded)'
    require_pattern "$drift" 'WRITE_TOOLS' \
        'MCP-PROFILE-DRIFT: drift test cross-checks the overlay WRITE_TOOLS auth set'

    # The two overlays must keep the byte-alignment comment that the drift test
    # makes enforceable (a removed comment is a sign someone hand-edited the
    # mirror without re-running the gate).
    for ov in br2_external_dcentos/board/zynq/rootfs-overlay/root/web/mcp_server.py \
              br2_external_dcentos/board/amlogic/rootfs-overlay/root/web/mcp_server.py; do
        if [ -f "$ov" ]; then
            require_pattern "$ov" 'byte-aligned with the Rust source of truth' \
                "MCP-PROFILE-DRIFT: overlay keeps the Rust-source-of-truth byte-alignment comment ($ov)"
        else
            pass "MCP-PROFILE-DRIFT: overlay not present at $ov (skipping comment check — path drift)"
        fi
    done
}
mcp_profile_drift_gate_present_check

#
# QA-007: BIP320 rejection-guard ban-gate self-presence assertion.
#
# The actual ban (W6.5-6 bip320_rejection_guard_check, above) greps the four
# BM1362-family share-submit modules for `if ... version_bits_raw != 0 {
# continue }`.  claims this guard is "banned" — this meta-gate makes
# sure the ban gate ITSELF is not silently deleted from this script (a removed
# gate is as bad as a missing one). It asserts the ban function exists and is
# invoked, and that it still keys off the load-bearing identifier + the
# adjacent continue/skip terminator.
#
bip320_bangate_present_check() {
    self="$0"
    [ -f "$self" ] || self="scripts/ci_offline_gates.sh"
    if [ ! -f "$self" ]; then
        fail "QA-007 bip320-bangate: cannot locate this script to self-verify the ban gate"
        return
    fi
    ok=1
    grep -F -- 'bip320_rejection_guard_check()' "$self" >/dev/null 2>&1 || ok=0
    # invoked (a call line that is not the definition)
    grep -E '^bip320_rejection_guard_check[[:space:]]*$' "$self" >/dev/null 2>&1 || ok=0
    # still keys off the banned identifier + a continue/skip terminator
    grep -F -- 'version_bits_raw' "$self" >/dev/null 2>&1 || ok=0
    grep -F -- 'continue' "$self" >/dev/null 2>&1 || ok=0
    if [ "$ok" -eq 1 ]; then
        pass "QA-007 bip320-bangate: the version_bits_raw!=0 rejection-guard ban gate is present + invoked (W6.5-6)"
    else
        fail "QA-007 bip320-bangate: the BIP320 rejection-guard ban gate (bip320_rejection_guard_check) is missing, not invoked, or no longer keys off 'version_bits_raw'/'continue' — restore it (load-bearing per feedback_am2_serial_dispatch_bip320_version_rolling_required.md)"
    fi
}
bip320_bangate_present_check

bip320_bangate_negative_control_check() {
    require_file 'scripts/test_bip320_bangate_negative_control.sh'
    if [ -f 'scripts/test_bip320_bangate_negative_control.sh' ]; then
        if sh 'scripts/test_bip320_bangate_negative_control.sh' >/dev/null 2>&1; then
            pass "QA-007 bip320-bangate: negative-control fixtures trip the real ban gate"
        else
            fail "QA-007 bip320-bangate: negative-control fixtures did NOT trip the real ban gate"
        fi
    fi
}
bip320_bangate_negative_control_check

#
# QA-006: home-safety fan cap. Every devmem PWM write in every S82dcentrald
# overlay (and any other init script that writes the FAN_BASE PWM registers
# 0x10 / 0x14) MUST command PWM <= 30. The form is:
#     devmem $((FAN_BASE + 0x10)) 32 <PWM>
# where `32` is the access width and <PWM> is the value. The PWM-30 home cap
# is a load-bearing safety contract (cut-hash-before-noise; never blast fans
# on a home/space-heater unit). This gate scans every overlay so a future
# edit can't reintroduce a 60-PWM transient spin-up via devmem.
#
fan_pwm_cap_check() {
    bad=''
    scanned=0
    # All init scripts in every board overlay (not just S82dcentrald — catch
    # any script that pokes the FAN_BASE PWM registers directly).
    for f in br2_external_dcentos/board/*/rootfs-overlay/etc/init.d/* \
             br2_external_dcentos/board/*/*/rootfs-overlay/etc/init.d/*; do
        [ -f "$f" ] || continue
        # Only consider files that write a FAN_BASE PWM register via devmem.
        grep -E 'devmem[[:space:]]+\$\(\(FAN_BASE[[:space:]]*\+[[:space:]]*0x1[04]\)\)[[:space:]]+32[[:space:]]+[0-9]+' \
            "$f" >/dev/null 2>&1 || continue
        scanned=$((scanned + 1))
        # Extract every PWM value written to 0x10/0x14 and check it is <= 30.
        # awk on the devmem line: the value is the field after the `32` width.
        offending=$(grep -E 'devmem[[:space:]]+\$\(\(FAN_BASE[[:space:]]*\+[[:space:]]*0x1[04]\)\)[[:space:]]+32[[:space:]]+[0-9]+' "$f" 2>/dev/null \
            | awk '{
                for (i = 1; i <= NF; i++) {
                    if ($i == "32" && (i + 1) <= NF && $(i+1) ~ /^[0-9]+$/) {
                        if ($(i+1) + 0 > 30) { print }
                    }
                }
            }')
        if [ -n "$offending" ]; then
            bad="$bad
$f:
$offending"
        fi
    done
    if [ "$scanned" -eq 0 ]; then
        fail "QA-006 fan-pwm-cap: no overlay init script writes the FAN_BASE PWM registers via devmem (path drift?)"
    elif [ -n "$bad" ]; then
        fail "QA-006 fan-pwm-cap: devmem PWM write > 30 found (regression of the PWM-30 home-safety cap; cut-hash-before-noise):$bad"
    else
        pass "QA-006 fan-pwm-cap: all devmem FAN_BASE PWM writes <= 30 across $scanned overlay init scripts"
    fi
}
fan_pwm_cap_check

#
# QA-002: BM1387 MiscCtrl triple-write source-parse pin. After a temp read,
# `disable_i2c_on_chip0()` MUST write MiscCtrl 0x4020_0180 exactly 3 times
# with 5 ms delays — CMD-register readback is impossible on BM1387, so the
# triple-write is the ONLY reliable way to take chip 0 out of I2C-passthrough
# mode (root cause of the 75 s zero-nonce stall). This is a grep/parse gate
# (per RE-007/QA-002 scope: do NOT edit the Rust). It asserts the loop count
# (0..3) + the 5 ms delay + the constant are all still present.
#
bm1387_misc_ctrl_triple_write_check() {
    f='dcentrald/dcentrald-asic/src/drivers/bm1387.rs'
    if [ ! -f "$f" ]; then
        fail "QA-002 bm1387-triple-write: missing $f (path drift?)"
        return
    fi
    ok=1
    why=''
    # 3-iteration loop guarding the MiscCtrl write.
    if ! grep -E 'for[[:space:]]+_[[:space:]]+in[[:space:]]+0\.\.3' "$f" >/dev/null 2>&1; then
        ok=0; why="$why [missing 'for _ in 0..3' triple-write loop]"
    fi
    # 5 ms inter-write delay.
    if ! grep -E 'from_millis\([[:space:]]*5[[:space:]]*\)' "$f" >/dev/null 2>&1; then
        ok=0; why="$why [missing 5ms inter-write delay]"
    fi
    # The mining-mode-I2C-off MiscCtrl constant (tolerate 0x4020_0180 /
    # 0x40200180 spellings).
    if ! grep -E '0x4020[_]?0180' "$f" >/dev/null 2>&1; then
        ok=0; why="$why [missing MiscCtrl 0x4020_0180 constant]"
    fi
    if [ "$ok" -eq 1 ]; then
        pass "QA-002 bm1387-triple-write: disable_i2c_on_chip0 still writes MiscCtrl 0x4020_0180 3x with 5ms delays"
    else
        fail "QA-002 bm1387-triple-write: BM1387 MiscCtrl triple-write contract regressed in $f:$why (CMD readback is impossible on BM1387; triple-write is the only safety net — root cause of the 75s zero-nonce stall)"
    fi
}
bm1387_misc_ctrl_triple_write_check

#
# RE-007 (safety): the BM1373 (S23) and BM1489 (L7/L9 scrypt) chip drivers are
# UNCONFIRMED, RE-inferred SCAFFOLDS — every register value is a projection
# from a sibling chip, NOT verified on live hardware. They MUST NOT silently
# run on a live unit. The intended runtime contract is a second confirmation
# gate (DCENT_CONFIRM_SCAFFOLD_ON_LIVE_HW) before any scaffold driver touches
# real hardware. This gate enforces the source-side invariant that backstops
# that contract: each scaffold driver's `init_chain` MUST fail closed (return
# an Err), so a scaffold can never bring up a chain without an explicit code
# change AND the operator's confirmation. It also records the required env-gate
# name so a future agent wiring the live path knows what to add.
#
# (Scope note: this is a CI/grep check per the RE-007 task framing. The Rust
# is owned by other groups; this gate documents + enforces the contract, it
# does not edit the drivers.)
#
scaffold_driver_fail_closed_check() {
    # Required runtime confirmation gate name (documented contract for the
    # future live-bring-up path — DO NOT remove without wiring it in Rust).
    required_gate='DCENT_CONFIRM_SCAFFOLD_ON_LIVE_HW'
    for f in dcentrald/dcentrald-asic/src/drivers/bm1373.rs \
             dcentrald/dcentrald-asic/src/drivers/bm1489.rs; do
        if [ ! -f "$f" ]; then
            fail "RE-007 scaffold-fail-closed: missing $f (path drift?)"
            continue
        fi
        # The scaffold must (a) declare itself a SCAFFOLD and (b) its
        # init_chain must return an Err (fail-closed: it cannot bring up live
        # hardware). We assert the SCAFFOLD marker + the fail-closed Err
        # construction both exist in the file.
        if ! grep -F -- 'SCAFFOLD' "$f" >/dev/null 2>&1; then
            fail "RE-007 scaffold-fail-closed: $f no longer self-identifies as a SCAFFOLD (was it promoted without live verification?)"
            continue
        fi
        # Fail-closed evidence: an InvalidParameter Err that names the scaffold
        # refusal. Both drivers construct
        # `AsicError::InvalidParameter("... scaffold ...".into())` in init_chain.
        if grep -E 'InvalidParameter' "$f" >/dev/null 2>&1 \
            && grep -iE 'scaffold|cannot init|verified register' "$f" >/dev/null 2>&1; then
            pass "RE-007 scaffold-fail-closed: $(basename "$f") fails closed (scaffold init refuses to bring up live hardware)"
        else
            fail "RE-007 scaffold-fail-closed: $f scaffold no longer fails closed — its init_chain must return an Err so it cannot run on live hw without an explicit code change + the $required_gate operator confirmation"
        fi
    done
}
scaffold_driver_fail_closed_check

#
# CI-GATE-STALE-BINARY: build_in_docker.sh stages prebuilt Rust binaries and
# does not recompile them. Snapshot-consistency receipts, rather than mutable
# mtimes, detect local binary/source/context drift without claiming that the
# receipt attests a compiler execution. Phase 0 exports one detached private
# generation and Phase 5 must never reopen the mutable host target tree.
#
stale_binary_guard_check() {
    f='scripts/build_in_docker.sh'
    require_file "$f"
    require_pattern "$f" 'dcent_required_prebuilt_binaries' \
        'CI-GATE-STALE-BINARY: build_in_docker enumerates all required staged binaries'
    require_pattern "$f" 'export-snapshot-set' \
        'CI-GATE-STALE-BINARY: Phase 0 captures a private snapshot-consistent binary set'
    require_pattern "$f" 'query-export-snapshot-path' \
        'CI-GATE-STALE-BINARY: host resolves helper-verified canonical export paths'
    require_pattern "$f" '--field path-sha256' \
        'CI-GATE-STALE-BINARY: host atomically queries verified paths and digests for Phase 5'
    require_pattern "$f" 'destination digest mismatch' \
        'CI-GATE-STALE-BINARY: Phase 5 proves destination bytes equal the verified export'
    reject_pattern "$f" 'RECEIPT_HELPER=/build/dcentos/scripts/binary_build_receipt.py' \
        'CI-GATE-STALE-BINARY: Phase 5 does not trust the mutable recopied helper'
    require_pattern "$f" 'export-snapshot-capability-path' \
        'CI-GATE-STALE-BINARY: packaging retains the out-of-stage destruction capability'
    require_pattern "$f" 'destroy-export-snapshot-set' \
        'CI-GATE-STALE-BINARY: cleanup destroys the detached binary set'
    require_pattern "$f" '--capability "$BINARY_EXPORT_CAPABILITY"' \
        'CI-GATE-STALE-BINARY: detached-set cleanup is capability-authorized'
    require_pattern "$f" '-v "${DOCKER_BINARY_EXPORT_STAGE}:/dcent-binaries:ro"' \
        'CI-GATE-STALE-BINARY: Phase 5 receives the private export through a comma-safe read-only mount'
    require_pattern "$f" 'ALL_PREBUILT_BINARIES="dcentrald dcentos-init dcentos-discovery pic-recovery dspic-flash"' \
        'CI-GATE-STALE-BINARY: warm volume purges shipped and historical recovery binary generations'
    require_pattern "$f" 'unsafe persistent binary staging component' \
        'CI-GATE-STALE-BINARY: persistent release path rejects symlink components'
    require_pattern "$f" '"$BUILD_CONTAINER_ID" bash -c' \
        'CI-GATE-STALE-BINARY: post-inspection Docker work runs the immutable image ID'
    reject_pattern "$f" '${POSIX_PROJECT_DIR}/dcentrald/target:/target:ro' \
        'CI-GATE-STALE-BINARY: mutable host Rust target tree is not mounted for packaging'
    require_pattern "$f" 'check-override-policy' \
        'CI-GATE-STALE-BINARY: release-context stale override policy stays enforced'
    require_pattern 'scripts/build-dcentrald.sh' 'emit_build_receipts' \
        'CI-GATE-STALE-BINARY: Rust build emits receipts for staged binaries'
    require_pattern 'scripts/build-dcentrald.sh' '--binary "$receipt_release_dir/dcentrald"' \
        'CI-GATE-STALE-BINARY: dcentrald receipt is emitted'
    require_pattern 'scripts/build-dcentrald.sh' '--binary "$receipt_release_dir/dcentos-init"' \
        'CI-GATE-STALE-BINARY: dcentos-init receipt is emitted'
    require_pattern 'scripts/build-dcentrald.sh' '--binary "$receipt_release_dir/dcentos-discovery"' \
        'CI-GATE-STALE-BINARY: dcentos-discovery receipt is emitted'
    require_pattern 'scripts/build_cv1835_s19jpro.sh' 'exit 78' \
        'CI-GATE-STALE-BINARY: CV1835 standalone build entry point refuses before consuming binaries'
    require_pattern 'br2_external_dcentos/board/cvitek/cv1835-s19jpro/post-build.sh' 'exit 78' \
        'CI-GATE-STALE-BINARY: CV1835 post-build refuses before staging binaries'
    require_pattern "$f" '--source-workspace DCENT_OS_Antminer/dcentrald' \
        'CI-GATE-STALE-BINARY: receipt inventory is rooted in the authenticated snapshot workspace'
    require_pattern 'scripts/binary_build_receipt.py' 'exact-git-object-snapshot' \
        'CI-GATE-STALE-BINARY: v4 receipts distinguish immutable source from live worktree state'
    if python3 - <<'PY'
import runpy

namespace = runpy.run_path("scripts/binary_build_receipt.py", run_name="ci_receipt_constants")
expected = (
    "declared-release-capsule-and-post-build-snapshot-consistency-"
    "not-build-causality-or-reproducibility-proof"
)
raise SystemExit(0 if namespace.get("RECEIPT_CLAIM_V4") == expected else 1)
PY
    then
        pass 'CI-GATE-STALE-BINARY: v4 capsule receipt claims neither build causality nor reproducibility proof'
    else
        fail 'CI-GATE-STALE-BINARY: v4 capsule receipt semantic claim regressed'
    fi
    require_pattern 'scripts/binary_build_receipt.py' 'is forbidden in release provenance/status/image mode' \
        'CI-GATE-STALE-BINARY: receipt bypass is categorically rejected for releases'
    require_pattern 'scripts/binary_build_receipt.py' 'it does not bypass snapshot/export validation' \
        'CI-GATE-STALE-BINARY: deprecated lab signal grants no immutable-boundary bypass'
    require_file 'scripts/test_binary_build_receipt.sh'
    require_file 'scripts/test_binary_export_phase5.sh'
    require_pattern 'br2_external_dcentos/board/zynq/post-build.sh' 'ERROR: dcentos-init not found' \
        'CI-GATE-STALE-BINARY: zynq post-build fails when dcentos-init is absent'
    require_pattern 'br2_external_dcentos/board/zynq/am2-s19jpro/post-build.sh' 'ERROR: dcentos-init not found' \
        'CI-GATE-STALE-BINARY: am2-s19jpro post-build fails when dcentos-init is absent'
    require_pattern 'br2_external_dcentos/board/zynq/post-build.sh' 'dcentos-init.sha256' \
        'CI-GATE-STALE-BINARY: zynq image stamps dcentos-init sha256'
    require_pattern 'br2_external_dcentos/board/zynq/am2-s19jpro/post-build.sh' 'dcentos-init.sha256' \
        'CI-GATE-STALE-BINARY: am2-s19jpro image stamps dcentos-init sha256'
    if sh scripts/test_binary_build_receipt.sh >/dev/null 2>&1; then
        pass "CI-GATE-STALE-BINARY: receipt suite rejects binary, source, context, and release-bypass drift"
    else
        fail "CI-GATE-STALE-BINARY: binary receipt adversarial suite failed"
    fi
    if bash scripts/test_binary_export_phase5.sh >/dev/null 2>&1; then
        pass "CI-GATE-STALE-BINARY: immutable Phase 0 to Phase 5 route and source pin hold"
    else
        fail "CI-GATE-STALE-BINARY: immutable Phase 5 binary export boundary failed"
    fi
}
stale_binary_guard_check

#
# CI-GATE-OTA-PRESERVE (RELIAB-1): the A/B self-update sysupgrade overlays MUST
# copy /data/dcent (dashboard auth.json password, onboarding.json,
# authorized_keys, .ssh-enabled) into the inactive slot. Before RELIAB-1 they
# synced /data/{keys,config,profiles,dcentrald.toml} but NOT /data/dcent, so
# every self-update wiped the operator's password/onboarding/SSH on the new slot
# (wizard re-triggered, SSH disabled). Both gating-platform (zynq) overlays must
# keep the `cp -a /data/dcent/.` preservation.
#
ota_preserve_data_dcent_check() {
    # Pattern includes the /tmp/inactive_data destination so it matches ONLY the
    # operative `cp` command, not the explanatory comment above it (which also
    # contains "cp -a /data/dcent/."). Deleting just the command line must fail
    # this gate.
    require_pattern \
        'br2_external_dcentos/board/zynq/rootfs-overlay/usr/sbin/sysupgrade' \
        'cp -a /data/dcent/. /tmp/inactive_data/dcent' \
        'CI-GATE-OTA-PRESERVE (RELIAB-1): zynq base sysupgrade preserves /data/dcent (password/onboarding/SSH) across A/B self-update'
    require_pattern \
        'br2_external_dcentos/board/zynq/am2-s19jpro/rootfs-overlay/usr/sbin/sysupgrade' \
        'cp -a /data/dcent/. /tmp/inactive_data/dcent' \
        'CI-GATE-OTA-PRESERVE (RELIAB-1): zynq am2-s19jpro sysupgrade preserves /data/dcent (password/onboarding/SSH) across A/B self-update'
}
ota_preserve_data_dcent_check

#
# CI-GATE-CVITEK-S99UPGRADE-SHADOW: the unadmitted CV1835 scaffold historically
# inherited Amlogic NAND/environment behavior. Keep a same-name negative
# authority so direct assembly, a synthetic merge, or warm-build residue cannot
# reopen that brick vector. The supported build hooks refuse before assembly.
#
cvitek_s99upgrade_shadow_check() {
    f='br2_external_dcentos/board/cvitek/cv1835-s19jpro/rootfs-overlay/etc/init.d/S99upgrade'
    require_file "$f"
    if [ ! -f "$f" ]; then
        return
    fi
    require_pattern "$f" 'persistent-update containment shadow' \
        'CI-GATE-CVITEK-S99UPGRADE-SHADOW: cvitek S99upgrade identifies its negative authority' || true
    # No NAND/flash/env write outside the explanatory header comment. Strip the
    # leading "LINENO:" grep prefix, then drop comment-only lines; any remaining
    # write token is a real command and fails the gate.
    write_hits=$(grep -nE 'flash_erase|nandwrite|fw_setenv|nanddump|/dev/mtd|/dev/nand_env' "$f" 2>/dev/null \
        | awk '{
            line=$0
            sub(/^[0-9]+:/, "", line)
            sub(/^[[:space:]]+/, "", line)
            if (line ~ /^#/) next
            print
        }' || true)
    if [ -n "$write_hits" ]; then
        fail "CI-GATE-CVITEK-S99UPGRADE-SHADOW: cvitek S99upgrade shadow contains a real NAND/flash/env write (must stay a no-op — only the header may name these tokens)"
        printf '%s\n' "$write_hits" >&2
    else
        pass "CI-GATE-CVITEK-S99UPGRADE-SHADOW: cvitek S99upgrade is a no-op shadow (no flash_erase/nandwrite/fw_setenv/mtd/nand_env writes outside comments)"
    fi
}
cvitek_s99upgrade_shadow_check

#
# CI-GATE-CVITEK-BRICK-VECTOR-RETIREMENT: held CV1835 U-Boot fingerprints use
# a built-in volatile environment, not a persistent MMC environment. The p2
# content marker is an observed selector, but no crash-safe transition or
# rollback contract has been reconstructed. Every historical build, update,
# recovery, and restore entry point must therefore fail closed.
#
cv1835_brick_vector_retirement_check() {
    tag='CI-GATE-CVITEK-BRICK-VECTOR-RETIREMENT'
    test_script='scripts/test_cv1835_brick_vector_retirement.sh'
    builder='scripts/build_cv1835_s19jpro.sh'
    consumer='scripts/safe_sysupgrade_cv_emmc.sh'
    revert='scripts/revert_to_stock_cv1835.sh'
    post_image='br2_external_dcentos/board/cvitek/cv1835-s19jpro/post-image.sh'
    post_build='br2_external_dcentos/board/cvitek/cv1835-s19jpro/post-build.sh'
    fw_env='br2_external_dcentos/board/cvitek/cv1835-s19jpro/rootfs-overlay/etc/fw_env.config'

    require_file "$test_script"
    require_file "$builder"
    require_file "$consumer"
    require_file "$revert"
    require_file "$post_image"
    require_file "$post_build"
    require_pattern "$builder" 'exit 78' \
        "$tag: standalone build entry point refuses every artifact lane"
    require_pattern "$post_build" 'exit 78' \
        "$tag: direct Buildroot post-build invocation fails closed"
    require_pattern "$post_image" 'exit 78' \
        "$tag: direct Buildroot post-image invocation fails closed"
    require_pattern "$consumer" 'exit 78' \
        "$tag: updater exits with an unconditional unavailable status"
    require_pattern "$revert" 'exit 78' \
        "$tag: stock-revert entrypoint exits with an unconditional unavailable status"
    require_pattern "$consumer" 'BuiltInVolatile/mutation-denied' \
        "$tag: updater records the evidenced environment backend"
    reject_pattern "$consumer" 'DCENT_ALLOW_UNSIGNED_SYSUPGRADE' \
        "$tag: updater has no override path"
    require_pattern 'scripts/build_in_docker.sh' \
        'cv1835-s19jpro has no firmware, sysupgrade, or supported artifact build lane' \
        "$tag: generic build driver refuses the evidence-only target"
    reject_pattern 'scripts/firmware_release_name.sh' \
        'cv1835-s19jpro.*stem=' \
        "$tag: firmware naming cannot mint a release-looking CV1835 alias"
    if [ ! -e "$fw_env" ] && [ ! -L "$fw_env" ]; then
        pass "$tag: guessed CV1835 fw_env.config remains absent"
    else
        fail "$tag: guessed CV1835 fw_env.config was reintroduced"
    fi

    if [ "$STATIC_ONLY" -eq 0 ]; then
        if sh "$test_script" >/dev/null 2>&1; then
            pass "$tag: executable retirement contract passed"
        else
            fail "$tag: executable retirement contract failed"
        fi
    fi
}
cv1835_brick_vector_retirement_check

#
# CI-GATE-AM3BB-SIGNED-SIDECARS (CE-204): the AM3-BB (BeagleBone) SD/package
# builds must emit the canonical Ed25519 sidecars (MANIFEST.json + MANIFEST.sig
# + release_ed25519.pub + SHA256SUMS) exactly like the zynq sysupgrade path — but
# as a deliberately NOT-NAND-installable "sdcard_payload" (nand_install:false), so
# the AM3-BB NAND-disabled honesty is preserved. Producer side: both am3-bb
# post-image.sh source the shared signing helper and run
# stage->write(sdcard_payload)->sign; both post-build.sh stage the pinned pubkey.
# Raw diagnostic/VNish prototype images remain outside release authority. The SD
# payload must NEVER call the sysupgrade manifest writer or self-set the unsigned
# lab escape.
#
am3_bb_signed_sidecars_check() {
    tag='CI-GATE-AM3BB-SIGNED-SIDECARS'
    lib='scripts/lib/sysupgrade_package_common.sh'
    pi_base='br2_external_dcentos/board/beaglebone/am3-bb/post-image.sh'
    pi_s19='br2_external_dcentos/board/beaglebone/am3-bb-s19jpro/post-image.sh'
    pb_base='br2_external_dcentos/board/beaglebone/am3-bb/post-build.sh'
    pb_s19='br2_external_dcentos/board/beaglebone/am3-bb-s19jpro/post-build.sh'
    disk_builder='scripts/build_am3_bb_sd_disk_image.sh'
    vnish_builder='scripts/build_am3_bb_sd_vnish_bootbin_image.sh'

    require_pattern "$lib" 'dcent_write_sdcard_payload_manifest' \
        "$tag LIB: shared helper defines the sdcard_payload manifest writer"
    require_pattern "$lib" '"package_type": "sdcard_payload"' \
        "$tag LIB: sdcard payload manifest is package_type sdcard_payload (not sysupgrade)"
    require_pattern "$lib" '"nand_install": false' \
        "$tag LIB: sdcard payload manifest declares nand_install false"

    for pi in "$pi_base" "$pi_s19"; do
        name=$(basename "$(dirname "$pi")")
        require_pattern "$pi" 'sysupgrade_package_common.sh' \
            "$tag PRODUCER [$name]: post-image sources the shared signing helper"
        require_pattern "$pi" 'dcent_stage_release_key' \
            "$tag PRODUCER [$name]: post-image stages release_ed25519.pub via the shared helper"
        require_pattern "$pi" 'dcent_write_sdcard_payload_manifest' \
            "$tag PRODUCER [$name]: post-image writes the sdcard_payload manifest"
        require_pattern "$pi" 'dcent_sign_sysupgrade_manifest' \
            "$tag PRODUCER [$name]: post-image signs MANIFEST.json (emits MANIFEST.sig)"
        require_pattern "$pi" 'sdcard_payload' \
            "$tag PRODUCER [$name]: post-image ties into the sdcard_payload schema"
        # NEGATIVE: the SD payload must never claim the sysupgrade/NAND schema and
        # the producer must never self-set the unsigned lab escape.
        reject_pattern "$pi" 'dcent_write_sysupgrade_manifest' \
            "$tag NEGATIVE [$name]: SD payload never claims the sysupgrade/NAND-installable schema"
        reject_pattern "$pi" 'DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1' \
            "$tag NEGATIVE [$name]: producer never self-sets the unsigned lab escape"
    done

    for pb in "$pb_base" "$pb_s19"; do
        name=$(basename "$(dirname "$pb")")
        require_pattern "$pb" 'etc/dcentos/release_ed25519.pub' \
            "$tag POST-BUILD [$name]: stages the pinned release_ed25519.pub into the rootfs"
        require_pattern "$pb" 'DCENT_RELEASE_PUBKEY_FILE' \
            "$tag POST-BUILD [$name]: embeds the trusted release pubkey from DCENT_RELEASE_PUBKEY_FILE"
    done

    require_pattern "$disk_builder" 'must never receive a release-authority signature' \
        "$tag BUILDER: bootloop diagnostic remains outside release authority"
    require_pattern "$vnish_builder" 'not eligible for DCENT_OS release signing' \
        "$tag BUILDER: open-gate VNish prototype remains outside release authority"
    require_pattern "$vnish_builder" 'mcopy readback failed; refusing an unverified image' \
        "$tag BUILDER: VNish completeness requires successful image readback"
    reject_pattern "$vnish_builder" 'release_ed25519.pub' \
        "$tag BUILDER: VNish prototype cannot ship a mutable pubkey sidecar"

    # FUNCTIONAL leg (mirrors the pre_flash Ed25519 gate; skipped gracefully
    # without openssl): stage->write->sign a fixture SD payload against a throwaway
    # keypair and assert MANIFEST.sig verifies against the staged pinned pubkey.
    if [ ! -f "$lib" ]; then
        return
    fi
    if command -v openssl >/dev/null 2>&1; then
        tmpdir=$(mktemp -d 2>/dev/null || echo "/tmp/dcentos-ce204-selftest.$$")
        rm -rf "$tmpdir"
        mkdir -p "$tmpdir/payload"
        if openssl genpkey -algorithm Ed25519 -out "$tmpdir/release.key" >/dev/null 2>&1 \
            && openssl pkey -in "$tmpdir/release.key" -pubout -out "$tmpdir/release.pub" >/dev/null 2>&1; then
            if [ "$(run_python_script -c 'import os; print(os.name)')" = nt ]; then
                run_python_script - "$SCRIPT_DIR" "$tmpdir/release.key" <<'PY'
from pathlib import Path
import sys

sys.path.insert(0, sys.argv[1])
import release_set_publication as release_io

release_io.set_windows_file_acl(
    Path(sys.argv[2]), release_io.WINDOWS_PRIVATE_FILE_SDDL
)
PY
            fi
            printf 'payload\n' > "$tmpdir/payload/uramdisk.image.gz"
            p_sha=$(sha256sum "$tmpdir/payload/uramdisk.image.gz" | awk '{print $1}')
            p_size=$(wc -c < "$tmpdir/payload/uramdisk.image.gz" | tr -d ' ')
            payload_block="
    \"uramdisk.image.gz\": {
      \"path\": \"dcentos-am3-bb-sdcard/uramdisk.image.gz\",
      \"size\": ${p_size},
      \"sha256\": \"${p_sha}\"
    }"
            # POSITIVE: a provenance-bound, release-image-hardened SD payload
            # may use release-root authority even though nand_install stays false.
            if (
                . "$lib"
                SUP_DIR="$tmpdir/payload"
                BOARD_NAME="am3-bb"
                BOARD_FAMILY="am3-bb"
                PACKAGE_VERSION="test"
                DCENT_SDCARD_TAR_PREFIX="dcentos-am3-bb-sdcard"
                DCENT_SDCARD_PAYLOAD_BLOCK="$payload_block"
                DCENT_PACKAGE_STATUS="release"
                DCENT_RELEASE_IMAGE=1
                DCENT_REQUIRE_RELEASE_PROVENANCE=1
                DCENT_RELEASE_SIGNING_KEY="$tmpdir/release.key"
                DCENT_RELEASE_PUBKEY_FILE="$tmpdir/release.pub"
                PROJECT_ROOT="$PROJECT_DIR"
                SOURCE_DATE_EPOCH=1700000000
                DCENT_SOURCE_COMMIT_EPOCH=1700000000
                DCENT_SOURCE_COMMIT="0123456789abcdef0123456789abcdef01234567"
                DCENT_SOURCE_TREE_STATE="clean"
                DCENT_BUILD_TARGET="am3-bb"
                DCENT_BUILD_ARCH="armv7"
                DCENT_TOOLCHAIN_ID="ci-fixture"
                dcent_stage_release_key
                dcent_write_sdcard_payload_manifest
                dcent_sign_sysupgrade_manifest
            ) >/dev/null 2>&1 \
                && [ -f "$tmpdir/payload/MANIFEST.sig" ] \
                && [ -f "$tmpdir/payload/release_ed25519.pub" ] \
                && openssl pkeyutl -verify -rawin -pubin \
                    -inkey "$tmpdir/payload/release_ed25519.pub" \
                    -sigfile "$tmpdir/payload/MANIFEST.sig" \
                    -in "$tmpdir/payload/MANIFEST.json" >/dev/null 2>&1; then
                pass "$tag FUNCTIONAL: stage->write->sign emits MANIFEST.sig that verifies against the staged pinned pubkey"
            else
                fail "$tag FUNCTIONAL: signed SD-payload sidecar generation/verification failed"
            fi

            # NEGATIVE A: no key + release status must fail closed.
            if (
                . "$lib"
                SUP_DIR="$tmpdir/neg-a"
                mkdir -p "$SUP_DIR"
                DCENT_PACKAGE_STATUS="release"
                dcent_stage_release_key
            ) >/dev/null 2>&1; then
                fail "$tag FUNCTIONAL NEGATIVE: unsigned release SD payload was NOT refused"
            else
                pass "$tag FUNCTIONAL NEGATIVE: unsigned release-status SD payload fails closed"
            fi

            # NEGATIVE B: no key + non-release status without the explicit lab
            # override must also fail closed.
            if (
                . "$lib"
                SUP_DIR="$tmpdir/neg-b"
                mkdir -p "$SUP_DIR"
                DCENT_PACKAGE_STATUS="management_bringup_sdcard_only"
                dcent_stage_release_key
            ) >/dev/null 2>&1; then
                fail "$tag FUNCTIONAL NEGATIVE: unsigned SD payload accepted without DCENT_ALLOW_UNSIGNED_SYSUPGRADE"
            else
                pass "$tag FUNCTIONAL NEGATIVE: unsigned lab SD payload requires explicit DCENT_ALLOW_UNSIGNED_SYSUPGRADE"
            fi
        else
            fail "$tag FUNCTIONAL: openssl Ed25519 keygen failed in the gate harness"
        fi
        rm -rf "$tmpdir"
    else
        pass "$tag FUNCTIONAL: openssl unavailable — signature legs skipped (pattern legs still enforced)"
    fi
}
am3_bb_signed_sidecars_check

#
# CI-GATE-COMMIT-AUTHORITY (CE-021): pin the already-normalized install/recovery
# COMMIT-AUTHORITY design so it cannot silently drift. Each platform's on-target
# commit mechanism is deliberate and per-platform live-tested; this gate is
# ENFORCEMENT-ONLY and changes NO boot behavior — it just fails closed if a
# future edit blurs the boundaries:
#
#   1. SHADOWS-STAY-NOOP   - the two beaglebone S99upgrade readiness shadows do
#      NO NAND/flash/env write (comment-stripped token scan; any non-comment hit
#      fails).
#   2. MARKER-PARITY       - the boot-success marker literal
#      /tmp/dcentos-upgrade-committed is present in BOTH the zynq S99upgrade (the
#      SOLE health-gated commit authority: bare-delete `fw_setenv upgrade_stage`)
#      and the zynq S99verify (which defers to that marker).
#   3. AMLOGIC-FAIL-CLOSED - amlogic S99upgrade keeps its platform-mandated raw-
#      NAND recovery-flag commit (commit_recovery_flag + the 0x03 readback-
#      mismatch fail path + replay_pending_env_clear WAL replay + the fw_setenv-
#      missing `return 1` in clear_uboot_env) and NEVER grows a zynq-style
#      upgrade_stage commit.
#   4. OTA-08-CONTAINMENT  - a command-position flash_erase/nandwrite appears in
#      NO etc/init.d/S99upgrade EXCEPT the amlogic copy (the documented OTA-08
#      raw-NAND exception); zynq stays fw_setenv-only. Command-position, NOT the
#      bare-token scan: the zynq S99upgrade carries a load-bearing
#      `echo "... DO NOT raw-nandwrite ..."` warning STRING that is documentation,
#      not a write — a bare-token comment-stripped scan would false-positive on it.
#   5. STAGING-NEVER-COMMITS - the 4 zynq sysupgrade overlays STAGE via
#      upgrade_stage=0 and NEVER bare-delete/commit upgrade_stage (that health-
#      gated commit is S99upgrade's sole authority).
#   6. HOST-ONLY-TRANSFORMERS - raw switch_firmware.{sh,py} environment-image
#      transformers remain available for offline forensics, refuse without an
#      explicit acknowledgement, and cannot land in a target overlay/rootfs.
#
commit_authority_normalization_check() {
    tag='CI-GATE-COMMIT-AUTHORITY'
    base='br2_external_dcentos/board'

    zynq_s99up="$base/zynq/rootfs-overlay/etc/init.d/S99upgrade"
    zynq_s99ver="$base/zynq/rootfs-overlay/etc/init.d/S99verify"
    aml_s99up="$base/amlogic/rootfs-overlay/etc/init.d/S99upgrade"
    bb_shadow_a="$base/beaglebone/am3-bb/rootfs-overlay/etc/init.d/S99upgrade"
    bb_shadow_b="$base/beaglebone/am3-bb-s19jpro/rootfs-overlay/etc/init.d/S99upgrade"
    sw_target_sh="$base/zynq/rootfs-overlay/usr/sbin/switch_firmware.sh"
    sw_target_py="$base/zynq/rootfs-overlay/usr/sbin/switch_firmware.py"
    sw_host_sh='scripts/switch_firmware.sh'
    sw_host_py='scripts/switch_firmware.py'
    runtime_prune="$base/common/prune-runtime-research-tools.sh"

    # Comment-stripped token scan (VERBATIM cvitek_s99upgrade_shadow_check idiom):
    # strip grep's "N:" line-number prefix + leading whitespace, drop comment-only
    # lines, print any remaining match. $1=file, $2=ERE.
    _ca_noncomment_hits() {
        grep -nE "$2" "$1" 2>/dev/null \
            | awk '{
                line=$0
                sub(/^[0-9]+:/, "", line)
                sub(/^[[:space:]]+/, "", line)
                if (line ~ /^#/) next
                print
            }' || true
    }

    # 1. SHADOWS-STAY-NOOP — both beaglebone shadows must perform no NAND/flash/env
    #    write. Bare-token comment-stripped scan (the /dev/nand_env in the am3-bb
    #    header comment is stripped by the ^# skip).
    shadow_ok=1
    for f in "$bb_shadow_a" "$bb_shadow_b"; do
        require_file "$f"
        if [ ! -f "$f" ]; then
            shadow_ok=0
            continue
        fi
        hits=$(_ca_noncomment_hits "$f" 'flash_erase|nandwrite|fw_setenv|nanddump|/dev/mtd|/dev/nand_env')
        if [ -n "$hits" ]; then
            fail "$tag SHADOWS-STAY-NOOP: $f is a readiness shadow but contains a real NAND/flash/env write (must stay a no-op)"
            printf '%s\n' "$hits" >&2
            shadow_ok=0
        fi
    done
    if [ "$shadow_ok" -eq 1 ]; then
        pass "$tag SHADOWS-STAY-NOOP: both beaglebone S99upgrade shadows stay no-op (no flash_erase/nandwrite/fw_setenv/mtd/nand_env writes outside comments)"
    fi

    # 2. MARKER-PARITY — the same boot-success marker literal binds the zynq
    #    commit authority (S99upgrade) and the report-only S99verify.
    require_pattern "$zynq_s99up" '/tmp/dcentos-upgrade-committed' \
        "$tag MARKER-PARITY: zynq S99upgrade owns the /tmp/dcentos-upgrade-committed boot-success marker"
    require_pattern "$zynq_s99ver" '/tmp/dcentos-upgrade-committed' \
        "$tag MARKER-PARITY: zynq S99verify defers to the same /tmp/dcentos-upgrade-committed marker"

    # 3. AMLOGIC-FAIL-CLOSED — amlogic commits via the platform-mandated mtd5
    #    recovery-flag (0x02 -> 0x03) mechanism with 0x03 readback fail-closed,
    #    WAL replay, and a fw_setenv-missing return 1; it never touches
    #    upgrade_stage.
    require_file "$aml_s99up"
    require_pattern "$aml_s99up" 'commit_recovery_flag' \
        "$tag AMLOGIC-FAIL-CLOSED: amlogic S99upgrade keeps the mtd5 recovery-flag commit (commit_recovery_flag)"
    require_pattern "$aml_s99up" '!= "0x03"' \
        "$tag AMLOGIC-FAIL-CLOSED: amlogic S99upgrade fails closed on a 0x03 recovery-flag readback mismatch"
    require_pattern "$aml_s99up" 'replay_pending_env_clear' \
        "$tag AMLOGIC-FAIL-CLOSED: amlogic S99upgrade keeps the WAL replay_pending_env_clear path"
    # The fw_setenv-missing `return 1` must live INSIDE clear_uboot_env (scan from
    # its definition to the next top-level function definition).
    if [ -f "$aml_s99up" ] && awk '
        /clear_uboot_env\(\)/ { inb = 1; next }
        inb && /^[A-Za-z_][A-Za-z0-9_]*\(\)/ { inb = 0 }
        inb && /command -v fw_setenv/ { guard = 1 }
        inb && guard && /return 1/ { found = 1 }
        END { exit(found ? 0 : 1) }
    ' "$aml_s99up"; then
        pass "$tag AMLOGIC-FAIL-CLOSED: clear_uboot_env returns 1 when fw_setenv is missing (fails closed, never a silent skip)"
    else
        fail "$tag AMLOGIC-FAIL-CLOSED: clear_uboot_env lost its fw_setenv-missing return-1 fail-closed guard"
    fi
    reject_pattern "$aml_s99up" 'upgrade_stage' \
        "$tag AMLOGIC-FAIL-CLOSED: amlogic S99upgrade never grows a zynq-style upgrade_stage commit"

    # 4. OTA-08-CONTAINMENT — command-position flash_erase/nandwrite only in the
    #    amlogic OTA-08 exception; every other S99upgrade (incl. zynq) stays
    #    fw_setenv-only.
    ota_ok=1
    for f in $(find "$base" -path '*/etc/init.d/S99upgrade' 2>/dev/null | sort); do
        hits=$(_ca_noncomment_hits "$f" '(^|[[:space:]])(flash_erase|nandwrite)([[:space:]]|;|$)')
        if [ "$f" = "$aml_s99up" ]; then
            if [ -z "$hits" ]; then
                fail "$tag OTA-08-CONTAINMENT: amlogic S99upgrade lost its raw-NAND recovery-flag commit (flash_erase/nandwrite) — OTA-08 exception gutted"
                ota_ok=0
            fi
        else
            if [ -n "$hits" ]; then
                fail "$tag OTA-08-CONTAINMENT: $f has a raw-NAND flash_erase/nandwrite command (only the amlogic OTA-08 exception may; every other S99upgrade stays fw_setenv-only)"
                printf '%s\n' "$hits" >&2
                ota_ok=0
            fi
        fi
    done
    if [ "$ota_ok" -eq 1 ]; then
        pass "$tag OTA-08-CONTAINMENT: raw-NAND flash_erase/nandwrite is contained to the amlogic S99upgrade OTA-08 exception; all other S99upgrade copies stay fw_setenv-only"
    fi

    # 5. STAGING-NEVER-COMMITS — the 4 zynq sysupgrade overlays stage
    #    upgrade_stage=0 and must never issue a command-position bare-delete of
    #    upgrade_stage (that health-gated commit is S99upgrade's sole authority).
    stage_ok=1
    for f in \
        "$base/zynq/rootfs-overlay/usr/sbin/sysupgrade" \
        "$base/zynq/am2-s19jpro/rootfs-overlay/usr/sbin/sysupgrade" \
        "$base/zynq/am2-s19pro/rootfs-overlay/usr/sbin/sysupgrade" \
        "$base/zynq/am2-s17pro/rootfs-overlay/usr/sbin/sysupgrade"
    do
        require_file "$f"
        if [ ! -f "$f" ]; then
            stage_ok=0
            continue
        fi
        # Staging form present: the env-script `upgrade_stage=0` line or a direct
        # `fw_setenv upgrade_stage 0` (explicit 0 value).
        if grep -Fq 'upgrade_stage=0' "$f" 2>/dev/null \
            || grep -Eq '^[[:space:]]*fw_setenv[[:space:]]+upgrade_stage[[:space:]]+0([[:space:]]|;|$)' "$f" 2>/dev/null; then
            :
        else
            fail "$tag STAGING-NEVER-COMMITS: $f lost the staging form (fw_setenv upgrade_stage 0 / upgrade_stage=0)"
            stage_ok=0
        fi
        # No command-position bare-delete of upgrade_stage (no value argument).
        bare=$(_ca_noncomment_hits "$f" '^[[:space:]]*fw_setenv[[:space:]]+upgrade_stage[[:space:]]*($|2>|#|;)')
        if [ -n "$bare" ]; then
            fail "$tag STAGING-NEVER-COMMITS: $f bare-deletes/commits upgrade_stage (that health-gated commit belongs to S99upgrade only)"
            printf '%s\n' "$bare" >&2
            stage_ok=0
        fi
    done
    if [ "$stage_ok" -eq 1 ]; then
        pass "$tag STAGING-NEVER-COMMITS: all 4 zynq sysupgrade overlays stage upgrade_stage=0 and never bare-delete/commit upgrade_stage"
    fi

    # 6. HOST-ONLY-TRANSFORMERS — raw environment-image transformers stay
    #    available for offline forensics but never ship in a miner rootfs.
    host_only_ok=1
    for f in "$sw_target_sh" "$sw_target_py"; do
        if [ -e "$f" ] || [ -L "$f" ]; then
            fail "$tag HOST-ONLY-TRANSFORMERS: target overlay contains forbidden raw environment transformer: $f"
            host_only_ok=0
        fi
    done
    for f in "$sw_host_sh" "$sw_host_py"; do
        b=$(basename "$f")
        require_pattern "$f" '--i-understand-this-is-not-fw-setenv' \
            "$tag HOST-ONLY-TRANSFORMERS: host-only $b requires the explicit --i-understand-this-is-not-fw-setenv acknowledgement"
        require_pattern "$f" 'REFUSING' \
            "$tag HOST-ONLY-TRANSFORMERS: host-only $b prints a REFUSING message (deprecated, not fw_setenv)"
    done
    require_pattern "$runtime_prune" 'usr/sbin/switch_firmware.py' \
        "$tag HOST-ONLY-TRANSFORMERS: final rootfs prune removes stale switch_firmware.py"
    require_pattern "$runtime_prune" 'usr/sbin/switch_firmware.sh' \
        "$tag HOST-ONLY-TRANSFORMERS: final rootfs prune removes stale switch_firmware.sh"
    require_pattern "$runtime_prune" 'root usr usr/bin usr/sbin' \
        "$tag HOST-ONLY-TRANSFORMERS: final rootfs prune rejects a symlinked usr/sbin delete path"
    if [ "$host_only_ok" -eq 1 ]; then
        pass "$tag HOST-ONLY-TRANSFORMERS: raw environment transformers are absent from the target overlay"
    fi
}
commit_authority_normalization_check

#
# CI-GATE-S99VERIFY-DRIFT ( beta): the post-flash S99verify proof-matrix
# init script ships as 4 per-overlay copies (zynq, amlogic, cvitek, and
# beaglebone/am3-bb-s19jpro). Unlike the silicon profiles guarded by
# check_profiles_drift.sh there is no migration tool that regenerates them, so a
# hand-edit to one copy can silently drift from the others. This gate ASSERTS
# THE CURRENT KNOWN-GOOD STATE so future *unintended* drift fails closed,
# WITHOUT unifying the scripts or changing any boot behavior:
#
#   1. CORE-INVARIANT MARKERS: all 4 copies carry the shared V1..V14
#      proof-matrix contract (run_verify/emit_check/the V1..V14 banner/schema
#      version=2). Catches an accidentally gutted or truncated copy.
#   2. REPORT-ONLY COMMIT AUTHORITY: all copies are non-mutating proof
#      consumers. They must not contain command-position durable boot-state
#      mutation, while the Zynq copy observes S99upgrade's decision marker.
#   3. NON-CV NON-ZYNQ EQUIVALENCE MODULO ARCH-FALLBACK: Amlogic and
#      BeagleBone are byte-identical after stripping only their two sanctioned
#      `uname -m` BOARD_FAMILY fallback lines. CV1835 is deliberately excluded
#      from this pair because its evidence-backed V14 result must remain red.
#   4. CV1835 BUILTIN-VOLATILE EXCEPTION: CV may differ only in its exact
#      architecture fallback, V14 maturity prose/result, and matching final
#      report-only comment. Its normalized hash must still equal Amlogic.
#
# FINDING (surfaced, NOT forced): CV1835's vendor FIP/BL33 evidence proves a
# built-in volatile U-Boot environment and a p2 payload selector, not an
# implemented persistent update/rollback transaction. Treating CV as another
# positive non-Zynq verifier would make a missing updater look successful.
#
s99verify_drift_check() {
    base='br2_external_dcentos/board'
    zynq="$base/zynq/rootfs-overlay/etc/init.d/S99verify"
    aml="$base/amlogic/rootfs-overlay/etc/init.d/S99verify"
    cvi="$base/cvitek/cv1835-s19jpro/rootfs-overlay/etc/init.d/S99verify"
    bb="$base/beaglebone/am3-bb-s19jpro/rootfs-overlay/etc/init.d/S99verify"

    all="$zynq $aml $cvi $bb"
    # Keep the ordinary non-Zynq parity set separate from CV1835. CV has an
    # evidence-backed fail-red BuiltInVolatile maturity contract, checked below
    # as an exact exception rather than admitted by a broad parity waiver.
    noncv_nonzynq="$aml $bb"

    # Presence — if any copy is missing the rest of the gate cannot reason about
    # drift, so bail after reporting the missing file(s).
    missing=0
    for f in $all; do
        if [ ! -f "$f" ]; then
            fail "CI-GATE-S99VERIFY-DRIFT: missing S99verify copy $f"
            missing=1
        fi
    done
    if [ "$missing" -ne 0 ]; then
        return
    fi

    # 1. Core-invariant markers present in all 4 copies.
    marker_ok=1
    for f in $all; do
        for marker in 'run_verify()' 'emit_check()' 'V1..V14 proof matrix' '"version": 2'; do
            if ! grep -F -- "$marker" "$f" >/dev/null 2>&1; then
                fail "CI-GATE-S99VERIFY-DRIFT: $f missing core-invariant marker '$marker' (proof-matrix contract gutted)"
                marker_ok=0
            fi
        done
    done
    if [ "$marker_ok" -eq 1 ]; then
        pass "CI-GATE-S99VERIFY-DRIFT: all 4 S99verify copies carry the shared V1..V14 proof-matrix markers"
    fi

    # 2. Every verifier is report-only. Ignore prose and reject only actual
    #    command-position durable mutation. Zynq additionally observes the
    #    marker written by its sole commit authority, S99upgrade.
    authority_ok=1
    for f in $all; do
        if ! grep -F -- 'report-only proof consumer' "$f" >/dev/null 2>&1; then
            fail "CI-GATE-S99VERIFY-DRIFT: $f lost the report-only ownership marker"
            authority_ok=0
        fi
        mutation_hits=$(grep -nE '^[[:space:]]*(fw_setenv|nandwrite|flash_erase)([[:space:]]|$)' "$f" 2>/dev/null || true)
        if [ -n "$mutation_hits" ]; then
            fail "CI-GATE-S99VERIFY-DRIFT: $f contains a durable boot-state mutation command"
            printf '%s\n' "$mutation_hits" >&2
            authority_ok=0
        fi
    done
    if ! grep -F -- 'UPGRADE_COMMIT_MARKER' "$zynq" >/dev/null 2>&1; then
        fail "CI-GATE-S99VERIFY-DRIFT: zynq S99verify lost the S99upgrade decision-marker observation"
        authority_ok=0
    fi
    if [ "$authority_ok" -eq 1 ]; then
        pass "CI-GATE-S99VERIFY-DRIFT: all S99verify copies are report-only and Zynq observes the sole commit authority's marker"
    fi

    # 3. The non-CV non-Zynq copies remain byte-identical modulo the sanctioned
    #    `uname -m` arch fallback (the two `armv7l|arm)` / `aarch64|arm64)`
    #    BOARD_FAMILY lines). Normalize by deleting exactly those lines, then
    #    compare sha256. CV is handled by the narrower contract below.
    arch_strip='^[[:space:]]*(armv7l\|arm|aarch64\|arm64)\)'
    norm_ok=1
    for f in $noncv_nonzynq; do
        # Self-test: the normalizer must still target exactly the 2 sanctioned
        # arch-fallback lines. If the arch-case shape ever changes, surface it
        # instead of silently normalizing the wrong thing.
        removed=$(grep -cE "$arch_strip" "$f" 2>/dev/null || true)
        if [ "${removed:-0}" -ne 2 ]; then
            fail "CI-GATE-S99VERIFY-DRIFT: $f has $removed sanctioned uname -m arch-fallback line(s) (expected 2) — the drift normalizer no longer targets the right block"
            norm_ok=0
        fi
    done
    if [ "$norm_ok" -eq 1 ]; then
        ref_hash=''
        for f in $noncv_nonzynq; do
            h=$(grep -vE "$arch_strip" "$f" | sha256sum | awk '{print $1}')
            if [ -z "$ref_hash" ]; then
                ref_hash="$h"
            elif [ "$h" != "$ref_hash" ]; then
                fail "CI-GATE-S99VERIFY-DRIFT: $f drifted from the other non-CV non-zynq S99verify copy OUTSIDE the sanctioned uname -m arch fallback (normalized sha256 $h != $ref_hash)"
                norm_ok=0
            fi
        done
        if [ "$norm_ok" -eq 1 ]; then
            pass "CI-GATE-S99VERIFY-DRIFT: amlogic/beaglebone S99verify are byte-identical apart from the sanctioned per-overlay uname -m arch fallback (normalized sha256 $ref_hash)"
        fi
    fi

    # 4. CV1835 is an explicit evidence-maturity exception, not a general
    #    parity exemption. Vendor FIP/BL33 evidence identifies BuiltInVolatile
    #    env plus a p2 payload selector, but no persistent update/rollback
    #    transaction. Pin the fail-red runtime result and every sanctioned
    #    textual difference, then normalize only those lines back to the common
    #    Amlogic form. Any unrelated CV drift still changes the hash and fails.
    cv_contract_ok=1
    for marker in \
        'aarch64|arm64) BOARD_FAMILY="cv1835-s19jpro" ;; # CV1835 overlay fallback' \
        'persistent update is NOT IMPLEMENTED for the' \
        'BuiltInVolatile/p2-selector fingerprints.' \
        'CV1835 has no implemented persistent updater,' \
        'automatic revert, persistent boot count, or p2 marker-write transaction.' \
        'emit_check V14 false "CV1835 persistent update NOT IMPLEMENTED: BuiltInVolatile environment is mutation-denied and no p2 marker-write transaction exists"'
    do
        count=$(grep -F -c -- "$marker" "$cvi" 2>/dev/null || true)
        if [ "${count:-0}" -ne 1 ]; then
            fail "CI-GATE-S99VERIFY-DRIFT: CV1835 evidence-maturity marker must occur exactly once: '$marker' (found ${count:-0})"
            cv_contract_ok=0
        fi
    done

    cv_route_count=$(grep -F -c -- 'cv1835*)            PLATFORM="cv1835" ;;' "$cvi" 2>/dev/null || true)
    if [ "${cv_route_count:-0}" -ne 2 ]; then
        fail "CI-GATE-S99VERIFY-DRIFT: CV1835 must route to PLATFORM=cv1835 in both detect_platform and run_verify (found ${cv_route_count:-0} exact routes)"
        cv_contract_ok=0
    fi

    # Extract only check_upgrade_stage_cleared's cv1835 branch. It must fail
    # red and return before the generic AM2 env observer. Even read access is
    # forbidden in this branch: BuiltInVolatile is not persistent authority.
    cv_v14_branch=$(awk '
        /^check_upgrade_stage_cleared\(\)[[:space:]]*\{/ { in_function = 1; next }
        in_function && /^[[:space:]]*cv1835\)[[:space:]]*$/ { in_cv = 1 }
        in_cv { print }
        in_cv && /^[[:space:]]*;;[[:space:]]*$/ { exit }
    ' "$cvi")
    cv_branch_count=$(printf '%s\n' "$cv_v14_branch" | grep -cE '^[[:space:]]*cv1835\)[[:space:]]*$' 2>/dev/null || true)
    if [ "${cv_branch_count:-0}" -ne 1 ] || \
       ! printf '%s\n' "$cv_v14_branch" | grep -F 'emit_check V14 false "CV1835 persistent update NOT IMPLEMENTED: BuiltInVolatile environment is mutation-denied and no p2 marker-write transaction exists"' >/dev/null 2>&1 || \
       ! printf '%s\n' "$cv_v14_branch" | grep -Eq '^[[:space:]]*return[[:space:]]*$'; then
        fail "CI-GATE-S99VERIFY-DRIFT: CV1835 V14 must fail red as NOT IMPLEMENTED and return before generic env observation"
        cv_contract_ok=0
    fi
    cv_forbidden=$(printf '%s\n' "$cv_v14_branch" | grep -nE 'fw_printenv|fw_setenv|bootcount|bootlimit|dcent_boot_count|emit_check[[:space:]]+V14[[:space:]]+true' 2>/dev/null || true)
    if [ -n "$cv_forbidden" ]; then
        fail "CI-GATE-S99VERIFY-DRIFT: CV1835 V14 regained env/bootcount access or falsely reports update support"
        printf '%s\n' "$cv_forbidden" >&2
        cv_contract_ok=0
    fi

    aml_common_hash=$(grep -vE "$arch_strip" "$aml" | sha256sum | awk '{print $1}')
    cv_common_hash=$(awk '
        /^[[:space:]]*(armv7l\|arm|aarch64\|arm64)\)/ { next }
        $0 == "#   V14 upgrade-commit-state   : report the platform updater\047s maturity or" {
            print "#   V14 upgrade-commit-state   : report the platform upgrader\047s commit state."
            next
        }
        $0 == "#                                commit state. On CV1835 this remains red:" {
            print "#                                S99verify is a proof consumer and never"
            next
        }
        $0 == "#                                persistent update is NOT IMPLEMENTED for the" {
            print "#                                mutates U-Boot environment state."
            next
        }
        $0 == "#                                BuiltInVolatile/p2-selector fingerprints." { next }
        $0 == "# CONTRACT: failures are logged loudly to syslog. S99verify never mutates" {
            print "# CONTRACT: failures are logged loudly to syslog. S99verify does NOT"
            next
        }
        $0 == "# storage or performs recovery. CV1835 has no implemented persistent updater," {
            print "# auto-revert -- that authority belongs to S99upgrade / U-Boot bootcount."
            next
        }
        $0 == "# automatic revert, persistent boot count, or p2 marker-write transaction." { next }
        /^check_upgrade_stage_cleared\(\)[[:space:]]*\{/ { in_upgrade = 1; print; next }
        in_upgrade && $0 == "        am3-bb)" { print "        am3-bb|cv1835)"; next }
        in_upgrade && $0 == "        cv1835)" { in_cv_v14 = 1; next }
        in_cv_v14 && $0 == "            ;;" { in_cv_v14 = 0; next }
        in_cv_v14 { next }
        in_upgrade && /^# --- main / { in_upgrade = 0 }
        $0 == "        # is report-only and never performs update commit or automatic revert." {
            print "        # does NOT auto-revert -- that authority belongs to S99upgrade /"
            print "        # U-Boot bootcount per QA report Q2 contract."
            next
        }
        { print }
    ' "$cvi" | sha256sum | awk '{print $1}')
    if [ "$cv_common_hash" != "$aml_common_hash" ]; then
        fail "CI-GATE-S99VERIFY-DRIFT: CV1835 drift exceeds the exact BuiltInVolatile V14 exception (normalized sha256 $cv_common_hash != common $aml_common_hash)"
        cv_contract_ok=0
    fi

    if [ "$cv_contract_ok" -eq 1 ]; then
        pass "CI-GATE-S99VERIFY-DRIFT: CV1835 differs only by the exact fail-red BuiltInVolatile/p2-selector V14 contract (normalized sha256 $cv_common_hash)"
    fi
}
s99verify_drift_check

#
# S43logrotate ENOSPC-rotator parity gate (2026-06-29).
#
# S43logrotate is the copytruncate rotator that bounds /tmp/dcentrald.log +
# dashboard.log + mcp.log so a long-running home unit can't ENOSPC-brick its
# /tmp tmpfs. Every target that ships the S82dcentrald daemon (which writes
# those /tmp/*.log files) MUST therefore also ship S43logrotate.
#
# IMPORTANT — assembled-chain model, NOT per-overlay-dir parity:
# DCENT_OS overlays are LAYERED. Each defconfig's BR2_ROOTFS_OVERLAY chains a
# BASE overlay (board/zynq/rootfs-overlay OR board/amlogic/rootfs-overlay) FIRST,
# then a per-SKU VARIANT overlay (e.g. board/zynq/am2-s19jpro/rootfs-overlay).
# Both base overlays ship S43logrotate AND S82dcentrald; the variant overlays
# carry only the files that DIFFER from the base and inherit the rest. So the
# variant overlays (am2-s17pro / am2-s19jpro / am2-s19pro / am3-bb-s19jpro /
# cv1835-s19jpro) legitimately ship S82dcentrald with no sibling S43logrotate —
# the assembled rootfs still gets the rotator from the base overlay. A naive
# per-init.d-dir parity check would false-FAIL on all five and fight the overlay
# design. This gate instead validates the GUARANTEE THAT ACTUALLY MATTERS: for
# every defconfig, if the union of its overlay chain ships S82dcentrald, the same
# union must also ship S43logrotate. It also pins every committed S43logrotate to
# be byte-identical to the canonical zynq copy, so a drifted/edited rotator on any
# overlay is caught. Purely additive; weakens no existing gate.
#
s43logrotate_parity_check() {
    canonical='br2_external_dcentos/board/zynq/rootfs-overlay/etc/init.d/S43logrotate'
    if [ ! -f "$canonical" ]; then
        fail "s43logrotate-parity: canonical rotator missing: $canonical"
        return
    fi
    canon_sha=$(sha256sum "$canonical" | awk '{ print $1 }')

    drift=0

    # 1. Every committed S43logrotate (any overlay) must match the canonical.
    for f in $(find br2_external_dcentos -path '*rootfs-overlay/etc/init.d/S43logrotate' 2>/dev/null); do
        sha=$(sha256sum "$f" | awk '{ print $1 }')
        if [ "$sha" != "$canon_sha" ]; then
            fail "s43logrotate-parity: $f drifts from canonical S43logrotate (sha256 $sha != $canon_sha)"
            drift=1
        fi
    done

    # 2. Every defconfig whose ASSEMBLED overlay chain ships S82dcentrald must
    #    also ship S43logrotate somewhere in the SAME chain.
    for cfg in br2_external_dcentos/configs/*_defconfig; do
        [ -f "$cfg" ] || continue
        # Extract the BR2_ROOTFS_OVERLAY value, strip quotes, and reduce the
        # $(BR2_EXTERNAL_DCENTOS_PATH)/ make-var prefix to repo-relative tokens
        # (so word-splitting is space-safe even though the checkout path has
        # spaces). Comment lines mentioning the var start with '#' and are
        # excluded by the '^BR2_ROOTFS_OVERLAY=' anchor.
        overlays=$(grep -E '^BR2_ROOTFS_OVERLAY=' "$cfg" | head -n1 \
            | sed -e 's/^BR2_ROOTFS_OVERLAY=//' -e 's/^"//' -e 's/"$//' \
                  -e 's#\$(BR2_EXTERNAL_DCENTOS_PATH)/##g')
        [ -n "$overlays" ] || continue
        has_daemon=0
        has_logrotate=0
        for ov in $overlays; do
            initd="br2_external_dcentos/$ov/etc/init.d"
            [ -f "$initd/S82dcentrald" ] && has_daemon=1
            [ -f "$initd/S43logrotate" ] && has_logrotate=1
        done
        if [ "$has_daemon" -eq 1 ] && [ "$has_logrotate" -eq 0 ]; then
            fail "s43logrotate-parity: $(basename "$cfg") ships S82dcentrald but its overlay chain has no S43logrotate (ENOSPC rotator) — /tmp tmpfs can fill over weeks"
            drift=1
        fi
    done

    if [ "$drift" -eq 0 ]; then
        pass "s43logrotate-parity: every S82dcentrald overlay chain ships a byte-identical S43logrotate ENOSPC rotator (canonical sha256 $canon_sha)"
    fi
}
s43logrotate_parity_check

data_growth_bounds_check() {
    require_file 'scripts/test_data_growth_bounds.sh'
    if [ -f 'scripts/test_data_growth_bounds.sh' ]; then
        if sh 'scripts/test_data_growth_bounds.sh' >/dev/null 2>&1; then
            pass "/data growth-bound audit: auth sessions, audit log, audit ring, and log rotation are capped"
        else
            fail "/data growth-bound audit: persistent storage growth controls regressed"
        fi
    fi
}
data_growth_bounds_check

time_posture_bounds_check() {
    require_file 'scripts/test_time_posture_bounds.sh'
    if [ -f 'scripts/test_time_posture_bounds.sh' ]; then
        if sh 'scripts/test_time_posture_bounds.sh' >/dev/null 2>&1; then
            pass "time/NTP posture audit: no-RTC restore, SNTP, auth timers, and schedule offsets are pinned"
        else
            fail "time/NTP posture audit: no-RTC/time contract regressed"
        fi
    fi
}
time_posture_bounds_check

auth_write_frequency_check() {
    require_file 'scripts/test_auth_write_frequency.sh'
    if [ -f 'scripts/test_auth_write_frequency.sh' ]; then
        if sh 'scripts/test_auth_write_frequency.sh' >/dev/null 2>&1; then
            pass "auth write-frequency audit: bearer-token hot path does not persist auth.json"
        else
            fail "auth write-frequency audit: auth.json hot-path write contract regressed"
        fi
    fi
}
auth_write_frequency_check

hardware_identification_confidence_check() {
    require_file 'scripts/test_hardware_identification_confidence.sh'
    if [ -f 'scripts/test_hardware_identification_confidence.sh' ]; then
        if sh 'scripts/test_hardware_identification_confidence.sh' >/dev/null 2>&1; then
            pass "hardware-identification confidence audit: identity confidence DTO, resolver, and JSON surfaces are pinned"
        else
            fail "hardware-identification confidence audit: structured identity confidence regressed"
        fi
    fi
}
hardware_identification_confidence_check

nonstandard_mining_identity_provenance_check() {
    require_file 'scripts/test_nonstandard_mining_identity_provenance.sh'
    if [ -f 'scripts/test_nonstandard_mining_identity_provenance.sh' ]; then
        if sh 'scripts/test_nonstandard_mining_identity_provenance.sh' >/dev/null 2>&1; then
            pass "non-standard mining engines remain non-Measured without retained enumeration receipts"
        else
            fail "non-standard mining identity provenance audit regressed"
        fi
    fi
}
nonstandard_mining_identity_provenance_check

asic_wire_crc5_check() {
    require_file 'scripts/test_asic_wire_crc5.sh'
    if [ -f 'scripts/test_asic_wire_crc5.sh' ]; then
        if sh 'scripts/test_asic_wire_crc5.sh' >/dev/null 2>&1; then
            pass "ASIC wire CRC5: captured command vectors, generated Python copies, and unverified response semantics are pinned"
        else
            fail "ASIC wire CRC5: command checksum or response-integrity boundary regressed"
        fi
    fi
}
asic_wire_crc5_check

offline_soak_harness_check() {
    require_file 'scripts/test_offline_soak_harness.sh'
    require_file 'scripts/offline_soak_harness.sh'
    if [ -f 'scripts/test_offline_soak_harness.sh' ]; then
        if sh 'scripts/test_offline_soak_harness.sh' >/dev/null 2>&1; then
            pass "offline soak harness: accelerated RSS/fd growth gate is pinned"
        else
            fail "offline soak harness: RSS/fd growth gate regressed"
        fi
    fi
}
offline_soak_harness_check

sim_vs_firmware_contract_check() {
    require_file 'scripts/test_sim_vs_firmware_contract.sh'
    if [ -f 'scripts/test_sim_vs_firmware_contract.sh' ]; then
        if sh 'scripts/test_sim_vs_firmware_contract.sh' >/dev/null 2>&1; then
            pass "sim-vs-firmware contract: simulator profiles match promoted firmware wire surfaces"
        else
            fail "sim-vs-firmware contract: simulator profiles drifted from firmware/API wire contracts"
        fi
    fi
}
sim_vs_firmware_contract_check

# hw-acceptance harness: the accepted-share PASS/FAIL parser is the load-bearing
# gate that decides whether a live miner passed acceptance. If its parse silently
# broke, the harness would rubber-stamp a dead unit. So (a) run the hardware-free
# parser unit test here, and (b) drift-guard that skus.conf still lists the
# expanded target SKU set (the harness's single source of truth for Antminer
# acceptance rows).
accept_harness_check() {
    base='scripts/hw-acceptance'
    require_file "$base/lib/accept_parse.sh"
    require_file "$base/dcent-accept.sh"
    require_file "$base/skus.conf"
    require_file "$base/test_accept_parse.sh"
    require_file "$base/test_accept_fuzz.sh"
    require_file "$base/test_skus_conf_valid.sh"
    require_file "$base/test_release_state_route.sh"

    for t in test_accept_parse.sh test_accept_fuzz.sh test_skus_conf_valid.sh test_release_state_route.sh; do
        if [ -f "$base/$t" ]; then
            if sh "$base/$t" >/dev/null 2>&1; then
                pass "hw-acceptance: $t green (accepted-share gate parser pinned)"
            else
                fail "hw-acceptance: $t FAILED (accepted-share PASS/FAIL parser regressed)"
            fi
        fi
    done

    if [ -f "$base/skus.conf" ]; then
        missing=''
        for want in S9 S15 T15 S17 S17Pro S17Plus T17 T17Plus S17e T17e S19 S19Pro S19jPro S19kPro T19 S19XP S21 T21 S21Pro S21XP; do
            if ! grep -qE "^$want\|" "$base/skus.conf"; then
                missing="$missing $want"
            fi
        done
        if [ -n "$missing" ]; then
            fail "hw-acceptance: skus.conf is missing target SKU row(s):$missing (target-set drift)"
        else
            pass "hw-acceptance: skus.conf lists all 20 target SKU rows"
        fi
    fi
}
accept_harness_check

# Operator evidence manifest gate. BENCH-1..8, production key ceremony, public
# HTTPS publication, and product decisions are intentionally operator-run. This
# offline validator keeps the release closeout evidence fail-closed: every gate
# must be marked pass, operator-run, agent-no-live-action, and backed by retained
# files whose SHA-256/size match the manifest.
operator_bench_evidence_check() {
    require_file 'scripts/verify_operator_bench_evidence.py'
    require_file 'scripts/test_operator_bench_evidence.py'
    require_file 'scripts/test_operator_bench_evidence.sh'
    require_pattern 'scripts/verify_operator_bench_evidence.py' 'dcentos-public-beta-external-gates/v3' 'operator evidence manifest schema requires checklist binding'
    require_pattern 'scripts/verify_operator_bench_evidence.py' 'DEFAULT_CHECKLIST = "checklist.json"' 'operator evidence validator names checklist.json'
    require_pattern 'scripts/verify_operator_bench_evidence.py' '--grandfather-legacy-manifest' 'operator evidence validator has explicit legacy grandfather flag'
    require_pattern 'scripts/test_operator_bench_evidence.py' 'test_manifest_requires_hash_bound_checklist' 'operator evidence tests require hash-bound checklist'
    require_pattern 'scripts/test_operator_bench_evidence.py' 'test_legacy_v2_manifest_requires_grandfather_flag' 'operator evidence tests pin explicit legacy grandfather behavior'
    require_pattern 'docs/release/checklist.json' 'dcentos-public-beta-operator-checklist/v1' 'source-controlled operator checklist JSON is present'
    require_pattern 'docs/release/PUBLIC_BETA_OPERATOR_CLOSEOUT_CHECKLIST.md' 'OPERATOR_BENCH_EVIDENCE_OK' 'operator closeout checklist documents verifier success marker'
    require_pattern 'docs/release/reference_repo_staleness_manifest.json' 'advisory_no_network_fetch_in_ci' 'reference-repo staleness manifest is advisory and source-only'
    require_pattern 'docs/PUBLIC_BETA_READINESS_REPORT.md' 'dcentos-public-beta-external-gates/v3' 'public beta report names v3 operator evidence schema'
    if [ -f 'scripts/test_operator_bench_evidence.sh' ]; then
        if sh 'scripts/test_operator_bench_evidence.sh' >/dev/null 2>&1; then
            pass "operator evidence: BENCH/publication/key-ceremony manifest validator self-test green"
        else
            fail "operator evidence: manifest validator self-test FAILED"
        fi
    fi
}
operator_bench_evidence_check

# Public beta artifact marker gate. The release definition of done requires the
# shipped sysupgrade images to carry /etc/dcentos/release-image and
# metrics_require_auth = true. The publication verifier must inspect the
# embedded SquashFS root payloads, not just the outer tar manifest.
publication_artifact_marker_check() {
    f='scripts/verify_beta_xil_publication_packet.sh'
    require_file "$f"
    [ -f "$f" ] || return
    require_pattern "$f" 'unsquashfs' \
        'publication packet verifier inspects embedded SquashFS root payloads'
    require_pattern "$f" 'verify_rootfs_release_markers' \
        'publication packet verifier has a rootfs marker check'
    require_pattern "$f" 'etc/dcentos/release-image' \
        'publication packet verifier requires /etc/dcentos/release-image in rootfs'
    require_pattern "$f" 'metrics_require_auth' \
        'publication packet verifier requires metrics_require_auth=true in rootfs'
}
publication_artifact_marker_check

# No-secret-logs regression pin. post_config once logged the ENTIRE config request
# body at INFO (`"Config update request: {:?}", body`) — which carries a pool
# `password` and a `stratum+tcp://user:pass@host` URL, landing a plaintext
# credential on disk (/tmp/dcentrald.log + the persistent ring + support bundles).
# Fixed in 5c871dd6 to log only top-level key names. Ban the exact pre-fix leak
# marker so the class cannot silently return. (2026-07-03 secrets-in-logs sweep:
# this was the sole daemon log leak; the pool-credential path masks via mask_wallet.)
secret_log_ban_check() {
    f='dcentrald/dcentrald-api/src/rest.rs'
    if [ ! -f "$f" ]; then
        fail "no-secret-logs: missing $f"
        return
    fi
    if grep -F -- 'Config update request: {:?}' "$f" >/dev/null 2>&1; then
        fail "no-secret-logs: post_config raw-body INFO leak reintroduced ($f) — logs pool password on disk"
    else
        pass "no-secret-logs: post_config does not log the raw config body (pool-password-on-disk leak stays fixed)"
    fi
}
secret_log_ban_check

# Broaden the no-secret-logs gate from the single known post_config leak to the
# whole CLASS: any log macro that DEBUG-formats ({:?}) a credential-bearing
# variable (raw_body / *config / creds / password / secret) risks writing a pool
# password (or other credential) to the on-disk log. Scan the API + stratum
# sources; the codebase is clean today, so any hit is a NEW single-line
# reintroduction (the exact-string check above still covers the historical leak).
secret_debug_log_class_check() {
    # `|| true`: the whole grep pipeline exits non-zero when it finds nothing (the
    # clean case), which under this script's `set -e` would abort the gate on the
    # assignment. Force success so the empty result is handled below.
    _hits=$(grep -rnE '(info|warn|error|debug|trace)!\(' \
        dcentrald/dcentrald-api/src dcentrald/dcentrald-stratum/src 2>/dev/null \
        | grep -E '\{:\?\}' \
        | grep -iE '\b(raw_body|new_config|pool_config|full_config|creds|credential|password|passwd|secret)\b' \
        | grep -viE '//|redact|mask|password_set|has_password|no_password|_present|_configured|onboarding' \
        || true)
    if [ -n "$_hits" ]; then
        fail "no-secret-logs: a log statement debug-formats a credential-bearing variable (may leak a credential to the on-disk log): $(printf '%s' "$_hits" | head -1)"
    else
        pass "no-secret-logs: no log statement debug-formats a credential-bearing variable (API + stratum)"
    fi
}
secret_debug_log_class_check

# Key-ceremony tooling self-test. The production Ed25519 release key ceremony
# (generate_release_keypair.sh mints the key; verify_release_keypair.sh proves it
# round-trips + emits the exact firmware-baked public-key hex) is an air-gapped
# operator step. This proves the SCRIPTS themselves work end-to-end with THROWAWAY
# keys — a matched pair PASSES and emits a 64-char hex, a mismatched pair FAILS —
# so a broken ceremony script can't silently ship a firmware that fails to verify
# its own OTA (a bricked update path). The self-test skips cleanly if openssl/od
# are unavailable, so this stays green on a minimal host.
key_ceremony_selftest_check() {
    require_file 'scripts/generate_release_keypair.sh'
    require_file 'scripts/verify_release_keypair.sh'
    require_file 'scripts/test_verify_release_keypair.sh'
    if [ -f 'scripts/test_verify_release_keypair.sh' ]; then
        if bash 'scripts/test_verify_release_keypair.sh' >/dev/null 2>&1; then
            pass "key-ceremony: generate+verify tooling self-test green (or cleanly skipped)"
        else
            fail "key-ceremony: verify_release_keypair.sh self-test FAILED (ceremony tooling broken)"
        fi
    fi
}
key_ceremony_selftest_check

# Public SKU support matrix completeness. The public-facing SUPPORTED_HARDWARE.md
# must describe every SKU DCENT_OS targets — a drift means a user's miner is
# silently absent from the support statement (they'd have no honest read on their
# hardware's real state). Gate on each SKU's board_target token (unique per SKU)
# from the skus.conf source of truth, so the doc can't fall out of sync with the
# actual target set.
supported_hardware_doc_check() {
    doc='docs/SUPPORTED_HARDWARE.md'
    conf='scripts/hw-acceptance/skus.conf'
    require_file "$doc"
    require_file "$conf"
    [ -f "$doc" ] && [ -f "$conf" ] || return
    bts=$(grep -vE '^[[:space:]]*#|^[[:space:]]*$' "$conf" | cut -d'|' -f2)
    missing=''
    for bt in $bts; do
        [ -n "$bt" ] || continue
        grep -Fq "$bt" "$doc" || missing="$missing $bt"
    done
    if [ -n "$missing" ]; then
        fail "SUPPORTED_HARDWARE.md is missing SKU board_target(s):$missing (public support-doc drift)"
    else
        pass "SUPPORTED_HARDWARE.md covers every SKU in skus.conf (public support matrix in sync)"
    fi
    require_pattern "$doc" 'Failed-boot auto-revert is not guaranteed' 'SUPPORTED_HARDWARE.md states the beta rollback boundary'
    require_pattern "$doc" 'known-good SD recovery media or a verified full-NAND restore path' 'SUPPORTED_HARDWARE.md names required beta recovery equipment'
    require_pattern 'docs/PUBLIC_BETA_READINESS_REPORT.md' 'Failed-boot auto-revert is not guaranteed' 'public beta report states the rollback boundary'
    require_pattern 'docs/PUBLIC_BETA_READINESS_REPORT.md' 'serial console access plus known-good SD recovery media or a verified full-NAND restore path' 'public beta report names recovery equipment'
}
supported_hardware_doc_check

# A/B rollback health-verdict gate. daemon_real_health_verdict in the zynq
# S99upgrade is the commit-vs-revert decision for a fresh firmware slot. Its
# fail-safe classification is load-bearing: absence of proof (no wget / empty /
# unparseable body) must SOFT-PASS as "unknown" so a good unit is never needlessly
# reverted, while a reachable-but-zero-uptime daemon must be "unhealthy" so a broken
# slot is blocked from committing (the W8 "defeated by S99" bug class).
# The first functional test sources the REAL function and drives it with a
# mock wget; the second runs the REAL start path with host-side shims and
# asserts a zero-uptime daemon writes the blocked marker without calling
# fw_setenv. The third runs the REAL start path with SSH deliberately down
# and pins that only the documented first-boot/release-image policy states
# soft-pass; unmarked SSH-down still blocks the commit.
s99_health_verdict_check() {
    require_file 'scripts/test_s99_health_verdict.sh'
    if [ -f 'scripts/test_s99_health_verdict.sh' ]; then
        if sh 'scripts/test_s99_health_verdict.sh' >/dev/null 2>&1; then
            pass "A/B rollback: S99upgrade health-verdict fail-safe classification green"
        else
            fail "A/B rollback: S99upgrade daemon_real_health_verdict classification regressed"
        fi
    fi
    require_file 'scripts/test_s99upgrade_failed_health_no_commit.sh'
    if [ -f 'scripts/test_s99upgrade_failed_health_no_commit.sh' ]; then
        if sh 'scripts/test_s99upgrade_failed_health_no_commit.sh' >/dev/null 2>&1; then
            pass "A/B rollback: failed-health S99upgrade start path leaves slot uncommitted"
        else
            fail "A/B rollback: failed-health S99upgrade start path committed or regressed"
        fi
    fi
    require_file 'scripts/test_s99upgrade_commit_refusals.sh'
    if [ -f 'scripts/test_s99upgrade_commit_refusals.sh' ]; then
        if sh 'scripts/test_s99upgrade_commit_refusals.sh' >/dev/null 2>&1; then
            pass "A/B rollback: S99upgrade commit-refusal paths leave the slot blocked"
        else
            fail "A/B rollback: S99upgrade commit-refusal path committed or regressed"
        fi
    fi
    require_file 'scripts/test_s99upgrade_ssh_soft_pass.sh'
    if [ -f 'scripts/test_s99upgrade_ssh_soft_pass.sh' ]; then
        if sh 'scripts/test_s99upgrade_ssh_soft_pass.sh' >/dev/null 2>&1; then
            pass "A/B rollback: S99upgrade SSH soft-pass policy is pinned"
        else
            fail "A/B rollback: S99upgrade SSH soft-pass policy regressed"
        fi
    fi
}
s99_health_verdict_check

# S99verify platform-detection gate. detect_platform() classifies the running
# board (board_family stamp -> board_target fallback -> uname heuristic) and that
# classification routes the per-platform upgrade/rollback health checks — a
# mis-classified platform runs the wrong V-checks. Pins the classification for
# every target platform, incl. the canonical am2-s19jpro-zynq board_target in the
# fallback path (the routing-key class fixed across the resolver/harness/init).
s99_detect_platform_check() {
    require_file 'scripts/test_s99_detect_platform.sh'
    if [ -f 'scripts/test_s99_detect_platform.sh' ]; then
        if sh 'scripts/test_s99_detect_platform.sh' >/dev/null 2>&1; then
            pass "OTA: S99verify detect_platform classifies every target platform"
        else
            fail "OTA: S99verify detect_platform platform classification regressed"
        fi
    fi
}
s99_detect_platform_check

# The S9 stock-restore selector assumptions were invalidated by local U-Boot
# and live evidence. Keep both current and legacy CLI entry points fail-closed,
# prevent stale destructive recipes from surviving in source, and ensure the
# Buildroot legacy pathname is only an alias of the canonical containment file.
s9_restore_containment_check() {
    require_file 'scripts/test_s9_restore_containment.sh'
    if [ -f 'scripts/test_s9_restore_containment.sh' ]; then
        if sh 'scripts/test_s9_restore_containment.sh' >/dev/null 2>&1; then
            pass "S9 stock restore: invalidated selector path is contained"
        else
            fail "S9 stock restore: containment boundary regressed"
        fi
    fi
}
s9_restore_containment_check

# The common Amlogic overlay must retain the historical /uninstall.sh pathname
# without inheriting the captured LuxOS S19k environment-corruption/rootfs-wipe
# authority. Exercise the compatibility stub so a future packaging edit cannot
# silently re-enable raw /dev/nand_env mutation or a hard reboot.
amlogic_uninstall_containment_check() {
    require_file 'scripts/test_amlogic_uninstall_containment.sh'
    if [ -f 'scripts/test_amlogic_uninstall_containment.sh' ]; then
        if sh 'scripts/test_amlogic_uninstall_containment.sh' >/dev/null 2>&1; then
            pass "Amlogic uninstall: unproven LuxOS restore procedure is contained"
        else
            fail "Amlogic uninstall: zero-mutation compatibility boundary regressed"
        fi
    fi
}
amlogic_uninstall_containment_check

# The historical SD-to-NAND installer and Zynq AM2 S19 stock-revert path remain
# addressable for compatibility, but neither has an admitted shared-engine
# transaction. Exercise both refusal interfaces so legacy CLI arguments,
# hostile environment overrides, or target packaging cannot restore authority.
legacy_boot_env_writer_containment_check() {
    require_file 'scripts/test_legacy_boot_env_writer_containment.sh'
    if [ -f 'scripts/test_legacy_boot_env_writer_containment.sh' ]; then
        if sh 'scripts/test_legacy_boot_env_writer_containment.sh' >/dev/null 2>&1; then
            pass "legacy boot-environment writers: unproven install/restore paths are contained"
        else
            fail "legacy boot-environment writers: zero-mutation compatibility boundary regressed"
        fi
    fi
}
legacy_boot_env_writer_containment_check

# Sysupgrade / stock-revert packaging + NAND-safety static gate. This test bundles
# the load-bearing recovery-safety invariants — fw_setenv-only env flips, extract-
# before-erase ordering, extracted-size caps, hard-link rejection, S17 fail-closed
# on an unknown bootslot, and the Amlogic-never-flash_erase brick guard — but was
# ORPHANED (invoked by no CI workflow or gate). Run it here so those revert/recovery
# safety checks actually gate the release instead of silently rotting.
sysupgrade_packaging_static_check() {
    require_file 'scripts/test_sysupgrade_packaging_static.sh'
    if [ -f 'scripts/test_sysupgrade_packaging_static.sh' ]; then
        if sh 'scripts/test_sysupgrade_packaging_static.sh' >/dev/null 2>&1; then
            pass "sysupgrade/revert packaging + NAND-safety static checks green"
        else
            fail "sysupgrade/revert packaging + NAND-safety static checks regressed"
        fi
    fi
}
sysupgrade_packaging_static_check

# The signed sysupgrade envelope must be byte-identical when the same staged
# payloads are packaged in unrelated directories with different host mtimes,
# creation order and modes. The test also proves invalid/missing/dirty source
# provenance fails closed before signing.
release_envelope_reproducibility_check() {
    require_file 'scripts/test_release_envelope_reproducibility.sh'
    if [ -f 'scripts/test_release_envelope_reproducibility.sh' ]; then
        if bash 'scripts/test_release_envelope_reproducibility.sh' >/dev/null 2>&1; then
            pass "release envelope is reproducible and provenance rejects invalid inputs"
        else
            fail "release envelope reproducibility/provenance contract regressed"
        fi
    fi
}
release_envelope_reproducibility_check

release_publication_check() {
    require_file 'scripts/release_publication.py'
    require_file 'scripts/test_release_publication.py'
    require_file 'scripts/test_release_publication.sh'
    if sh 'scripts/test_release_publication.sh' >/dev/null 2>&1; then
        pass "each flat release compatibility file publishes atomically without replacement"
    else
        fail "release publication path/identity boundary regressed"
    fi
}
release_publication_check

# A release invocation is an identity/capability boundary, not cleanup authority
# over arbitrary Docker or output state. Source materialization and authoritative
# publication have separate exact-tree capabilities so each can fail closed.
release_capsule_primitives_check() {
    require_file 'scripts/release_invocation.py'
    require_file 'scripts/release_signing_authority.py'
    require_file 'scripts/test_release_signing_authority.py'
    require_file 'scripts/test_release_signing_authority.sh'
    require_file 'scripts/release_capsule_lineage.py'
    require_file 'scripts/test_release_invocation.py'
    require_file 'scripts/test_release_invocation.sh'
    require_file 'scripts/release_result_stage.py'
    require_file 'scripts/test_release_result_stage.py'
    require_file 'scripts/test_release_result_stage.sh'
    require_file 'scripts/release_docker_resources.py'
    require_file 'scripts/test_release_docker_resources.py'
    require_file 'scripts/test_release_docker_resources.sh'
    require_file 'scripts/build_s9_release_capsule.sh'
    require_file 'scripts/test_cargo_capsule_driver.sh'
    require_file 'scripts/test_s9_release_capsule_driver.sh'
    require_file 'scripts/release_capsule_target_policy.py'
    require_file 'scripts/test_release_capsule_target_policy.py'
    require_file 'scripts/test_release_capsule_target_policy.sh'
    require_file 'scripts/portable_release_evidence.py'
    require_file 'scripts/test_portable_release_evidence.py'
    require_file 'scripts/test_portable_release_evidence.sh'
    require_file 'scripts/source_snapshot.py'
    require_file 'scripts/test_source_snapshot.py'
    require_file 'scripts/test_source_snapshot.sh'
    require_file 'scripts/release_set_publication.py'
    require_file 'scripts/test_release_set_publication.py'
    require_file 'scripts/test_release_set_publication.sh'

    if sh 'scripts/test_release_invocation.sh' >/dev/null 2>&1; then
        pass "release invocation identities are unique, capability-owned, and explicitly GC-gated"
    else
        fail "release invocation identity/capability boundary regressed"
    fi
    if sh 'scripts/test_release_signing_authority.sh' >/dev/null 2>&1; then
        pass "release signing keys are stable, private, invocation-bound capabilities"
    else
        fail "release signing-authority snapshot/integrity boundary regressed"
    fi
    # The wrapper declares Bash and uses `set -o pipefail`; invoking it through
    # Ubuntu's `/bin/sh` (dash) exits before any result-stage test runs.
    if bash 'scripts/test_release_result_stage.sh' >/dev/null 2>&1; then
        pass "Cargo result handoff is invocation-bound, exact, and outside the live source tree"
    else
        fail "release result-stage isolation/integrity boundary regressed"
    fi
    if sh 'scripts/test_release_docker_resources.sh' >/dev/null 2>&1; then
        pass "Docker volume operations require exact invocation labels and cleanup authority"
    else
        fail "release Docker resource authority boundary regressed"
    fi
    if bash 'scripts/test_cargo_capsule_driver.sh' >/dev/null 2>&1; then
        pass "Cargo capsule consumes read-only snapshot source and isolated invocation results"
    else
        fail "Cargo capsule driver isolation/cleanup boundary regressed"
    fi
    if bash 'scripts/test_s9_release_capsule_driver.sh' >/dev/null 2>&1; then
        pass "S9 capsule preserves immutable source, private invocation state, cleanup, and no-replace publication"
    else
        fail "S9 release-capsule orchestration boundary regressed"
    fi
    if bash 'scripts/test_release_capsule_target_policy.sh' >/dev/null 2>&1; then
        pass "release-capsule targets use one exact fail-closed identity policy"
    else
        fail "release-capsule target admission policy regressed"
    fi
    if sh 'scripts/test_source_snapshot.sh' >/dev/null 2>&1; then
        pass "source snapshots materialize exact Git-object bytes outside the live worktree"
    else
        fail "Git-object source snapshot integrity/cleanup boundary regressed"
    fi
    if bash 'scripts/test_portable_release_evidence.sh' >/dev/null 2>&1; then
        pass "published release sets remain target-bound and independently auditable after private-stage cleanup"
    else
        fail "portable signed release-evidence target boundary regressed"
    fi
    if sh 'scripts/test_release_set_publication.sh' >/dev/null 2>&1; then
        pass "authoritative release directories publish as one exact no-replace set"
    else
        fail "authoritative release-set sealing/publication boundary regressed"
    fi
}
release_capsule_primitives_check

# Partial source closure is a separate, deliberately narrower claim than
# envelope reproducibility. Bind immutable build definitions and actual output
# member digests while keeping unresolved Buildroot/container inputs explicit.
source_closure_check() {
    require_file 'scripts/source_closure.py'
    require_file 'scripts/test_source_closure.sh'
    require_file 'scripts/sign_release_receipt.sh'
    require_file 'scripts/sign_release_receipt.py'
    require_file 'scripts/test_sign_release_receipt.sh'
    if sh scripts/test_sign_release_receipt.sh >/dev/null 2>&1; then
        pass "release receipt signing pins exact inputs and durably publishes without replacement"
    else
        fail "release receipt signing lifecycle regressed"
    fi
    require_file 'scripts/test_build_input_preflight.sh'
    require_file 'scripts/build_input_snapshot.py'
    require_file 'scripts/test_build_input_snapshot.py'
    require_file 'scripts/test_build_input_snapshot.sh'
    require_file 'scripts/buildroot_local_source_digest.py'
    require_file 'scripts/test_buildroot_local_source_digest.py'
    if [ -f 'scripts/test_source_closure.sh' ]; then
        if bash 'scripts/test_source_closure.sh' >/dev/null 2>&1; then
            pass "partial source closure is deterministic and rejects missing/mutable inputs"
        else
            fail "partial source-closure generation/verification regressed"
        fi
    fi
    if [ -f 'scripts/test_build_input_preflight.sh' ]; then
        if bash 'scripts/test_build_input_preflight.sh' >/dev/null 2>&1; then
            pass "target-scoped out-of-band inputs fail closed before Cargo/Docker consumers"
        else
            fail "target-scoped build-input preflight regressed"
        fi
    fi
    if [ -f 'scripts/test_build_input_snapshot.sh' ]; then
        if sh 'scripts/test_build_input_snapshot.sh' >/dev/null 2>&1; then
            pass "manifest-pinned external bytes use exact-tree snapshots and consumer-side digest checks"
        else
            fail "build-input snapshot integrity/cleanup/consumption gate regressed"
        fi
    fi
    if run_python_script 'scripts/test_buildroot_local_source_digest.py' -q >/dev/null 2>&1; then
        pass "Buildroot warm-tree invalidation binds every staged BR2_EXTERNAL source byte, path, and mode"
    else
        fail "Buildroot local-source digest or warm-tree invalidation tests regressed"
    fi
    require_pattern 'scripts/source_closure.py' \
        'org.dcentral.dcentos.source-closure.v4' \
        'source closure v4 binds one authenticated invocation and Git-object snapshot'
    require_pattern 'scripts/source_closure.py' \
        'org.dcentral.dcentos.source-closure.v3' \
        'historical source closure v3 remains verification-only during migration'
    require_pattern 'scripts/source_closure.py' \
        'legacy source-closure v1 receipts lack required out-of-band input binding' \
        'source closure rejects unbound legacy v1 receipts by default'
    require_pattern 'scripts/source_closure.py' \
        'legacy source-closure v2 receipts lack retained prebuilt Rust input binding' \
        'source closure rejects v2 receipts that omit retained prebuilt Rust inputs'
    require_pattern 'scripts/source_closure.py' \
        'retained-packaging-input-snapshots-not-build-execution-attestation' \
        'retained prebuilt Rust evidence keeps its build-execution boundary explicit'
    require_pattern 'scripts/build_inputs.manifest' \
        'knowledge-base/extractions/s9/s9_devicetree.dtb' \
        'S9 FIT device tree is pinned as a consumed out-of-band build input'
    require_pattern 'scripts/build-dcentrald.sh' \
        '--target cargo-workspace' \
        'Cargo workspace snapshots its ignored embedded input before compilation'
    require_pattern 'scripts/build_in_docker.sh' \
        'build_input_snapshot.py" create' \
        'firmware packaging snapshots target inputs before Docker consumption'
    require_pattern 'scripts/build_in_docker.sh' \
        'buildroot_local_source_digest.py br2_external_dcentos' \
        'Buildroot warm target stamp digests the complete staged BR2_EXTERNAL tree'
    require_pattern 'scripts/build_in_docker.sh' \
        'br2_external_sha256=$BR2_EXTERNAL_SOURCE_SHA256' \
        'Buildroot warm target stamp invalidates cached local-package sources'
    require_pattern 'scripts/build_in_docker.sh' \
        'build_driver_sha256=$BUILD_DRIVER_SHA256' \
        'Buildroot warm target stamp invalidates build-driver semantic changes'
    require_pattern 'scripts/build_in_docker.sh' \
        'source_digest_tool_sha256=$SOURCE_DIGEST_TOOL_SHA256' \
        'Buildroot warm target stamp binds the digest implementation itself'
    reject_pattern 'scripts/build_in_docker.sh' \
        ':/kb:ro' \
        'supported packaging lanes cannot bypass snapshots through a live knowledge-base mount'
    reject_pattern 'scripts/build_in_docker.sh' \
        '/kb/extractions/' \
        'packaging contains no dormant live-extraction fallback path'
    if [ ! -e 'dcentrald/pic-recovery/build.rs' ]; then
        pass 'Cargo diagnostics contain no recovery-artifact build script'
    else
        fail 'Cargo diagnostics unexpectedly contain dcentrald/pic-recovery/build.rs'
    fi
    reject_pattern 'dcentrald/pic-recovery/src/main.rs' \
        'include_bytes!' \
        'controller diagnostics cannot embed a proprietary recovery artifact'
    reject_pattern 'dcentrald/pic-recovery/src/main.rs' \
        'include_str!' \
        'controller diagnostics cannot embed a textual recovery artifact'
    reject_pattern 'dcentrald/pic-recovery/src/dspic_flash_main.rs' \
        'include_bytes!' \
        'dsPIC status command cannot embed a proprietary recovery artifact'
    reject_pattern 'dcentrald/pic-recovery/src/dspic_flash_main.rs' \
        'include_str!' \
        'dsPIC status command cannot embed a textual recovery artifact'
    reject_pattern 'scripts/build-dcentrald.sh' \
        'DCENT_STOCK_FPGA' \
        'normal Cargo builds cannot carry retired stock-FPGA environment authority'
    reject_pattern 'scripts/build-dcentrald.sh' \
        'STAGED_STOCK_FPGA' \
        'normal Cargo builds cannot stage the retired stock-FPGA recovery input'
    reject_pattern 'scripts/build-dcentrald.sh' \
        '/dcent-inputs/stock_fpga' \
        'normal Cargo builds cannot mount the retired stock-FPGA recovery input'
    reject_pattern 'scripts/build_inputs.manifest' \
        'pic-recovery/firmware/stock_fpga_s9.bin' \
        'release input policy cannot retain the unconsumed stock FPGA blob'
    reject_pattern 'scripts/build_inputs.manifest' \
        'pic-recovery/firmware/stock_fpga_extracted.bin' \
        'release input policy cannot retain the unconsumed comparison blob'
    require_pattern 'scripts/source_closure.py' \
        'COMMON_CARGO_BUILD_INPUTS = ()' \
        'Cargo external-input evidence truthfully selects an empty file set'
    require_pattern 'scripts/build_in_docker.sh' \
        '--token "$BUILD_INPUT_DESTROY_TOKEN"' \
        'firmware packaging cleanup requires the out-of-band snapshot destruction capability'
    reject_pattern 'scripts/build-dcentrald.sh' \
        '$KNOWLEDGE_BASE_DIR":/knowledge-base:ro' \
        'cross Cargo cannot inspect the full live knowledge-base tree'
    require_pattern 'scripts/build-dcentrald.sh' \
        ':/knowledge-base/firmware-archive/stock-bitmain-manifest.json:ro' \
        'cross Cargo receives only the exact tracked stock manifest input'
    require_pattern 'scripts/build_in_docker.sh' \
        'git -C buildroot status --porcelain --untracked-files=normal' \
        'source closure rejects modified, staged, and untracked warm-volume Buildroot source'
    require_pattern 'scripts/build_in_docker.sh' \
        'source_closure.py" generate' \
        'all Docker image producers emit a source-closure receipt'
    require_pattern '../../.github/workflows/dcentos-image-smoke.yml' \
        'portable_release_evidence.py verify' \
        'S9 image smoke reauthenticates closure, receipts, inputs, and artifacts after capsule cleanup'
    require_pattern '../../.github/workflows/dcentos-image-smoke.yml' \
        'portable-release-evidence.json.sig' \
        'S9 image smoke requires the signed portable exact-set index'
    require_pattern '../../.github/workflows/dcentos-image-smoke.yml' \
        '.dcent-release-set.json' \
        'S9 image smoke requires the final sealed release-set descriptor'
    require_pattern '../../.github/workflows/dcentos-image-smoke.yml' \
        '${{ runner.temp }}/dcentos-image-smoke-${{ github.run_id }}-${{ github.run_attempt }}/releases/' \
        'S9 image smoke uploads the atomic published directory instead of a hand-picked flat sidecar subset'
    require_pattern '../../.github/workflows/dcentos-image-smoke.yml' \
        'AM2 package smoke: intentionally unavailable' \
        'AM2 image-smoke source-closure coverage remains explicitly blocked until an AM2 capsule exists'
}
source_closure_check

# Buildroot package file lists are path claims, not final-content ownership.
# Keep the bounded final-rootfs ledger's deterministic classifier and negative
# controls live in CI without pretending that a fixture proves production image
# attribution or a complete SPDX/CycloneDX SBOM.
rootfs_ownership_ledger_check() {
    require_file 'scripts/rootfs_ownership_ledger.py'
    require_file 'scripts/test_rootfs_ownership_ledger.sh'
    if [ -f 'scripts/test_rootfs_ownership_ledger.sh' ]; then
        if bash 'scripts/test_rootfs_ownership_ledger.sh' >/dev/null 2>&1; then
            pass "final-rootfs ownership ledger is deterministic and keeps ambiguous/unattributed evidence explicit"
        else
            fail "final-rootfs ownership ledger fixture or negative controls regressed"
        fi
    fi
    require_pattern 'scripts/rootfs_ownership_ledger.py' \
        '"is_sbom": False' \
        'rootfs ownership ledger does not overclaim complete SBOM coverage'
    require_pattern 'scripts/rootfs_ownership_ledger.py' \
        'single-buildroot-path-claim;final-content-origin-not-proven' \
        'unique Buildroot package attribution remains explicitly path-claim-only'
}
rootfs_ownership_ledger_check

# Buildroot's own legal-info output is hash-enumerated as a bounded release
# evidence slice. It is artifact-bound and source/license aware, but remains a
# custom partial inventory rather than an SBOM or a license-compliance claim.
buildroot_legal_inventory_check() {
    require_file 'scripts/buildroot_legal_inventory.py'
    require_file 'scripts/test_buildroot_legal_inventory.sh'
    if [ -f 'scripts/test_buildroot_legal_inventory.sh' ]; then
        if bash 'scripts/test_buildroot_legal_inventory.sh' >/dev/null 2>&1; then
            pass "artifact-bound Buildroot legal-info inventory is deterministic and fail-closed"
        else
            fail "Buildroot legal-info inventory fixture or negative controls regressed"
        fi
    fi
    require_pattern 'scripts/buildroot_legal_inventory.py' \
        '"is_sbom": False' \
        'Buildroot legal inventory does not overclaim complete SBOM coverage'
    require_pattern 'scripts/buildroot_legal_inventory.py' \
        '"license_compliance": "not_assessed"' \
        'Buildroot legal inventory does not overclaim license compliance'
    require_pattern 'scripts/buildroot_legal_inventory.py' \
        '"vulnerability_analysis": "not_performed"' \
        'Buildroot legal inventory keeps advisory analysis explicitly unresolved'
    require_pattern 'scripts/build_in_docker.sh' \
        'buildroot_legal_inventory.py generate' \
        'release image producers emit the Buildroot legal-info inventory'
    require_pattern 'scripts/build_in_docker.sh' \
        '--artifact "$BUILDROOT_LEGAL_INVENTORY_PATH"' \
        'signed source-closure receipts bind the Buildroot legal-info inventory'
}
buildroot_legal_inventory_check

# Rust packages are the first dependency-inventory vertical slice. Keep the
# custom schema honest: it is artifact-bound and locked/offline, but does not
# pretend to be a complete SPDX/CycloneDX SBOM for Buildroot firmware.
rust_dependency_inventory_check() {
    require_file 'scripts/rust_dependency_inventory.py'
    require_file 'scripts/test_rust_dependency_inventory.sh'
    if [ -f 'scripts/test_rust_dependency_inventory.sh' ]; then
        if bash 'scripts/test_rust_dependency_inventory.sh' >/dev/null 2>&1; then
            pass "artifact-bound Rust dependency inventory is deterministic and locked offline"
        else
            fail "Rust dependency inventory generation/verification regressed"
        fi
    fi
    require_pattern 'scripts/rust_dependency_inventory.py' \
        '"spdx_conformance": "not_claimed"' \
        'Rust inventory does not overclaim SPDX conformance'
    require_pattern 'scripts/rust_dependency_inventory.py' \
        '"cyclonedx_conformance": "not_claimed"' \
        'Rust inventory does not overclaim CycloneDX conformance'
    require_pattern 'scripts/build_in_docker.sh' \
        'rust_dependency_inventory.py" generate' \
        'release image producers emit the Rust dependency inventory'
    require_pattern 'scripts/build-dcentrald.sh' \
        'cargo metadata --locked --offline --filter-platform' \
        'Rust inventory metadata is emitted locked/offline for the release target'
    require_pattern 'scripts/build-dcentrald.sh' \
        'FROM ${RUST_BUILDER_BASE}' \
        'Rust inventory metadata shares the selected builder base'
    require_pattern 'scripts/build-dcentrald.sh' \
        'builder_image_id=$DCENT_BUILDER_IMAGE_ID' \
        'Rust inventory receipt context binds the inspected builder image ID'
    require_pattern 'scripts/build-dcentrald.sh' \
        '"$DOCKER_IMAGE_ID" \' \
        'Rust inventory build executes the inspected immutable builder image ID'
    require_pattern 'scripts/build_in_docker.sh' \
        '--artifact "$RUST_INVENTORY_PATH"' \
        'source-closure receipt binds the Rust dependency inventory'
    require_pattern '../../.github/workflows/dcentos-image-smoke.yml' \
        'portable_release_evidence.py verify' \
        'S9 image smoke verifies the closure-bound Rust inventory through portable capsule evidence'
    require_pattern '../../.github/workflows/dcentos-image-smoke.yml' \
        'AM2 package smoke: intentionally unavailable' \
        'AM2 Rust-inventory workflow coverage is not claimed without an admitted capsule'
}
rust_dependency_inventory_check

# Amlogic first boot uses /data/.firstboot-pending as a write-ahead marker
# before the raw recovery-flag transition. Keep the crash-injection harness in
# the aggregate gate so the marker cannot regress to unchecked truncate/sync or
# a fail-open continuation into flash/env mutation.
amlogic_firstboot_wal_durability_check() {
    require_file 'scripts/test_amlogic_s99upgrade_wal_durability.sh'
    if [ -f 'scripts/test_amlogic_s99upgrade_wal_durability.sh' ]; then
        if bash 'scripts/test_amlogic_s99upgrade_wal_durability.sh' >/dev/null 2>&1; then
            pass "Amlogic firstboot WAL is durable before recovery authority mutation"
        else
            fail "Amlogic firstboot WAL durability/fail-closed harness regressed"
        fi
    fi
    require_pattern \
        'br2_external_dcentos/board/amlogic/rootfs-overlay/etc/init.d/S99upgrade' \
        'refusing recovery-flag commit' \
        'Amlogic S99upgrade refuses recovery authority when WAL durability is unproven'
}
amlogic_firstboot_wal_durability_check

# OTA downgrade-floor monotonicity gate. The signed-package shell path must
# never treat an older release version as installable, even when the operator's
# lab downgrade override is present. This host-only test extracts the comparator
# and floor functions from every shipped Zynq sysupgrade overlay and drives a
# local fixture matrix without touching hardware or flash.
ota_version_monotonicity_check() {
    require_file 'scripts/test_ota_version_monotonicity.sh'
    if [ -f 'scripts/test_ota_version_monotonicity.sh' ]; then
        if sh 'scripts/test_ota_version_monotonicity.sh' >/dev/null 2>&1; then
            pass "OTA: sysupgrade version monotonicity matrix green"
        else
            fail "OTA: sysupgrade version monotonicity matrix regressed"
        fi
    fi
}
ota_version_monotonicity_check

# Overlay board_target recognition gate (structural fix for the S19j-Pro-class
# mis-route found 2026-07-03: the canonical `am2-s19jpro-zynq` board_target was
# declared/used but the ZynqVariant resolver did NOT recognize it, silently
# routing a BM1362 board to the S9 fail-safe — wrong chain init at first-light).
# Every Buildroot overlay that stamps /etc/dcentos/board_target MUST have that
# exact string recognized by the daemon: either the zynq ZynqVariant resolver
# (matches the dashed form) OR model.rs board_target_chip_label (matches the
# dash-stripped/normalized form). A newly-added SKU overlay that stamps an
# unrecognized target fails HERE instead of mis-routing on live hardware.
overlay_board_target_recognized_check() {
    _zynq='dcentrald/dcentrald-hal/src/platform/zynq.rs'
    _model='dcentrald/dcentrald/src/model.rs'
    for _f in $(find br2_external_dcentos -path '*/etc/dcentos/board_target' 2>/dev/null | sort); do
        _bt=$(tr -d ' \t\r\n' < "$_f")
        [ -n "$_bt" ] || continue
        _norm=$(printf '%s' "$_bt" | tr -d '-')
        if grep -Fq "\"$_bt\"" "$_zynq" 2>/dev/null || grep -Fq "\"$_norm\"" "$_model" 2>/dev/null; then
            pass "overlay board_target '$_bt' is recognized by the daemon resolver"
        else
            fail "overlay board_target '$_bt' (${_f#br2_external_dcentos/board/}) is NOT recognized by the ZynqVariant resolver or board_target_chip_label — a flashed unit would mis-route to the fail-safe variant"
        fi
    done
}
overlay_board_target_recognized_check

# Controller mutation is not a shipped software capability. Keep this proof in
# one executable semantic test so Cargo membership, dependencies, syscalls,
# dashboard ownership, and stale-binary cleanup cannot drift independently.
controller_diagnostic_boundary_check() {
    require_file 'scripts/test_controller_diagnostic_boundary.py'
    require_file 'dcentrald/dcentrald-api-types/src/dspic_frame.rs'
    require_file 'dcentrald/dcentrald-hal/src/stock_fpga_iic.rs'
    require_pattern 'dcentrald/dcentrald-api-types/src/dspic_frame.rs' \
        'SET_HOST_MAC_ADDRESS_OPCODE: u8 = 0x14' \
        'CONTROLLER-DIAGNOSTICS: canonical API catalog pins 0x14 as SET_HOST_MAC_ADDRESS'
    require_pattern 'dcentrald/dcentrald-hal/src/stock_fpga_iic.rs' \
        'SET_HOST_MAC_ADDRESS: u8 = 0x14' \
        'CONTROLLER-DIAGNOSTICS: HAL catalog independently pins 0x14 as SET_HOST_MAC_ADDRESS'
    local diagnostic_output
    if diagnostic_output="$(python3 'scripts/test_controller_diagnostic_boundary.py' 2>&1)"; then
        pass 'CONTROLLER-DIAGNOSTICS: standalone tools are opt-in and structurally read-only'
    else
        printf '%s\n' "$diagnostic_output" \
            | sed 's/^/ERROR: CONTROLLER-DIAGNOSTICS: /' >&2
        fail 'CONTROLLER-DIAGNOSTICS: standalone diagnostic-only boundary regressed'
    fi
}
controller_diagnostic_boundary_check

# Normal-runtime hardware ownership is process-exclusive. Web adapters and
# post-boot checks consume daemon snapshots; normal REST handlers never open a
# second transport; raw research executors are pruned after every product
# overlay. S99verify observes boot-commit state but never owns its mutation.
runtime_hardware_ownership_check() {
    require_file 'scripts/test_runtime_hardware_ownership.py'
    require_file 'scripts/test_s99verify_commit_authority.sh'
    require_file 'br2_external_dcentos/board/common/prune-runtime-research-tools.sh'
    if python3 'scripts/test_runtime_hardware_ownership.py' >/dev/null 2>&1; then
        pass 'HARDWARE-OWNERSHIP: runtime adapters are snapshot-only and research executors are absent from product rootfs images'
    else
        fail 'HARDWARE-OWNERSHIP: parallel runtime hardware access or release-composition pruning regressed'
    fi
    if sh 'scripts/test_s99verify_commit_authority.sh' >/dev/null 2>&1; then
        pass 'HARDWARE-OWNERSHIP: S99verify observes committed/blocked boot state without durable mutation authority'
    else
        fail 'HARDWARE-OWNERSHIP: S99verify boot-commit authority contract regressed'
    fi
}
runtime_hardware_ownership_check

# Recovery-feature manifest/source gate (guarantee #2 from the .74/.139 incident).
# Historical protocol research remains feature-gated inside library crates.
# Cargo features are additive across a resolved graph, so this proves only that
# shipped package manifests do not directly request the feature; source/API
# visibility gates carry the stronger runtime boundary.
recovery_tool_not_in_daemon_check() {
    _dc='dcentrald/dcentrald/Cargo.toml'
    _controller='dcentrald/pic-recovery/Cargo.toml'
    require_file "$_dc"
    require_file "$_controller"
    _hits=$(grep -nE 'recovery-tool' "$_dc" "$_controller" 2>/dev/null | grep -vE '^[^:]+:[0-9]+:[[:space:]]*#' || true)
    if [ -z "$_hits" ]; then
        pass "EEPROM/PIC: shipped daemon/controller manifests do not directly enable recovery-tool"
    else
        fail "EEPROM/PIC: a shipped package enables recovery-tool. Offending line(s): $_hits"
    fi
}
recovery_tool_not_in_daemon_check

# Credential-URL log-redaction gate. Caught 5 real leaks 2026-07-03: the Telegram/
# Discord webhook token (x3 sites), the donation pool URL, the bitcoind RPC URL,
# and the MQTT broker URL. A log field whose VALUE is a known credential-bearing
# URL — any `*_rpc_url` (embeds rpcuser:rpcpassword@host), a webhook URL (embeds a
# bot token in the path), or a `.broker` (mqtt://user:pass@host) — must pass
# through a redactor (sanitize_pool_url / sanitize_webhook_url / redact_rpc_url)
# before it reaches the daemon log / support bundle / dashboard log-tail. A
# redactor is a no-op for a credential-free URL, so a new RAW log of one fails
# HERE instead of shipping the secret. (`.broker` matches the raw field access,
# not a pre-sanitized `broker_display` variable.)
credential_url_log_redaction_check() {
    _hits=$(grep -rnE '= %[A-Za-z0-9_:&.()]*(rpc_url|webhook[A-Za-z0-9_.]*url|\.broker\b)' \
        dcentrald/dcentrald/src dcentrald/dcentrald-api/src dcentrald/dcentrald-stratum/src 2>/dev/null \
        | grep -viE 'sanitize_pool_url|sanitize_webhook_url|redact_rpc_url|redact|mask|//|test' || true)
    if [ -z "$_hits" ]; then
        pass "logging: every credential-bearing URL (rpc_url / webhook url) is run through a redactor before logging"
    else
        fail "logging: a credential-bearing URL is logged RAW — leaks rpcuser:rpcpassword@ or a webhook token to logs/support-bundles. Wrap it in sanitize_pool_url / sanitize_webhook_url. Offending: $_hits"
    fi
}
credential_url_log_redaction_check

# Wallet/PII log-masking gate. On Stratum V1 the `worker` field IS the operator's
# Bitcoin wallet/payout address (likewise fallback_worker + coinbase_output_
# address) — logging it raw leaks the operator's address into the daemon log /
# support bundle / dashboard log-tail. Every such field must pass through
# dcentrald_common::wallet_mask::mask_wallet (the load-bearing W1.4 rule). This
# gate caught a raw `worker = %self.config.pool.worker` in stock_mining.rs
# 2026-07-03. mask_wallet is a no-op on an empty value, so a new raw log fails
# HERE instead of shipping the operator's address. (`.worker` matches the raw
# field access, not worker_count / worker_name / a pre-masked variable.)
wallet_log_masking_check() {
    _hits=$(grep -rnE '= %[A-Za-z0-9_:&.()]*(\.worker\b|coinbase_output_address|fallback_worker|payout_address)' \
        dcentrald/dcentrald/src dcentrald/dcentrald-api/src dcentrald/dcentrald-stratum/src 2>/dev/null \
        | grep -viE 'mask_wallet|mask|redact|sanitize|//|test' || true)
    if [ -z "$_hits" ]; then
        pass "logging: every worker/wallet/coinbase-address log field is masked (mask_wallet)"
    else
        fail "logging: an operator wallet/payout address (worker / coinbase_output_address) is logged RAW — leaks the operator's Bitcoin address to logs/support-bundles. Wrap it in dcentrald_common::wallet_mask::mask_wallet. Offending: $_hits"
    fi
}
wallet_log_masking_check

# CI-GATE-CE026 (reverse A/B + AM2 vendor first-install evidence boundary).
# The AM2 sysupgrade harness proves the already-running-DCENT_OS A/B writer in
# both directions. The separate stage1 first-install harness is S9-only: AM2
# has no authenticated source-runtime capsule or persistent-state migration,
# so injecting virtme host tools must never emit a vendor first-install proof.
ce026_reverse_ab_and_am2_first_install_boundary_check() {
    tag='CI-GATE-CE026'
    harness='scripts/sysupgrade_offline_nandsim_harness.sh'
    stage1='scripts/stage1_first_install_offline_nandsim_harness.sh'
    runner='scripts/sysupgrade_offline_virtme_nandsim_runner.sh'
    workflow='../../.github/workflows/dcentos-offline-nandsim.yml'
    capsule_contract='../dcent-toolbox/docs/AM2_FIRST_INSTALL_CAPSULE.md'

    require_file "$harness"
    require_file "$stage1"
    require_file "$runner"
    require_file "$workflow"
    require_file "$capsule_contract"
    if [ ! -f "$harness" ] || [ ! -f "$stage1" ] || [ ! -f "$runner" ] \
        || [ ! -f "$workflow" ] || [ ! -f "$capsule_contract" ]; then
        return
    fi

    # (1) REVERSE A/B in the sysupgrade harness: the --current-fw selector, the
    #     reverse both-slots nandsim layout, and the DISTINCT reverse sentinel.
    #     The default forward path (current-fw=2) stays byte-identical.
    require_pattern "$harness" '--current-fw' \
        "$tag REVERSE: sysupgrade harness exposes the --current-fw {1,2} selector"
    require_pattern "$harness" 'NANDSIM_PARTS_REVERSE' \
        "$tag REVERSE: sysupgrade harness defines the reverse both-slots layout"
    require_pattern "$harness" '1,1,1,1,4,1,1,900,900' \
        "$tag REVERSE: reverse layout provisions BOTH slots (mtd7 + mtd8, 128KiB eraseblocks)"
    require_pattern "$harness" 'OFFLINE_NANDSIM_PROOF_OK target=\$TARGET direction=reverse current_fw=1 inactive_mtd=8' \
        "$tag REVERSE: sysupgrade harness emits the distinct reverse sentinel (current_fw=1 inactive_mtd=8)"

    # (2) AM2 FIRST-INSTALL BOUNDARY: the S9 stage1 harness cannot accept an
    #     AM2 target/package or emit an AM2 first-install proof. The missing
    #     capsule remains explicit in the architecture contract.
    require_pattern "$stage1" 'This harness is intentionally S9-only' \
        "$tag AM2-FIRST-INSTALL: stage1 harness declares its S9-only authority"
    reject_pattern "$stage1" 'am2-s19jpro' \
        "$tag AM2-FIRST-INSTALL: stage1 harness has no AM2 target"
    reject_pattern "$stage1" '--am2-package' \
        "$tag AM2-FIRST-INSTALL: stage1 harness has no AM2 package input"
    reject_pattern "$stage1" 'OFFLINE_FIRST_INSTALL_PROOF_OK target=am2' \
        "$tag AM2-FIRST-INSTALL: stage1 harness cannot emit an AM2 proof sentinel"
    require_pattern "$capsule_contract" '^Status: architecture contract; not implemented$' \
        "$tag AM2-FIRST-INSTALL: capsule contract remains explicitly unimplemented"
    require_pattern "$capsule_contract" 'must refuse before package upload or target mutation' \
        "$tag AM2-FIRST-INSTALL: capsule contract refuses vendor-source mutation"

    # (3) RUNNER COVERAGE + HONEST CI WIRING: the reusable runner retains AM2
    #     DCENT_OS A/B coverage but cannot invoke a vendor first-install path.
    #     The workflow asserts only capsule-backed S9 first-install sentinels
    #     and carries an explicit AM2 blocked disposition.
    require_pattern "$runner" '--current-fw 1 --target am1-s9' \
        "$tag CI-WIRING: runner invokes the am1-s9 reverse-direction guest proof"
    require_pattern "$runner" '--current-fw 1 --target am2-s19jpro' \
        "$tag CI-WIRING: runner invokes the am2-s19jpro reverse-direction guest proof"
    reject_pattern "$runner" '--target am2-s19jpro --am2-package' \
        "$tag CI-WIRING: runner has no synthetic AM2 first-install invocation"
    reject_pattern "$runner" '/tmp/dcent-first-install-proof-am2' \
        "$tag CI-WIRING: runner has no AM2 first-install proof workspace"
    require_pattern "$workflow" "grep -q 'OFFLINE_NANDSIM_PROOF_OK target=am1-s9 direction=reverse'" \
        "$tag CI-WIRING: workflow asserts the am1-s9 reverse sentinel"
    require_pattern "$workflow" "grep -q 'OFFLINE_FIRST_INSTALL_PROOF_OK target=am1-s9'" \
        "$tag CI-WIRING: workflow asserts the am1-s9 first-install sentinel"
    require_pattern "$workflow" 'AM2 nandsim: intentionally unavailable' \
        "$tag CI-WIRING: workflow reports AM2 nandsim as unavailable without a capsule"
    require_pattern "$workflow" 'No AM2 nandsim, first-install, OTA-parser, boot, or mining claim is made by this run.' \
        "$tag CI-WIRING: workflow explicitly bounds the missing AM2 dynamic claims"
    reject_pattern "$workflow" "grep -q 'OFFLINE_NANDSIM_PROOF_OK target=am2-s19jpro direction=reverse'" \
        "$tag CI-WIRING: workflow does not assert an AM2 reverse proof it cannot build"
    reject_pattern "$workflow" "grep -q 'OFFLINE_FIRST_INSTALL_PROOF_OK target=am2-s19jpro'" \
        "$tag CI-WIRING: workflow does not assert an AM2 first-install proof it cannot build"
}
ce026_reverse_ab_and_am2_first_install_boundary_check

# CE-114: release-image proxy-nonce weak-entropy fail-closed (defense-in-depth).
# (a) every board S80dashboard that keeps the weak date+pid+uptime fallback must
#     release-gate it (reference '/etc/dcentos/release-image' in generate_proxy_nonce
#     so the weak fallback is refused on a release image).
# (b) the dcentrald-api backend refuses to trust a non-64-hex proxy nonce on a
#     release image (auth.rs is_strong_proxy_nonce).
ce114_proxy_nonce_weak_entropy_check() {
    s80_found=0
    s80_missing=''
    for s80 in br2_external_dcentos/board/*/rootfs-overlay/etc/init.d/S80dashboard; do
        [ -f "$s80" ] || continue
        s80_found=$((s80_found + 1))
        if grep -F -- 'date +%s%N' "$s80" >/dev/null 2>&1 \
           && ! grep -F -- '/etc/dcentos/release-image' "$s80" >/dev/null 2>&1; then
            s80_missing="$s80_missing $s80"
        fi
    done
    if [ "$s80_found" -eq 0 ]; then
        fail "CE-114: no board S80dashboard init scripts found (path drift?)"
    elif [ -n "$s80_missing" ]; then
        fail "CE-114 S80dashboard: weak date+pid+uptime proxy-nonce fallback NOT release-gated in:$s80_missing"
    else
        pass "CE-114 S80dashboard: weak proxy-nonce fallback release-gated in all $s80_found scripts"
    fi
    ce114_auth='dcentrald/dcentrald-api/src/auth.rs'
    if [ -f "$ce114_auth" ]; then
        require_pattern "$ce114_auth" 'fn is_strong_proxy_nonce' \
            "CE-114 auth: strong-entropy proxy-nonce validator present"
        require_pattern "$ce114_auth" 'nonce.map(is_strong_proxy_nonce)' \
            "CE-114 auth: release trust path requires a strong-entropy nonce"
    else
        fail "CE-114: dcentrald-api/src/auth.rs not found (path drift?)"
    fi
}
ce114_proxy_nonce_weak_entropy_check

# SIM-HAL: host simulator must remain impossible to reference from a default
# build and absent from every firmware/Buildroot profile. The compile probe is
# a real downstream crate (not a source grep): it imports SimPlatform without
# enabling the feature and MUST fail to compile.
sim_hal_nonshipping_gate() {
    hal_manifest='dcentrald/dcentrald-hal/Cargo.toml'
    platform_mod='dcentrald/dcentrald-hal/src/platform/mod.rs'
    require_pattern "$hal_manifest" 'sim-hal = [' \
        'SIM-HAL: opt-in Cargo feature exists'
    require_pattern 'dcentrald/dcentrald-hal/src/lib.rs' \
        'sim-hal is host-only and must never be compiled into ARM Linux firmware artifacts' \
        'SIM-HAL: ARM Linux firmware compile guard is present'
    require_pattern "$platform_mod" '#[cfg(feature = "sim-hal")]' \
        'SIM-HAL: module export is compile-time gated'

    if grep -R -n --include='*defconfig' --include='*.mk' --include='Config.in' \
        'sim-hal' br2_external_dcentos >/dev/null 2>&1; then
        fail 'SIM-HAL: a Buildroot/release profile enables the host simulator feature'
    else
        pass 'SIM-HAL: no Buildroot/release profile enables the host simulator feature'
    fi

    [ "$STATIC_ONLY" -eq 0 ] || return 0
    sim_rust_toolchain="${DCENT_RUST_TOOLCHAIN:-1.90.0}"
    sim_rustup="$(command -v rustup 2>/dev/null || true)"
    if [ -z "$sim_rustup" ] && [ -x "${HOME:-}/.cargo/bin/rustup" ]; then
        sim_rustup="${HOME}/.cargo/bin/rustup"
    fi
    if [ -z "$sim_rustup" ]; then
        fail 'SIM-HAL compile-fail probe: rustup is unavailable for the pinned toolchain'
        return
    fi
    if ! "$sim_rustup" run "$sim_rust_toolchain" cargo --version >/dev/null 2>&1; then
        fail "SIM-HAL compile-fail probe: pinned Rust toolchain is unavailable: $sim_rust_toolchain"
        return
    fi

    sim_probe_dir=$(mktemp -d "${TMPDIR:-/tmp}/dcent-sim-hal-negative.XXXXXX")
    mkdir -p "$sim_probe_dir/src"
    cat >"$sim_probe_dir/Cargo.toml" <<EOF
[package]
name = "dcent-sim-hal-negative-probe"
version = "0.0.0"
edition = "2021"

[workspace]

[dependencies]
dcentrald-hal = { path = "$PROJECT_DIR/dcentrald/dcentrald-hal" }
EOF
    cat >"$sim_probe_dir/src/main.rs" <<'EOF'
use dcentrald_hal::platform::sim::SimPlatform;

fn main() {
    let _ = std::mem::size_of::<SimPlatform>();
}
EOF
    if "$sim_rustup" run "$sim_rust_toolchain" cargo check --quiet \
        --manifest-path "$sim_probe_dir/Cargo.toml" \
        >"$sim_probe_dir/stdout" 2>"$sim_probe_dir/stderr"; then
        fail 'SIM-HAL compile-fail probe: SimPlatform was linkable without --features sim-hal'
    elif grep -E 'could not find `sim`|unresolved import.*platform::sim' \
        "$sim_probe_dir/stderr" >/dev/null 2>&1; then
        pass 'SIM-HAL compile-fail probe: default dependency cannot reference SimPlatform'
    else
        fail 'SIM-HAL compile-fail probe failed for an unexpected reason (not the feature gate)'
        sed -n '1,80p' "$sim_probe_dir/stderr" >&2
    fi
    rm -rf -- "$sim_probe_dir"
}
sim_hal_nonshipping_gate

# SIM-HAL evidence/contract meta-gates. These are static and host-safe: they
# neither contact a pool nor touch a device. The full VM/nandsim proof remains
# a separate, artifact-bearing CI job because it needs a compatible kernel.
sim_hal_evidence_contract_gates() {
    for script in \
        scripts/sim/bringup_ladder.sh \
        scripts/sim/full_offline_model_proof.sh \
        scripts/sim/virtme_sim_hal_runner.sh \
        scripts/sim/wsl_namespace_sim_hal_runner.sh \
        scripts/sim/wsl_all_model_proof.sh; do
        if bash -n "$script"; then
            pass "SIM-HAL: shell syntax valid for $script"
        else
            fail "SIM-HAL: shell syntax invalid for $script"
        fi
    done
    if python3 scripts/sim/check_sim_tier_honesty.py; then
        pass 'SIM-HAL: S9-S23 declared tiers do not exceed checked evidence'
    else
        fail 'SIM-HAL: tier-honesty matrix failed'
    fi
    if python3 scripts/sim/check_esp_contract_parity.py; then
        pass 'ESP convergence: donation/onboarding contract parity gate passed'
    else
        fail 'ESP convergence: donation/onboarding contract parity gate failed'
    fi

    # A simulator that only runs on a developer workstation is not a release
    # gate.  Pin the executable model proofs to the workflow so they cannot
    # become another orphaned safety suite during CI refactors.
    sim_workflow='../../.github/workflows/dcentos-offline-gates.yml'
    require_pattern "$sim_workflow" 'sim-hal-contract:' \
        'SIM-HAL CI: independent executable contract job is present'
    require_pattern "$sim_workflow" \
        'cargo test -p dcentrald-asic --features sim-hal --test golden_init_trace' \
        'SIM-HAL CI: provenance-backed golden initialization traces execute'
    require_pattern "$sim_workflow" \
        'cargo test -p dcentrald --features sim-hal --test sim_s19pro_t2' \
        'SIM-HAL CI: ten-model T2 enumeration/init/share proof executes'
    require_pattern "$sim_workflow" \
        'i2c_service_deadline_tests' \
        'SIM-HAL CI: deadline-aware serialized I2C regression executes'
    require_pattern "$sim_workflow" \
        'cargo test -p dcentrald-hal --features sim-hal --test pic16_admission' \
        'SIM-HAL CI: worker-owned PIC16 batch admission and receipt integration executes'
    require_pattern "$sim_workflow" \
        'cargo test -p dcentrald-asic --features sim-hal --test sim_pic16_runtime' \
        'SIM-HAL CI: PIC16 cold-boot runtime grammar and admission regression executes'
    require_pattern "$sim_workflow" \
        'cargo test -p dcentrald --features sim-hal --bin dcentrald hardware_preflight_policy_tests' \
        'SIM-HAL CI: daemon PIC16 controller-admission regression executes'
    require_pattern "$sim_workflow" \
        'cargo test -p dcentrald --features sim-hal --bin dcentrald initialized_pic_addrs_tests' \
        'SIM-HAL CI: PIC16 heartbeat membership remains deduplicated'
    require_pattern "$sim_workflow" \
        'caller_supplied_privileged_intent_surface_stays_crate_private' \
        'SIM-HAL CI: I2C privileged-intent visibility regression executes'
    require_pattern "$sim_workflow" \
        'cargo test -p dcentrald-hal --doc' \
        'SIM-HAL CI: I2C privileged-intent compile-fail contract executes'
    require_pattern "$sim_workflow" \
        'init_heartbeat_ownership_tests' \
        'SIM-HAL CI: initialization-heartbeat ownership regression executes'
    require_pattern "$sim_workflow" \
        'cargo test -p dcentrald --features sim-hal --bin dcentrald voltage_mailbox::tests' \
        'SIM-HAL CI: prioritized voltage mailbox lifecycle regressions execute'
    require_pattern "$sim_workflow" \
        'psu_apw12_smbus::tests::power_off' \
        'SIM-HAL CI: PSU safe-off failure and compensation regression executes'
    require_pattern "$sim_workflow" \
        'psu_apw12_smbus::tests::cold_boot' \
        'SIM-HAL CI: partial cold-boot rollback regression executes'
    require_pattern "$sim_workflow" \
        'runtime::thread_guard::tests' \
        'SIM-HAL CI: bounded runtime-thread ownership regression executes'
    require_pattern "$sim_workflow" \
        'am2_power_shutdown_evidence_tests' \
        'SIM-HAL CI: AM2 shutdown evidence regression executes'
}
sim_hal_evidence_contract_gates

# Native NoPic safety-watchdog evidence must remain executable in hosted CI.
# Source invariants complement (not replace) the fake-device behavioral tests:
# every API PSU mutator is admitted through the shared gate, serial teardown
# drains that gate before safe-off, and fan liveness uses checked actuation.
nopic_watchdog_evidence_contract_gates() {
    workflow='../../.github/workflows/dcentos-offline-gates.yml'
    require_pattern "$workflow" \
        'cargo test -p dcentrald --bin dcentrald runtime::safety_watchdog::tests' \
        'NoPic watchdog CI: fail-closed worker state-machine tests execute'
    require_pattern "$workflow" \
        'cargo test -p dcentrald --bin dcentrald nopic_watchdog_and_safeoff_order_is_fail_closed' \
        'NoPic watchdog CI: engine teardown source-order contract executes'
    require_pattern "$workflow" \
        'cargo test -p dcentrald --bin dcentrald watchdog_armed_on_all_mining_entry_paths' \
        'NoPic watchdog CI: mining entry-path admission contract executes'
    require_pattern "$workflow" \
        'cargo test -p dcentrald-hal --lib hardware_mutation_gate_tests' \
        'NoPic watchdog CI: control-plane mutation drain contract executes'
    require_pattern "$workflow" \
        'checked_fan_command_surfaces_partial_two_channel_write' \
        'NoPic watchdog CI: partial two-channel fan failure executes'
    require_pattern "$workflow" \
        'checked_psu_gpio_parser_never_converts_unknown_data_to_off' \
        'NoPic watchdog CI: unknown GPIO readback remains fail-closed'
    require_pattern 'dcentrald/dcentrald-api/src/rest/late.rs' \
        'state.hardware_mutation_gate.try_acquire()' \
        'NoPic watchdog: API PSU mutations acquire teardown-drain admission'
    require_pattern 'dcentrald/dcentrald/src/serial_mining.rs' \
        '.close_and_drain(RUNTIME_THREAD_STOP_TIMEOUT)' \
        'NoPic watchdog: serial teardown closes and drains API mutations'
    require_pattern 'dcentrald/dcentrald/src/serial_mining.rs' \
        'fan.set_speed_checked' \
        'NoPic watchdog: thermal safety liveness uses checked fan actuation'
}
nopic_watchdog_evidence_contract_gates

# A watchdog-disarm followed by a voltage-minimum command removes the PSU's
# independent cutoff before the safe-direction command is known to have
# completed. Runtime code must use Apw121215a::safe_shutdown_to_min(), whose
# ordering is minimum first and disarm second.
if grep -R -n --include='*.rs' '\.watchdog(false)' dcentrald/dcentrald/src >/tmp/dcentos-naked-psu-disarm.$$ 2>/dev/null; then
    fail 'PSU safe-off ordering: naked watchdog(false) remains in daemon runtime code'
    cat /tmp/dcentos-naked-psu-disarm.$$ >&2
else
    pass 'PSU safe-off ordering: daemon runtime uses the minimum-then-disarm coordinator'
fi
rm -f /tmp/dcentos-naked-psu-disarm.$$
require_pattern 'dcentrald/dcentrald-hal/src/psu.rs' \
    'pub fn safe_shutdown_to_min' \
    'PSU safe-off ordering: typed minimum-then-disarm coordinator exists'

# Raw public I2C operations are conservatively terminal-fenced mutations;
# privileged intent is HAL-internal. Production protocol modules use typed
# plans or audit-only mutation labels instead of the compatibility transaction.
if grep -R -n --include='*.rs' --exclude='i2c.rs' '\.transaction(' \
    dcentrald/dcentrald-hal/src \
    dcentrald/dcentrald-asic/src \
    dcentrald/dcentrald/src \
    >/tmp/dcentos-untyped-i2c-transaction.$$ 2>/dev/null; then
    fail 'I2C mutation labeling: compatibility transaction call remains in production modules'
    cat /tmp/dcentos-untyped-i2c-transaction.$$ >&2
else
    pass 'I2C mutation labeling: raw public operations are conservatively fenced; production compound transactions use typed plans or audit labels'
fi
rm -f /tmp/dcentos-untyped-i2c-transaction.$$

# Application crates must never regain the ability to select the HAL's
# authorizing intent or invoke an intent-bearing executor. Keep the scan on
# production source roots so compile-fail documentation and HAL unit tests do
# not become false positives.
if grep -R -n --include='*.rs' -E 'I2cOperationIntent|_with_intent' \
    dcentrald/dcentrald-asic/src \
    dcentrald/dcentrald/src \
    >/tmp/dcentos-external-i2c-intent.$$ 2>/dev/null; then
    fail 'I2C privilege boundary: application crate references HAL-internal intent authority'
    cat /tmp/dcentos-external-i2c-intent.$$ >&2
else
    pass 'I2C privilege boundary: privileged intent and intent-bearing executors remain HAL-internal'
fi
rm -f /tmp/dcentos-external-i2c-intent.$$

# AM3-BB carrier contract: keep the active `a lab unit` board-target, HAL constants,
# safe boot/shutdown GPIO directions, UART topology, DTB admission gates, and
# the quarantined legacy BBCtrl/S70 DTS from silently converging into one
# misleading product definition.  Both commands are host-only Python checks.
if run_python_script scripts/test_am3_bb_hardware_contract.py; then
    pass 'AM3-BB hardware contract: negative fixtures reject unsafe drift'
else
    fail 'AM3-BB hardware contract: negative-fixture suite failed'
fi
if run_python_script scripts/check_am3_bb_hardware_contract.py --root "$PROJECT_DIR"; then
    pass 'AM3-BB hardware contract: catalog and all offline consumers agree'
else
    fail 'AM3-BB hardware contract: board/DTS/build consistency gate failed'
fi

# Safety regressions must be executable from this canonical gate, not only from
# a workflow step or a developer-local command.
if sh scripts/test_run_wave_regressions_driver.sh >/dev/null 2>&1; then
    pass 'wave regression driver: dependency-lock and exact-index command contract is pinned'
else
    fail 'wave regression driver: dependency-lock or command contract regressed'
fi
if sh scripts/test_sysupgrade_resource_ledger.sh >/dev/null 2>&1; then
    pass 'sysupgrade resource ledger: durable ownership and reconciliation contract is pinned'
else
    fail 'sysupgrade resource ledger: ownership or reconciliation contract regressed'
fi
if sh scripts/test_verify_sysupgrade_signature.sh >/dev/null 2>&1; then
    pass 'sysupgrade signature verifier: executable signed-envelope contract is pinned'
else
    fail 'sysupgrade signature verifier: signed-envelope contract regressed'
fi

# A release panic aborts without running Rust Drop cleanup. Until the durable
# hardware-disposition journal and startup resolver are active, a fresh daemon
# must not be admitted automatically after an abnormal exit. Check every board
# overlay dynamically so new product supervisors inherit the same fail-closed
# policy instead of reintroducing a bounded-but-unsafe crash loop.
if sh scripts/test_dcentrald_crash_restart_policy.sh; then
    pass 'dcentrald crash policy: every shipped supervisor refuses automatic readmission'
else
    fail 'dcentrald crash policy: a shipped supervisor can automatically readmit after an abnormal exit'
fi

if sh scripts/test_amlogic_boot_safe_state.sh; then
    pass 'Amlogic lifecycle: boot baseline, runtime handoff, crash cut, and PID1 ordering are pinned'
else
    fail 'Amlogic lifecycle: boot/runtime/crash safe-state contract regressed'
fi

if sh scripts/test_dcentos_receipt_core.sh; then
    pass 'compiled receipt foundation: SHA-256 and complete resource/claim transitions are pinned'
else
    fail 'compiled receipt foundation: hash or state-machine boundary regressed'
fi
if sh scripts/test_dcentos_receipt_parser.sh; then
    pass 'compiled receipt ABI1: canonical byte parsers and semantic chains are pinned'
else
    fail 'compiled receipt ABI1: parser or semantic-chain boundary regressed'
fi
if sh scripts/test_dcentos_receipt_store.sh; then
    pass 'compiled receipt storage: descriptor-only topology and race boundary is pinned'
else
    fail 'compiled receipt storage: descriptor, metadata, topology, or race boundary regressed'
fi
if sh scripts/test_dcentos_receipt_storage.sh; then
    pass 'compiled receipt ABI2 storage: seal/head grammar and composite manifest-pair validation are pinned'
else
    fail 'compiled receipt ABI2 storage: parser, parity, linkage, or delta boundary regressed'
fi
if sh scripts/test_dcentos_receipt_fuzz_corpus.sh; then
    pass 'compiled receipt ABI2 fuzz corpus: every structured seed is a real valid manifest pair'
else
    fail 'compiled receipt ABI2 fuzz corpus: framed seed or pair validity regressed'
fi
if sh scripts/test_dcentos_receipt_projection.sh; then
    pass 'compiled receipt ABI2 projection: complete bounded chronology and surviving-head projections are pinned'
else
    fail 'compiled receipt ABI2 projection: chronology, authority, prefix, or manifest boundary regressed'
fi
if sh scripts/test_dcentos_receipt_quality.sh; then
    pass 'compiled receipt quality: sanitizer, analyzer, and projection stack budgets are durable'
else
    fail 'compiled receipt quality: sanitizer, analyzer, or projection stack boundary regressed'
fi
if grep -Eq '^[[:space:]]*run:[[:space:]]+sh[[:space:]]+scripts/test_dcentos_receipt_cross_compile\.sh[[:space:]]*$' \
    ../../.github/workflows/dcentos-image-smoke.yml; then
    pass 'compiled receipt foundation: exact Zynq cross proof runs only after restricted-input provisioning'
else
    fail 'compiled receipt foundation: restricted-input image smoke no longer invokes the exact Zynq cross proof'
fi

if sh scripts/test_sysupgrade_mount_identity.sh >/dev/null 2>&1 &&
   sh scripts/test_sysupgrade_ubi_volume_plan.sh >/dev/null 2>&1; then
    pass 'Zynq state observers: mount and UBI identities are admitted without mutation'
else
    fail 'Zynq state observers: mount or UBI identity admission regressed'
fi

if sh scripts/test_zynq_nandsim_geometry.sh >/dev/null 2>&1; then
    pass 'Zynq nandsim geometry: evidence-derived emulator tuple remains subordinate to package authority'
else
    fail 'Zynq nandsim geometry: emulator tuple or package cross-contract regressed'
fi

if sh scripts/test_zynq_sysupgrade_geometry.sh >/dev/null 2>&1 &&
   sh scripts/test_zynq_payload_geometry_integration.sh >/dev/null 2>&1; then
    pass 'Zynq payload geometry: canonical boundaries and producer/consumer wiring are enforced'
else
    fail 'Zynq payload geometry: canonical boundaries or producer/consumer wiring regressed'
fi

# Anti-orphan meta-gate (structural fix for the recurring orphaned-safety-test
# class — ESP ban-gates, dcent-schema, and packaging-static all once ran NOWHERE,
# so a test that LOOKS like coverage protected nothing). Enforce that every
# scripts/test_*.sh safety test is actually invoked by THIS offline gate (the
# aggregator CI runs). A newly-added test that is not wired in fails here instead
# of silently rotting un-run. Live-hardware probes are named *_probe.sh (not
# test_*.sh) and are correctly excluded by the glob.
anti_orphan_test_gate() {
    _self='scripts/ci_offline_gates.sh'
    # RECURSIVE: a nested safety test (e.g. scripts/hw-acceptance/test_*.sh) is
    # just as orphanable as a top-level one, so walk the whole scripts/ tree — not
    # a shallow `scripts/test_*.sh` glob that would miss them. Paths carry no
    # spaces, so word-splitting the find output is safe here.
    for _t in $(find scripts -name 'test_*.sh' 2>/dev/null | sort); do
        [ -f "$_t" ] || continue
        _b=$(basename "$_t")
        if grep -Fq "$_b" "$_self"; then
            pass "anti-orphan: $_b is invoked by the offline gate"
        else
            fail "anti-orphan: $_b is a safety test that ci_offline_gates.sh does NOT invoke (orphaned — wire it in so it actually gates the release)"
        fi
    done
}
anti_orphan_test_gate

if [ "$failures" -ne 0 ]; then
    printf '\nDCENT_OS offline gates failed: %s failure(s)\n' "$failures" >&2
    exit 1
fi

printf '\nDCENT_OS offline gates passed.\n'
