#!/bin/sh
#
# Shared Buildroot post-build helper: PRODUCTION/RELEASE image trust-boundary
# provisioning (production-readiness matrix §7 #1, the top release blocker).
#
# D-Central Technologies, 2026.
#
# The DCENT_OS public-image trust boundary is split into TWO image postures,
# selected at Buildroot time by the DCENT_RELEASE_IMAGE build flag:
#
#   DEV/LAB image  (default; DCENT_RELEASE_IMAGE unset/0):
#     * root password stays "dcentral" (the shared dcentos-common.fragment
#       BR2_TARGET_GENERIC_ROOT_PASSWD), so the operator's ssh_cmd.js / fleet
#       tooling keeps working unchanged.
#     * the dashboard/API "freedom-first" passwordless opt-out
#       (/api/setup/skip-password, /api/setup/skip-safety) still works.
#     * NO /etc/dcentos/release-image marker is stamped.
#     => this helper is a NO-OP. The dev/lab rootfs is byte-identical to today.
#
#   PRODUCTION/RELEASE image (DCENT_RELEASE_IMAGE=1):
#     * the root account is LOCKED at the defconfig layer — build_in_docker.sh
#       appends BR2_TARGET_GENERIC_ROOT_PASSWD="*" AFTER the per-product
#       defconfig (last-wins) so /etc/shadow ships root with a "*" hash and
#       NO default SSH password login is possible. Operator SSH access is
#       provisioned on first boot (dashboard wizard sets the Argon2id
#       password + stamps /data/dcent/.ssh-enabled; an authorized_keys upload
#       does the same), gated by the existing S50dropbear lockdown.
#     * this helper stamps /etc/dcentos/release-image into the rootfs. dcentrald
#       reads that marker (auth.rs::is_release_image) and DISABLES the
#       passwordless opt-out: a release unit cannot run passwordless.
#     * this helper also strips any baked first-boot grace marker so a release
#       image never auto-opens SSH before a credential exists.
#
# Usage (sourced near the end of each board post-build.sh):
#     . "${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/lib/release_image_provision.sh"
#     dcent_provision_release_image "$TARGET_DIR" "<board-label>"
#
# POSIX sh only (BusyBox ash / Buildroot host sh). No bashisms.

dcent_release_image_truthy() {
    case "${1:-}" in
        1|true|TRUE|yes|YES|y|Y) return 0 ;;
        *) return 1 ;;
    esac
}

# dcent_provision_release_image TARGET_DIR LABEL
#
# Stamps the release-image marker + tightens the first-boot SSH posture when
# DCENT_RELEASE_IMAGE is truthy. No-op otherwise (dev/lab byte-identical).
dcent_provision_release_image() {
    _dcent_target_dir="$1"
    _dcent_label="${2:-unknown}"

    if [ -z "$_dcent_target_dir" ]; then
        echo "release_image_provision: ERROR: TARGET_DIR not supplied" >&2
        return 1
    fi

    if ! dcent_release_image_truthy "${DCENT_RELEASE_IMAGE:-0}"; then
        # DEV/LAB image — keep everything byte-identical to today. Defensive:
        # if a stale marker somehow exists in an overlay, remove it so a dev
        # build can never silently inherit the stricter release posture.
        if [ -f "${_dcent_target_dir}/etc/dcentos/release-image" ]; then
            rm -f "${_dcent_target_dir}/etc/dcentos/release-image" 2>/dev/null || true
            echo "DCENTos post-build (${_dcent_label}): removed stray release-image marker (this is a DEV/LAB build)"
        fi
        echo "DCENTos post-build (${_dcent_label}): DEV/LAB image (DCENT_RELEASE_IMAGE unset) — root:dcentral SSH + passwordless opt-out preserved"
        return 0
    fi

    # ---- PRODUCTION/RELEASE image ----
    mkdir -p "${_dcent_target_dir}/etc/dcentos"

    # 1. Runtime posture marker consumed by dcentrald-api::auth::is_release_image.
    #    Its presence flips the API into "password required, opt-out disabled".
    {
        echo "# DCENT_OS PRODUCTION/RELEASE image marker."
        echo "# Presence => dashboard/API require a password; the freedom-first"
        echo "# passwordless opt-out is DISABLED and root SSH password login is"
        echo "# locked. Built with DCENT_RELEASE_IMAGE=1. Do not hand-create."
        echo "release_image=1"
    } > "${_dcent_target_dir}/etc/dcentos/release-image"
    chmod 644 "${_dcent_target_dir}/etc/dcentos/release-image"

    # 2. First-boot SSH credential posture. The root account is locked at the
    #    defconfig layer (BR2_TARGET_GENERIC_ROOT_PASSWD="*"), so NO default
    #    SSH password login is possible. The existing S50dropbear lockdown
    #    already requires a first-boot credential (wizard Argon2id password OR
    #    an uploaded authorized_keys) before SSH comes up. On a release image
    #    we additionally strip any build-time-baked first-boot-grace marker so
    #    SSH can never auto-open on a fresh release unit before a credential
    #    exists — the operator MUST provision a credential first.
    if [ -f "${_dcent_target_dir}/etc/dcentos/first-boot-grace" ]; then
        rm -f "${_dcent_target_dir}/etc/dcentos/first-boot-grace" 2>/dev/null || true
        echo "DCENTos post-build (${_dcent_label}): release image — removed first-boot-grace marker (no auto-SSH before a credential exists)"
    fi

    echo "DCENTos post-build (${_dcent_label}): PRODUCTION/RELEASE image — stamped /etc/dcentos/release-image; root SSH password login locked at defconfig (BR2_TARGET_GENERIC_ROOT_PASSWD=*); dashboard/API password REQUIRED (opt-out disabled)"
    return 0
}
