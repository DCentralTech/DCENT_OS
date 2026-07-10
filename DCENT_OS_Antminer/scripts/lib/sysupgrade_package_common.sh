#!/bin/sh
#
# Shared manifest/signature helpers for Buildroot board post-image scripts.

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
        echo "       (defconfig root-lock + /etc/dcentos/release-image marker). Use 'make release'," >&2
        echo "       or set DCENT_PACKAGE_STATUS to a non-release lab value (e.g. lab_signed)." >&2
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

dcent_stage_release_key() {
    DCENT_RELEASE_KEY_STAGED=0
    DCENT_RELEASE_KEY_SIZE=""
    DCENT_RELEASE_KEY_SHA256=""

    if [ -z "${DCENT_RELEASE_SIGNING_KEY:-}" ]; then
        if [ "${DCENT_REQUIRE_RELEASE_KEY:-0}" = "1" ]; then
            echo "ERROR: DCENT_REQUIRE_RELEASE_KEY=1 but DCENT_RELEASE_SIGNING_KEY is unset" >&2
            exit 1
        fi
        dcent_require_unsigned_lab_override "unsigned package generation"
        echo "WARNING: no signing key configured - package is unsigned (lab-only)"
        return 0
    fi

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
        if [ "${DCENT_REQUIRE_RELEASE_KEY:-0}" = "1" ]; then
            echo "ERROR: DCENT_REQUIRE_RELEASE_KEY=1 but DCENT_RELEASE_PUBKEY_FILE is unset" >&2
            exit 1
        fi
        if dcent_is_release_status "${DCENT_PACKAGE_STATUS:-release}"; then
            echo "ERROR: production release signing requires DCENT_RELEASE_PUBKEY_FILE; refusing self-derived generated-key package" >&2
            exit 1
        fi
        dcent_require_unsigned_lab_override "self-derived generated-key package generation"
        openssl pkey -in "$DCENT_RELEASE_SIGNING_KEY" -pubout -out "$SUP_DIR/release_ed25519.pub" >/dev/null 2>&1 || {
            echo "ERROR: failed to derive release_ed25519.pub from signing key" >&2
            exit 1
        }
        echo "WARNING: derived release_ed25519.pub from signing key - generated-key package is lab-only"
    fi

    DCENT_RELEASE_KEY_SIZE=$(stat -c%s "$SUP_DIR/release_ed25519.pub" 2>/dev/null || stat -f%z "$SUP_DIR/release_ed25519.pub")
    DCENT_RELEASE_KEY_SHA256=$(sha256sum "$SUP_DIR/release_ed25519.pub" | awk '{print $1}')
    echo "${DCENT_RELEASE_KEY_SHA256}  release_ed25519.pub" >> "$SUP_DIR/SHA256SUMS"
    DCENT_RELEASE_KEY_STAGED=1
}

dcent_write_sysupgrade_manifest() {
    dcent_require_release_image_hardening
    install_command="${DCENT_TOOLBOX_INSTALL_COMMAND:-dcent install <ip> -f dcentos-sysupgrade.tar}"
    update_command="${DCENT_TOOLBOX_UPDATE_COMMAND-$install_command}"
    upload_endpoint="${DCENT_TOOLBOX_UPLOAD_ENDPOINT:-null}"
    board_target_header="${DCENT_TOOLBOX_BOARD_TARGET_HEADER:-null}"
    requires_inactive_slot="${DCENT_TOOLBOX_REQUIRES_INACTIVE_SLOT:-true}"
    install_mode="${DCENT_TOOLBOX_INSTALL_MODE:-target_sysupgrade}"
    target_side_sysupgrade="${DCENT_TARGET_SIDE_SYSUPGRADE:-true}"
    package_status="${DCENT_PACKAGE_STATUS:-release}"

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
  "product": "DCENT_OS",
  "family": "antminer",
  "package_type": "sysupgrade",
  "board_family": "${BOARD_FAMILY}",
  "board": "${BOARD_NAME}",
  "board_target": "${BOARD_NAME}",
  "version": "${PACKAGE_VERSION}",
  "created_at_utc": "$(date -u +"%Y-%m-%dT%H:%M:%SZ")",
  "status": "${package_status}",${psu_config_mode_block}
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
}

dcent_sign_sysupgrade_manifest() {
    if [ "${DCENT_RELEASE_KEY_STAGED:-0}" != "1" ]; then
        return 0
    fi

    openssl pkeyutl -sign -rawin \
        -inkey "$DCENT_RELEASE_SIGNING_KEY" \
        -in "$SUP_DIR/MANIFEST.json" \
        -out "$SUP_DIR/MANIFEST.sig" || {
            echo "ERROR: failed to sign MANIFEST.json" >&2
            exit 1
        }

    openssl pkeyutl -verify -rawin -pubin \
        -inkey "$SUP_DIR/release_ed25519.pub" \
        -sigfile "$SUP_DIR/MANIFEST.sig" \
        -in "$SUP_DIR/MANIFEST.json" >/dev/null || {
            echo "ERROR: MANIFEST.sig verification failed against release_ed25519.pub" >&2
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
  "created_at_utc": "$(date -u +"%Y-%m-%dT%H:%M:%SZ")",
  "status": "${package_status}",
  "nand_install": false,
  "payloads": {${payload_block}${verification_block}
  }
}
EOF
}
