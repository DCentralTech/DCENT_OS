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
#       defconfig (last-wins). This helper verifies the resulting /etc/shadow
#       entry before it stamps the marker, so alternate build entry points fail
#       closed unless they provide the same lock. NO default SSH password login
#       is possible. Operator SSH access is
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

dcent_release_path_exists() {
    [ -e "$1" ] || [ -L "$1" ]
}

dcent_release_require_directory() {
    _dcent_directory_path="$1"
    _dcent_directory_label="$2"
    if [ ! -d "$_dcent_directory_path" ] || [ -L "$_dcent_directory_path" ]; then
        echo "release_image_provision: ERROR: ${_dcent_directory_label} is not a real directory: ${_dcent_directory_path}" >&2
        return 1
    fi
}

dcent_release_remove_exact_entry() {
    _dcent_remove_path="$1"
    _dcent_remove_label="$2"
    if ! dcent_release_path_exists "$_dcent_remove_path"; then
        return 0
    fi
    if ! rm -f "$_dcent_remove_path" 2>/dev/null; then
        _dcent_remove_parent=${_dcent_remove_path%/*}
        _dcent_remove_name=${_dcent_remove_path##*/}
        _dcent_remove_attempt=0
        while [ "$_dcent_remove_attempt" -lt 32 ]; do
            _dcent_remove_quarantine="${_dcent_remove_parent}/.${_dcent_remove_name}.rejected.$$.${_dcent_remove_attempt}"
            if ! dcent_release_path_exists "$_dcent_remove_quarantine"; then
                if mv "$_dcent_remove_path" "$_dcent_remove_quarantine" 2>/dev/null; then
                    break
                fi
                echo "release_image_provision: ERROR: cannot quarantine ${_dcent_remove_label}: ${_dcent_remove_path}" >&2
                return 1
            fi
            _dcent_remove_attempt=$((_dcent_remove_attempt + 1))
        done
    fi
    if dcent_release_path_exists "$_dcent_remove_path"; then
        echo "release_image_provision: ERROR: ${_dcent_remove_label} survived removal: ${_dcent_remove_path}" >&2
        return 1
    fi
}

dcent_release_require_single_link_file() {
    _dcent_file_path="$1"
    _dcent_file_label="$2"
    if [ ! -f "$_dcent_file_path" ] || [ -L "$_dcent_file_path" ]; then
        echo "release_image_provision: ERROR: ${_dcent_file_label} is not a real regular file: ${_dcent_file_path}" >&2
        return 1
    fi
    _dcent_file_links="$(stat -c '%h' "$_dcent_file_path" 2>/dev/null)" || {
        echo "release_image_provision: ERROR: cannot inspect ${_dcent_file_label}: ${_dcent_file_path}" >&2
        return 1
    }
    if [ "$_dcent_file_links" != 1 ]; then
        echo "release_image_provision: ERROR: ${_dcent_file_label} has ${_dcent_file_links} filesystem links: ${_dcent_file_path}" >&2
        return 1
    fi
}

dcent_release_close_locked_root() {
    exec 9<&-
}

dcent_release_stat_signature() {
    stat -c '%d:%i:%h:%s:%f:%y:%z' "$1" 2>/dev/null
}

dcent_release_open_locked_root() {
    _dcent_shadow_path="$1"
    dcent_release_require_single_link_file "$_dcent_shadow_path" "/etc/shadow" || return 1
    _dcent_locked_root_path_signature="$(dcent_release_stat_signature "$_dcent_shadow_path")" || {
        echo "release_image_provision: ERROR: cannot inspect /etc/shadow identity" >&2
        return 1
    }
    if ! exec 9< "$_dcent_shadow_path"; then
        echo "release_image_provision: ERROR: cannot pin /etc/shadow" >&2
        return 1
    fi
    _dcent_locked_root_handle_signature="$(stat -L -c '%d:%i:%h:%s:%f:%y:%z' /proc/self/fd/9 2>/dev/null)" || {
        dcent_release_close_locked_root
        echo "release_image_provision: ERROR: cannot inspect pinned /etc/shadow" >&2
        return 1
    }
    _dcent_locked_root_handle_policy="$(stat -L -c '%F|%h|%a' /proc/self/fd/9 2>/dev/null)" || {
        dcent_release_close_locked_root
        echo "release_image_provision: ERROR: cannot inspect pinned /etc/shadow policy" >&2
        return 1
    }
    _dcent_locked_root_handle_type=${_dcent_locked_root_handle_policy%%|*}
    _dcent_locked_root_handle_rest=${_dcent_locked_root_handle_policy#*|}
    _dcent_locked_root_handle_links=${_dcent_locked_root_handle_rest%%|*}
    _dcent_locked_root_handle_mode=${_dcent_locked_root_handle_rest#*|}
    if [ "$_dcent_locked_root_handle_type" != "regular file" ] ||
       [ "$_dcent_locked_root_handle_links" != 1 ]; then
        dcent_release_close_locked_root
        echo "release_image_provision: ERROR: pinned /etc/shadow is not a single-link regular file" >&2
        return 1
    fi
    # Buildroot applies the final 0600 device-table mode under fakeroot after
    # post-build hooks, so a staging-tree 0644 is legitimate here. No group or
    # other write/execute bit is legitimate at either stage.
    case "$_dcent_locked_root_handle_mode" in
        4[04][04]|6[04][04]) ;;
        *)
            dcent_release_close_locked_root
            echo "release_image_provision: ERROR: pinned /etc/shadow has unsafe staging mode ${_dcent_locked_root_handle_mode}" >&2
            return 1
            ;;
    esac
    if [ "$_dcent_locked_root_handle_signature" != "$_dcent_locked_root_path_signature" ]; then
        dcent_release_close_locked_root
        echo "release_image_provision: ERROR: /etc/shadow changed while it was pinned" >&2
        return 1
    fi
    _dcent_root_record="$(awk -F: '
        $1 == "root" { count += 1; password = $2 }
        END { printf "%d|%s", count + 0, password }
    ' <&9)" || {
        dcent_release_close_locked_root
        echo "release_image_provision: ERROR: cannot parse pinned /etc/shadow" >&2
        return 1
    }
    _dcent_root_count=${_dcent_root_record%%|*}
    _dcent_root_hash=${_dcent_root_record#*|}
    _dcent_locked_root_after_read="$(dcent_release_stat_signature "$_dcent_shadow_path")" || {
        dcent_release_close_locked_root
        echo "release_image_provision: ERROR: cannot recheck /etc/shadow after reading" >&2
        return 1
    }
    _dcent_locked_handle_after_read="$(stat -L -c '%d:%i:%h:%s:%f:%y:%z' /proc/self/fd/9 2>/dev/null)" || {
        dcent_release_close_locked_root
        echo "release_image_provision: ERROR: cannot recheck pinned /etc/shadow" >&2
        return 1
    }
    if [ "$_dcent_locked_root_after_read" != "$_dcent_locked_root_path_signature" ] ||
       [ "$_dcent_locked_handle_after_read" != "$_dcent_locked_root_path_signature" ]; then
        dcent_release_close_locked_root
        echo "release_image_provision: ERROR: /etc/shadow changed while it was read" >&2
        return 1
    fi
    if [ "$_dcent_root_count" != 1 ]; then
        dcent_release_close_locked_root
        echo "release_image_provision: ERROR: /etc/shadow must contain exactly one root account" >&2
        return 1
    fi
    case "$_dcent_root_hash" in
        \!*|\**) ;;
        *)
            dcent_release_close_locked_root
            echo "release_image_provision: ERROR: release root account is not locked in /etc/shadow" >&2
            return 1
            ;;
    esac
    _dcent_locked_root_path="$_dcent_shadow_path"
}

dcent_release_recheck_locked_root() {
    _dcent_locked_root_final_path="$(dcent_release_stat_signature "$_dcent_locked_root_path")" || return 1
    _dcent_locked_root_final_handle="$(stat -L -c '%d:%i:%h:%s:%f:%y:%z' /proc/self/fd/9 2>/dev/null)" || return 1
    [ "$_dcent_locked_root_final_path" = "$_dcent_locked_root_path_signature" ] &&
        [ "$_dcent_locked_root_final_handle" = "$_dcent_locked_root_path_signature" ]
}

dcent_release_abort_marker() {
    _dcent_abort_marker="$1"
    _dcent_abort_message="$2"
    _dcent_abort_cleanup=ok
    if ! dcent_release_remove_exact_entry "$_dcent_abort_marker" "failed release-image marker" >/dev/null 2>&1; then
        _dcent_abort_cleanup=failed
    fi
    dcent_release_close_locked_root
    echo "release_image_provision: ERROR: ${_dcent_abort_message}" >&2
    if [ "$_dcent_abort_cleanup" != ok ]; then
        echo "release_image_provision: ERROR: failed release-image marker could not be retired: ${_dcent_abort_marker}" >&2
    fi
    return 1
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
    dcent_release_require_directory "$_dcent_target_dir" "TARGET_DIR" || return 1

    if ! dcent_release_image_truthy "${DCENT_RELEASE_IMAGE:-0}"; then
        # DEV/LAB image — keep everything byte-identical to today. Defensive:
        # if a stale marker somehow exists in an overlay, remove it so a dev
        # build can never silently inherit the stricter release posture.
        _dcent_etc_dir="${_dcent_target_dir}/etc"
        _dcent_config_dir="${_dcent_etc_dir}/dcentos"
        _dcent_marker="${_dcent_config_dir}/release-image"
        if dcent_release_path_exists "$_dcent_etc_dir"; then
            dcent_release_require_directory "$_dcent_etc_dir" "rootfs /etc" || return 1
        fi
        if dcent_release_path_exists "$_dcent_config_dir"; then
            dcent_release_require_directory "$_dcent_config_dir" "rootfs /etc/dcentos" || return 1
        fi
        if dcent_release_path_exists "$_dcent_marker"; then
            dcent_release_remove_exact_entry "$_dcent_marker" "stray release-image marker" || return 1
            echo "DCENTos post-build (${_dcent_label}): removed stray release-image marker (this is a DEV/LAB build)"
        fi
        echo "DCENTos post-build (${_dcent_label}): DEV/LAB image (DCENT_RELEASE_IMAGE unset) — root:dcentral SSH + passwordless opt-out preserved"
        return 0
    fi

    # ---- PRODUCTION/RELEASE image ----
    _dcent_etc_dir="${_dcent_target_dir}/etc"
    _dcent_config_dir="${_dcent_etc_dir}/dcentos"
    _dcent_marker="${_dcent_config_dir}/release-image"
    _dcent_grace="${_dcent_config_dir}/first-boot-grace"
    dcent_release_require_directory "$_dcent_etc_dir" "rootfs /etc" || return 1
    if dcent_release_path_exists "$_dcent_config_dir"; then
        dcent_release_require_directory "$_dcent_config_dir" "rootfs /etc/dcentos" || return 1
    elif ! mkdir "$_dcent_config_dir"; then
        echo "release_image_provision: ERROR: cannot create rootfs /etc/dcentos" >&2
        return 1
    fi
    # No failed release preflight may leave a stale runtime release claim.
    dcent_release_remove_exact_entry "$_dcent_marker" "prior release-image marker" || return 1
    dcent_release_open_locked_root "${_dcent_etc_dir}/shadow" || return 1

    # 1. First-boot SSH credential posture. The root account is locked at the
    #    defconfig layer (BR2_TARGET_GENERIC_ROOT_PASSWD="*"), so NO default
    #    SSH password login is possible. The existing S50dropbear lockdown
    #    already requires a first-boot credential (wizard Argon2id password OR
    #    an uploaded authorized_keys) before SSH comes up. On a release image
    #    we additionally strip any build-time-baked first-boot-grace marker so
    #    SSH can never auto-open on a fresh release unit before a credential
    #    exists — the operator MUST provision a credential first.
    if dcent_release_path_exists "$_dcent_grace"; then
        if ! dcent_release_remove_exact_entry "$_dcent_grace" "first-boot-grace marker"; then
            dcent_release_abort_marker "$_dcent_marker" "cannot retire first-boot-grace marker"
            return 1
        fi
        echo "DCENTos post-build (${_dcent_label}): release image — removed first-boot-grace marker (no auto-SSH before a credential exists)"
    fi

    # 2. Publish the runtime posture marker only after every prerequisite has
    #    passed. Unlinking the exact old name first is safe for symlinks and
    #    hardlinks: neither target inode is opened or rewritten.
    if ! _dcent_marker_tmp="$(mktemp "${_dcent_config_dir}/.release-image.tmp.XXXXXX")"; then
        dcent_release_abort_marker "$_dcent_marker" "cannot allocate release-image marker temporary"
        return 1
    fi
    if ! cat > "$_dcent_marker_tmp" <<'EOF'
# DCENT_OS PRODUCTION/RELEASE image marker.
# Presence => dashboard/API require a password; the freedom-first
# passwordless opt-out is DISABLED and root SSH password login is
# locked. Built with DCENT_RELEASE_IMAGE=1. Do not hand-create.
release_image=1
EOF
    then
        rm -f "$_dcent_marker_tmp"
        dcent_release_abort_marker "$_dcent_marker" "cannot write release-image marker"
        return 1
    fi
    if ! chmod 644 "$_dcent_marker_tmp"; then
        rm -f "$_dcent_marker_tmp"
        dcent_release_abort_marker "$_dcent_marker" "cannot prepare release-image marker"
        return 1
    fi
    if ! mv -f "$_dcent_marker_tmp" "$_dcent_marker"; then
        rm -f "$_dcent_marker_tmp"
        dcent_release_abort_marker "$_dcent_marker" "cannot publish release-image marker"
        return 1
    fi
    if ! dcent_release_require_single_link_file "$_dcent_marker" "release-image marker"; then
        dcent_release_abort_marker "$_dcent_marker" "published release-image marker failed identity verification"
        return 1
    fi
    if ! _dcent_marker_mode="$(stat -c '%a' "$_dcent_marker" 2>/dev/null)"; then
        dcent_release_abort_marker "$_dcent_marker" "cannot inspect release-image marker mode"
        return 1
    fi
    if ! _dcent_marker_lines="$(wc -l < "$_dcent_marker" 2>/dev/null)"; then
        dcent_release_abort_marker "$_dcent_marker" "cannot inspect release-image marker contents"
        return 1
    fi
    if [ "$_dcent_marker_mode" != 644 ] || [ "$_dcent_marker_lines" -ne 5 ] || ! grep -Fx 'release_image=1' "$_dcent_marker" >/dev/null 2>&1; then
        dcent_release_abort_marker "$_dcent_marker" "published release-image marker is not canonical"
        return 1
    fi
    if ! dcent_release_recheck_locked_root; then
        dcent_release_abort_marker "$_dcent_marker" "/etc/shadow changed before release-image publication completed"
        return 1
    fi
    dcent_release_close_locked_root

    echo "DCENTos post-build (${_dcent_label}): PRODUCTION/RELEASE image — stamped /etc/dcentos/release-image after verifying the locked root shadow entry; dashboard/API password REQUIRED (opt-out disabled)"
    return 0
}
