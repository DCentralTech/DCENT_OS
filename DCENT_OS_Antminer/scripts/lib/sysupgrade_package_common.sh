#!/bin/sh
#
# Shared manifest/signature helpers for Buildroot board post-image scripts.

DCENT_SYSUPGRADE_AUTHORITY_PROFILE='dcentos.sysupgrade-authority/v1'
DCENT_SYSUPGRADE_UNSIGNED_LAB_PROFILE='dcentos.sysupgrade-unsigned-lab/v1'

if ! command -v dcent_release_provenance_init >/dev/null 2>&1; then
    dcent_release_envelope_lib=""
    if [ -n "${PROJECT_ROOT:-}" ] && [ -f "${PROJECT_ROOT}/scripts/lib/release_envelope.sh" ]; then
        dcent_release_envelope_lib="${PROJECT_ROOT}/scripts/lib/release_envelope.sh"
    elif [ -n "${BR2_EXTERNAL_DCENTOS_PATH:-}" ] &&
         [ -f "${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/lib/release_envelope.sh" ]; then
        dcent_release_envelope_lib="${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/lib/release_envelope.sh"
    elif [ -f "scripts/lib/release_envelope.sh" ]; then
        dcent_release_envelope_lib="scripts/lib/release_envelope.sh"
    fi
    [ -n "$dcent_release_envelope_lib" ] || {
        echo "ERROR: cannot locate scripts/lib/release_envelope.sh" >&2
        exit 1
    }
    . "$dcent_release_envelope_lib"
fi

dcent_is_truthy() {
    case "${1:-}" in
        1|true|TRUE|yes|YES|y|Y) return 0 ;;
        *) return 1 ;;
    esac
}

dcent_is_release_status() {
    case "${1:-release}" in
        release|production|stable) return 0 ;;
        *) return 1 ;;
    esac
}

# CE-183: a release-status package must not decouple from release-image
# hardening (root SSH lockdown + /etc/dcentos/release-image marker).
dcent_require_release_image_hardening() {
    package_status="${DCENT_PACKAGE_STATUS:-release}"
    dcent_is_release_status "$package_status" || return 0
    if ! dcent_is_truthy "${DCENT_RELEASE_IMAGE:-0}"; then
        echo "ERROR: DCENT_PACKAGE_STATUS='${package_status}' (release-status) requires DCENT_RELEASE_IMAGE=1" >&2
        echo "       (defconfig root-lock + /etc/dcentos/release-image marker)." >&2
        echo "       Release-root signatures are reserved for fully hardened release profiles." >&2
        exit 1
    fi
    if [ -n "${TARGET_DIR:-}" ] && [ ! -f "${TARGET_DIR}/etc/dcentos/release-image" ]; then
        echo "ERROR: release-status package but ${TARGET_DIR}/etc/dcentos/release-image is missing" >&2
        echo "       (release_image_provision.sh did not stamp this rootfs); refusing to package." >&2
        exit 1
    fi
}

dcent_require_unsigned_lab_override() {
    reason="$1"
    package_status="${DCENT_PACKAGE_STATUS:-release}"

    if dcent_is_release_status "$package_status"; then
        echo "ERROR: production release package requires trusted release keys/signatures; refusing ${reason}" >&2
        echo "       Set DCENT_PACKAGE_STATUS to a non-release lab value and DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1 only for lab packages." >&2
        exit 1
    fi

    if ! dcent_is_truthy "${DCENT_ALLOW_UNSIGNED_SYSUPGRADE:-0}"; then
        echo "ERROR: ${reason} requires explicit lab override DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1" >&2
        exit 1
    fi
}

dcent_require_canonical_unsigned_lab_status() {
    dcent_require_unsigned_lab_override "unsigned package generation"
    package_status="${DCENT_PACKAGE_STATUS:-release}"
    if [ "$package_status" != "lab_unsigned" ]; then
        echo "ERROR: unsigned sysupgrade profile requires exact DCENT_PACKAGE_STATUS=lab_unsigned (found '${package_status}')" >&2
        exit 1
    fi
}

dcent_require_canonical_authority_status() {
    package_status="${DCENT_PACKAGE_STATUS:-release}"
    case "$package_status" in
        ''|*[!A-Za-z0-9._+:-]*)
            echo "ERROR: signed authority profile requires DCENT_PACKAGE_STATUS to be a non-empty JSON-safe token using only A-Za-z0-9._+:-" >&2
            exit 1
            ;;
    esac
    if ! dcent_release_require_signed_authority_profile \
        "${DCENT_RELEASE_SIGNING_KEY:-}"; then
        echo "ERROR: signed authority profile is not an admitted release profile" >&2
        exit 1
    fi
}

dcent_sysupgrade_manifest_profile() {
    if [ "${DCENT_RELEASE_KEY_STAGED:-0}" = "1" ]; then
        dcent_require_canonical_authority_status
        printf '%s\n' "$DCENT_SYSUPGRADE_AUTHORITY_PROFILE"
        return 0
    fi

    dcent_require_canonical_unsigned_lab_status
    printf '%s\n' "$DCENT_SYSUPGRADE_UNSIGNED_LAB_PROFILE"
}

dcent_stage_release_key() {
    DCENT_RELEASE_KEY_STAGED=0
    DCENT_RELEASE_KEY_SIZE=""
    DCENT_RELEASE_KEY_SHA256=""

    if [ -z "${DCENT_RELEASE_SIGNING_KEY:-}" ]; then
        if [ "${DCENT_REQUIRE_RELEASE_KEY:-0}" = "1" ]; then
            echo "ERROR: DCENT_REQUIRE_RELEASE_KEY=1 but DCENT_RELEASE_SIGNING_KEY is unset" >&2
            exit 1
        fi
        dcent_require_canonical_unsigned_lab_status
        for forbidden_leaf in MANIFEST.sig release_ed25519.pub; do
            if [ -e "$SUP_DIR/$forbidden_leaf" ]; then
                echo "ERROR: unsigned lab package staging contains forbidden $forbidden_leaf" >&2
                exit 1
            fi
        done
        echo "WARNING: no signing key configured - package is unsigned (lab-only)"
        return 0
    fi
    dcent_require_canonical_authority_status

    [ -f "$DCENT_RELEASE_SIGNING_KEY" ] || {
        echo "ERROR: signing key not found: $DCENT_RELEASE_SIGNING_KEY" >&2
        exit 1
    }
    command -v openssl >/dev/null 2>&1 || {
        echo "ERROR: openssl is required when DCENT_RELEASE_SIGNING_KEY is set" >&2
        exit 1
    }

    if [ -n "${DCENT_RELEASE_PUBKEY_FILE:-}" ]; then
        [ -f "$DCENT_RELEASE_PUBKEY_FILE" ] || {
            echo "ERROR: release public key not found: $DCENT_RELEASE_PUBKEY_FILE" >&2
            exit 1
        }
        cp "$DCENT_RELEASE_PUBKEY_FILE" "$SUP_DIR/release_ed25519.pub"
    else
        echo "ERROR: release-root signing requires a pinned DCENT_RELEASE_PUBKEY_FILE" >&2
        echo "       The package must never derive its trust root from the signing key." >&2
        exit 1
    fi

    DCENT_RELEASE_KEY_SIZE=$(stat -c%s "$SUP_DIR/release_ed25519.pub" 2>/dev/null || stat -f%z "$SUP_DIR/release_ed25519.pub")
    DCENT_RELEASE_KEY_SHA256=$(sha256sum "$SUP_DIR/release_ed25519.pub" | awk '{print $1}')
    echo "${DCENT_RELEASE_KEY_SHA256}  release_ed25519.pub" >> "$SUP_DIR/SHA256SUMS"
    DCENT_RELEASE_KEY_STAGED=1
}

dcent_toolbox_command_has_token() {
    _dcent_toolbox_command=$1
    _dcent_toolbox_token=$2
    case " $_dcent_toolbox_command " in
        *" $_dcent_toolbox_token "*) return 0 ;;
        *) return 1 ;;
    esac
}

dcent_require_toolbox_install_contract() {
    _dcent_toolbox_command=$1
    _dcent_toolbox_install_mode=$2

    case "$_dcent_toolbox_command" in
        "dcent install <ip> -f "*) ;;
        *)
            echo "ERROR: toolbox install metadata is not a target-bound dcent install command" >&2
            return 1
            ;;
    esac
    if dcent_toolbox_command_has_token "$_dcent_toolbox_command" "--yes"; then
        echo "ERROR: toolbox install metadata must preserve interactive confirmation" >&2
        return 1
    fi

    case "$_dcent_toolbox_install_mode" in
        host_driven_rootfs_window_lab)
            if ! dcent_toolbox_command_has_token "$_dcent_toolbox_command" "--artifact-dir"; then
                echo "ERROR: Amlogic install metadata must require restore-verified --artifact-dir evidence" >&2
                return 1
            fi
            if dcent_toolbox_command_has_token \
                "$_dcent_toolbox_command" "--accept-vnish-aml-rootfs-window"; then
                echo "ERROR: package metadata must not pre-acknowledge the VNish-source safety gate" >&2
                return 1
            fi
            ;;
        target_sysupgrade)
            case "${BOARD_NAME:-}" in
                am2-*)
                    for _dcent_toolbox_required_arg in \
                        --artifact-dir \
                        --accept-am2-persistent-lab \
                        --i-have-recovery
                    do
                        if ! dcent_toolbox_command_has_token \
                            "$_dcent_toolbox_command" "$_dcent_toolbox_required_arg"; then
                            echo "ERROR: AM2 install metadata omits required $_dcent_toolbox_required_arg gate" >&2
                            return 1
                        fi
                    done
                    ;;
            esac
            ;;
        *)
            echo "ERROR: unsupported toolbox install mode: $_dcent_toolbox_install_mode" >&2
            return 1
            ;;
    esac
    return 0
}

dcent_write_sysupgrade_manifest() {
    dcent_require_release_image_hardening
    dcent_release_provenance_init
    install_command="${DCENT_TOOLBOX_INSTALL_COMMAND:-dcent install <ip> -f dcentos-sysupgrade.tar}"
    update_command="${DCENT_TOOLBOX_UPDATE_COMMAND-$install_command}"
    upload_endpoint="${DCENT_TOOLBOX_UPLOAD_ENDPOINT:-null}"
    board_target_header="${DCENT_TOOLBOX_BOARD_TARGET_HEADER:-null}"
    requires_inactive_slot="${DCENT_TOOLBOX_REQUIRES_INACTIVE_SLOT:-true}"
    install_mode="${DCENT_TOOLBOX_INSTALL_MODE:-target_sysupgrade}"
    target_side_sysupgrade="${DCENT_TARGET_SIDE_SYSUPGRADE:-true}"
    package_status="${DCENT_PACKAGE_STATUS:-release}"
    manifest_profile=$(dcent_sysupgrade_manifest_profile) || exit 1
    dcent_require_toolbox_install_contract "$install_command" "$install_mode" || exit 1
    if [ -n "$update_command" ]; then
        dcent_require_toolbox_install_contract "$update_command" "$install_mode" || exit 1
    fi

    verification_block=""
    if [ "${DCENT_RELEASE_KEY_STAGED:-0}" = "1" ]; then
        verification_block=",
    \"verification_key\": {
      \"path\": \"sysupgrade-${BOARD_NAME}/release_ed25519.pub\",
      \"size\": ${DCENT_RELEASE_KEY_SIZE},
      \"sha256\": \"${DCENT_RELEASE_KEY_SHA256}\"
    }"
    fi

    extra_payload_block="${DCENT_EXTRA_PAYLOAD_BLOCK:-}"

    # Optional PSU-configuration declaration. Only emitted when the board's
    # post-image script sets DCENT_PSU_CONFIG_MODE (currently am2-s19jpro,
    # whose baked /etc/dcentrald/xil_override.toml has [power.psu_override]
    # enabled=true and a matching /etc/dcentos/psu_config). The toolbox XIL
    # install gate G5 reads this hint and refuses an install whose declared
    # --psu-config does not match. Absent for every other board (no behaviour
    # change), so a missing key keeps G5's legacy "loki" default for them.
    psu_config_mode_block=""
    if [ -n "${DCENT_PSU_CONFIG_MODE:-}" ]; then
        psu_config_mode_block="
  \"psu_config_mode\": \"${DCENT_PSU_CONFIG_MODE}\","
    fi

    cat > "$SUP_DIR/MANIFEST.json" <<EOF
{
  "schema": 1,
  "manifest_profile": "${manifest_profile}",
  "product": "DCENT_OS",
  "family": "antminer",
  "package_type": "sysupgrade",
  "installable": true,
  "artifact_maturity": "experimental",
  "board_family": "${BOARD_FAMILY}",
  "board": "${BOARD_NAME}",
  "board_target": "${BOARD_NAME}",
  "version": "${PACKAGE_VERSION}",
  "created_at_utc": "${DCENT_CREATED_AT_UTC}",
  "status": "${package_status}",${psu_config_mode_block}
  "provenance": {
    "source_commit": "${DCENT_SOURCE_COMMIT}",
    "source_tree_state": "${DCENT_SOURCE_TREE_STATE}",
    "source_date_epoch": ${SOURCE_DATE_EPOCH},
    "source_commit_epoch": ${DCENT_SOURCE_COMMIT_EPOCH},
    "build_target": "${DCENT_BUILD_TARGET}",
    "build_arch": "${DCENT_BUILD_ARCH}",
    "toolchain_id": "${DCENT_TOOLCHAIN_ID}"
  },
  "target_side_sysupgrade": ${target_side_sysupgrade},
  "payloads": {
    "kernel": {
      "path": "sysupgrade-${BOARD_NAME}/kernel",
      "size": ${KERNEL_SIZE},
      "sha256": "${KERNEL_SHA256}"
    },
    "rootfs": {
      "path": "sysupgrade-${BOARD_NAME}/root",
      "size": ${ROOTFS_SIZE},
      "sha256": "${ROOTFS_SHA256}"
    },
    "metadata": {
      "path": "sysupgrade-${BOARD_NAME}/METADATA",
      "size": ${METADATA_SIZE},
      "sha256": "${METADATA_SHA256}"
    }${verification_block}${extra_payload_block}
  },
  "toolbox": {
    "install_command": "${install_command}",
    "update_command": "${update_command}",
    "upload_endpoint": ${upload_endpoint},
    "board_target_header": ${board_target_header},
    "requires_inactive_slot": ${requires_inactive_slot},
    "install_mode": "${install_mode}",
    "target_side_sysupgrade": ${target_side_sysupgrade}
  }
}
EOF

    if [ "$manifest_profile" = "$DCENT_SYSUPGRADE_UNSIGNED_LAB_PROFILE" ]; then
        if grep -Eq '"(verification_key|ota_intermediate_cert|ota_revoked_intermediates)"[[:space:]]*:' "$SUP_DIR/MANIFEST.json"; then
            echo "ERROR: unsigned lab manifest contains forbidden authority material" >&2
            exit 1
        fi
        for forbidden_leaf in MANIFEST.sig release_ed25519.pub; do
            if [ -e "$SUP_DIR/$forbidden_leaf" ]; then
                echo "ERROR: unsigned lab manifest staging contains forbidden $forbidden_leaf" >&2
                exit 1
            fi
        done
    fi
}

dcent_sign_sysupgrade_manifest() {
    if [ "${DCENT_RELEASE_KEY_STAGED:-0}" != "1" ]; then
        return 0
    fi

    exact_signer="${PROJECT_ROOT:-}/scripts/sign_release_artifact.py"
    [ -n "${PROJECT_ROOT:-}" ] && [ -f "$exact_signer" ] || {
        echo "ERROR: exact release artifact signer is missing: $exact_signer" >&2
        exit 1
    }
    dcent_release_run_python "$exact_signer" "$SUP_DIR/MANIFEST.json" \
        --key "$DCENT_RELEASE_SIGNING_KEY" \
        --pubkey "$SUP_DIR/release_ed25519.pub" \
        --output-sig "$SUP_DIR/MANIFEST.sig" >/dev/null || {
        echo "ERROR: failed to sign exact MANIFEST.json bytes" >&2
        exit 1
    }

    echo "Signed MANIFEST.json"
}

# CE-204: canonical MANIFEST.json for an SD-card PAYLOAD tar (am3-bb / am3-bb-s19jpro).
# This is deliberately NOT a sysupgrade/NAND-installable package — package_type is
# "sdcard_payload" and nand_install is false so the SD-card honesty posture (AM3-BB
# NAND install disabled) is preserved. Do NOT reuse dcent_write_sysupgrade_manifest
# here (it hardcodes kernel/root payload paths that do not exist in an SD payload tar).
# Caller must set: SUP_DIR, BOARD_NAME, BOARD_FAMILY, PACKAGE_VERSION,
# DCENT_SDCARD_PAYLOAD_BLOCK (JSON payload entries, no surrounding braces),
# and DCENT_SDCARD_TAR_PREFIX (the tar's top-level dir name). Uses
# DCENT_RELEASE_KEY_STAGED/_SIZE/_SHA256 from dcent_stage_release_key, same as
# dcent_write_sysupgrade_manifest.
dcent_write_sdcard_payload_manifest() {
    dcent_release_provenance_init
    package_status="${DCENT_PACKAGE_STATUS:-release}"
    sdcard_tar_prefix="${DCENT_SDCARD_TAR_PREFIX:-dcentos-${BOARD_NAME}-sdcard}"

    verification_block=""
    if [ "${DCENT_RELEASE_KEY_STAGED:-0}" = "1" ]; then
        verification_block=",
    \"verification_key\": {
      \"path\": \"${sdcard_tar_prefix}/release_ed25519.pub\",
      \"size\": ${DCENT_RELEASE_KEY_SIZE},
      \"sha256\": \"${DCENT_RELEASE_KEY_SHA256}\"
    }"
    fi

    payload_block="${DCENT_SDCARD_PAYLOAD_BLOCK:-}"

    cat > "$SUP_DIR/MANIFEST.json" <<EOF
{
  "schema": 1,
  "product": "DCENT_OS",
  "family": "antminer",
  "package_type": "sdcard_payload",
  "board_family": "${BOARD_FAMILY}",
  "board": "${BOARD_NAME}",
  "board_target": "${BOARD_NAME}",
  "version": "${PACKAGE_VERSION}",
  "created_at_utc": "${DCENT_CREATED_AT_UTC}",
  "status": "${package_status}",
  "provenance": {
    "source_commit": "${DCENT_SOURCE_COMMIT}",
    "source_tree_state": "${DCENT_SOURCE_TREE_STATE}",
    "source_date_epoch": ${SOURCE_DATE_EPOCH},
    "source_commit_epoch": ${DCENT_SOURCE_COMMIT_EPOCH},
    "build_target": "${DCENT_BUILD_TARGET}",
    "build_arch": "${DCENT_BUILD_ARCH}",
    "toolchain_id": "${DCENT_TOOLCHAIN_ID}"
  },
  "nand_install": false,
  "payloads": {${payload_block}${verification_block}
  }
}
EOF
}
