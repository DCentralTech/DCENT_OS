#!/bin/sh
#
# Canonical source/provenance and archive helpers for signed release envelopes.
# This contract makes the envelope reproducible. It does not imply that the
# kernel or root filesystem payloads are themselves reproducible.

dcent_release_is_truthy() {
    case "${1:-}" in
        1|true|TRUE|yes|YES|y|Y) return 0 ;;
        *) return 1 ;;
    esac
}

dcent_release_is_release_status() {
    case "${1:-release}" in
        release|production|stable) return 0 ;;
        *) return 1 ;;
    esac
}

dcent_release_provenance_required() {
    dcent_release_is_release_status "${DCENT_PACKAGE_STATUS:-release}" ||
        dcent_release_is_truthy "${DCENT_RELEASE_IMAGE:-0}" ||
        dcent_release_is_truthy "${DCENT_REQUIRE_RELEASE_PROVENANCE:-0}"
}

dcent_release_error() {
    echo "ERROR: release envelope provenance: $*" >&2
    return 1
}

# Remove the complete exact-name publication set for one release invocation.
# Call before building and from every failed EXIT path so a late signer or
# private-stage cleanup failure cannot leave a canonical alias plus release
# metadata from either the current or a prior same-name run.
dcent_release_remove_publication() {
    dcent_release_publication_dir="$1"
    dcent_release_publication_name="$2"
    dcent_release_publication_ext="$3"

    [ -d "$dcent_release_publication_dir" ] ||
        dcent_release_error "release publication directory is missing" || return 1
    case "$dcent_release_publication_name" in
        ""|.|..|*/*|*\\*)
            dcent_release_error "release publication name is not a flat basename" || return 1
            ;;
    esac
    case "$dcent_release_publication_ext" in
        tar|img) ;;
        *) dcent_release_error "release publication extension is invalid" || return 1 ;;
    esac
    rm -f -- \
        "$dcent_release_publication_dir/${dcent_release_publication_name}.${dcent_release_publication_ext}" \
        "$dcent_release_publication_dir/${dcent_release_publication_name}.${dcent_release_publication_ext}.sig" \
        "$dcent_release_publication_dir/${dcent_release_publication_name}-LAB-UNSIGNED-NOT-FOR-RELEASE.${dcent_release_publication_ext}" \
        "$dcent_release_publication_dir/${dcent_release_publication_name}-LAB-UNSIGNED-NOT-FOR-RELEASE.${dcent_release_publication_ext}.sig" \
        "$dcent_release_publication_dir/${dcent_release_publication_name}.release.txt"
}

dcent_release_require_publication_absent() {
    dcent_release_publication_dir="$1"
    dcent_release_publication_name="$2"
    dcent_release_publication_ext="$3"
    for dcent_release_candidate in \
        "$dcent_release_publication_dir/${dcent_release_publication_name}.${dcent_release_publication_ext}" \
        "$dcent_release_publication_dir/${dcent_release_publication_name}.${dcent_release_publication_ext}.sig" \
        "$dcent_release_publication_dir/${dcent_release_publication_name}-LAB-UNSIGNED-NOT-FOR-RELEASE.${dcent_release_publication_ext}" \
        "$dcent_release_publication_dir/${dcent_release_publication_name}-LAB-UNSIGNED-NOT-FOR-RELEASE.${dcent_release_publication_ext}.sig" \
        "$dcent_release_publication_dir/${dcent_release_publication_name}.release.txt"; do
        if [ -e "$dcent_release_candidate" ] || [ -L "$dcent_release_candidate" ]; then
            dcent_release_error \
                "canonical publication already exists; archive it or choose a new source/channel: $dcent_release_candidate" || return 1
        fi
    done
}

# Serialize the global Buildroot work volume and shared output namespace. The
# directory creation is the cross-platform atomic admission operation; callers
# hold it for the complete build and release it from their EXIT trap.
dcent_release_build_lock_acquire() {
    dcent_release_lock_dir="$1"
    if ! mkdir -- "$dcent_release_lock_dir" 2>/dev/null; then
        dcent_release_error \
            "another build owns the shared build/output lock: $dcent_release_lock_dir" || return 1
    fi
    chmod 0700 "$dcent_release_lock_dir" || {
        rmdir -- "$dcent_release_lock_dir" 2>/dev/null || true
        dcent_release_error "failed to make build lock private" || return 1
    }
}

dcent_release_build_lock_release() {
    dcent_release_lock_dir="$1"
    [ ! -L "$dcent_release_lock_dir" ] ||
        dcent_release_error "build lock was replaced by a symlink" || return 1
    [ -d "$dcent_release_lock_dir" ] ||
        dcent_release_error "build lock directory is missing" || return 1
    rmdir -- "$dcent_release_lock_dir" ||
        dcent_release_error "build lock directory is not empty or removable" || return 1
}

dcent_release_validate_identifier() {
    dcent_release_identifier_name="$1"
    dcent_release_identifier_value="$2"
    [ -n "$dcent_release_identifier_value" ] ||
        dcent_release_error "$dcent_release_identifier_name is missing" || return 1
    case "$dcent_release_identifier_value" in
        *[!A-Za-z0-9._+:/@-]*)
            dcent_release_error "$dcent_release_identifier_name contains non-canonical characters" || return 1
            ;;
    esac
}

dcent_release_epoch_to_utc() {
    dcent_release_epoch="$1"
    if date -u -d "@${dcent_release_epoch}" '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null; then
        return 0
    fi
    if date -u -r "$dcent_release_epoch" '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null; then
        return 0
    fi
    dcent_release_error "SOURCE_DATE_EPOCH cannot be represented by the host date implementation"
}

dcent_release_provenance_init() {
    dcent_release_required=0
    dcent_release_provenance_required && dcent_release_required=1

    if [ "$dcent_release_required" = "0" ]; then
        : "${SOURCE_DATE_EPOCH:=0}"
        : "${DCENT_SOURCE_COMMIT:=unbound}"
        : "${DCENT_SOURCE_COMMIT_EPOCH:=$SOURCE_DATE_EPOCH}"
        : "${DCENT_SOURCE_TREE_STATE:=unbound}"
        : "${DCENT_BUILD_TARGET:=unknown}"
        : "${DCENT_BUILD_ARCH:=unknown}"
        : "${DCENT_TOOLCHAIN_ID:=unknown}"
    fi

    [ -n "${SOURCE_DATE_EPOCH:-}" ] ||
        dcent_release_error "SOURCE_DATE_EPOCH is required" || return 1
    case "$SOURCE_DATE_EPOCH" in
        *[!0-9]*) dcent_release_error "SOURCE_DATE_EPOCH must be an unsigned integer" || return 1 ;;
    esac

    [ -n "${DCENT_SOURCE_COMMIT_EPOCH:-}" ] ||
        dcent_release_error "DCENT_SOURCE_COMMIT_EPOCH is required" || return 1
    case "$DCENT_SOURCE_COMMIT_EPOCH" in
        *[!0-9]*) dcent_release_error "DCENT_SOURCE_COMMIT_EPOCH must be an unsigned integer" || return 1 ;;
    esac
    [ "$SOURCE_DATE_EPOCH" = "$DCENT_SOURCE_COMMIT_EPOCH" ] ||
        dcent_release_error "SOURCE_DATE_EPOCH must equal the source commit epoch" || return 1

    if [ "$dcent_release_required" = "1" ]; then
        case "${DCENT_SOURCE_COMMIT:-}" in
            [0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F]*) ;;
            *) dcent_release_error "DCENT_SOURCE_COMMIT must be a hexadecimal object id" || return 1 ;;
        esac
        case "${DCENT_SOURCE_COMMIT:-}" in
            *[!0-9a-fA-F]*|'') dcent_release_error "DCENT_SOURCE_COMMIT must be hexadecimal" || return 1 ;;
        esac
        dcent_release_commit_length=${#DCENT_SOURCE_COMMIT}
        [ "$dcent_release_commit_length" = "40" ] || [ "$dcent_release_commit_length" = "64" ] ||
            dcent_release_error "DCENT_SOURCE_COMMIT must be a full 40- or 64-character object id" || return 1
        case "${DCENT_SOURCE_TREE_STATE:-}" in
            clean|exact_git_object_snapshot) ;;
            *)
                dcent_release_error \
                    "release provenance requires DCENT_SOURCE_TREE_STATE=clean or exact_git_object_snapshot" || return 1
                ;;
        esac
    else
        case "${DCENT_SOURCE_COMMIT:-}" in
            unbound) ;;
            *[!0-9a-fA-F]*|'') dcent_release_error "DCENT_SOURCE_COMMIT must be hexadecimal or unbound" || return 1 ;;
        esac
        case "${DCENT_SOURCE_TREE_STATE:-}" in
            clean|dirty|unbound|exact_git_object_snapshot) ;;
            *) dcent_release_error "DCENT_SOURCE_TREE_STATE must be clean, dirty, unbound, or exact_git_object_snapshot" || return 1 ;;
        esac
    fi

    dcent_release_validate_identifier DCENT_BUILD_TARGET "${DCENT_BUILD_TARGET:-}" || return 1
    dcent_release_validate_identifier DCENT_BUILD_ARCH "${DCENT_BUILD_ARCH:-}" || return 1
    dcent_release_validate_identifier DCENT_TOOLCHAIN_ID "${DCENT_TOOLCHAIN_ID:-}" || return 1

    DCENT_CREATED_AT_UTC=$(dcent_release_epoch_to_utc "$SOURCE_DATE_EPOCH") || return 1
    export SOURCE_DATE_EPOCH DCENT_SOURCE_COMMIT DCENT_SOURCE_COMMIT_EPOCH
    export DCENT_SOURCE_TREE_STATE DCENT_BUILD_TARGET DCENT_BUILD_ARCH
    export DCENT_TOOLCHAIN_ID DCENT_CREATED_AT_UTC
}

# Bind a build to the checked-out source tree. Required/release builds reject
# dirty trees and caller-supplied provenance that disagrees with Git.
dcent_prepare_git_release_provenance() {
    dcent_release_repo="$1"
    DCENT_PACKAGE_STATUS="${2:-${DCENT_PACKAGE_STATUS:-release}}"
    DCENT_BUILD_TARGET="$3"
    DCENT_BUILD_ARCH="$4"
    DCENT_TOOLCHAIN_ID="$5"

    command -v git >/dev/null 2>&1 || dcent_release_error "git is required" || return 1
    git -C "$dcent_release_repo" rev-parse --is-inside-work-tree >/dev/null 2>&1 ||
        dcent_release_error "$dcent_release_repo is not a Git worktree" || return 1

    dcent_release_git_commit=$(git -C "$dcent_release_repo" rev-parse HEAD) || return 1
    dcent_release_git_epoch=$(git -C "$dcent_release_repo" show -s --format=%ct HEAD) || return 1
    dcent_release_git_state=clean
    if ! git -C "$dcent_release_repo" diff --quiet --ignore-submodules -- ||
       ! git -C "$dcent_release_repo" diff --cached --quiet --ignore-submodules -- ||
       [ -n "$(git -C "$dcent_release_repo" ls-files --others --exclude-standard | sed -n '1p')" ]; then
        dcent_release_git_state=dirty
    fi

    [ -z "${DCENT_SOURCE_COMMIT:-}" ] || [ "$DCENT_SOURCE_COMMIT" = "$dcent_release_git_commit" ] ||
        dcent_release_error "DCENT_SOURCE_COMMIT disagrees with Git HEAD" || return 1
    [ -z "${SOURCE_DATE_EPOCH:-}" ] || [ "$SOURCE_DATE_EPOCH" = "$dcent_release_git_epoch" ] ||
        dcent_release_error "SOURCE_DATE_EPOCH disagrees with the Git commit epoch" || return 1
    [ -z "${DCENT_SOURCE_COMMIT_EPOCH:-}" ] || [ "$DCENT_SOURCE_COMMIT_EPOCH" = "$dcent_release_git_epoch" ] ||
        dcent_release_error "DCENT_SOURCE_COMMIT_EPOCH disagrees with Git HEAD" || return 1
    [ -z "${DCENT_SOURCE_TREE_STATE:-}" ] || [ "$DCENT_SOURCE_TREE_STATE" = "$dcent_release_git_state" ] ||
        dcent_release_error "DCENT_SOURCE_TREE_STATE disagrees with the worktree" || return 1

    DCENT_SOURCE_COMMIT="$dcent_release_git_commit"
    SOURCE_DATE_EPOCH="$dcent_release_git_epoch"
    DCENT_SOURCE_COMMIT_EPOCH="$dcent_release_git_epoch"
    DCENT_SOURCE_TREE_STATE="$dcent_release_git_state"
    export DCENT_PACKAGE_STATUS DCENT_REQUIRE_RELEASE_PROVENANCE
    dcent_release_provenance_init
}

# Produce a canonical POSIX ustar archive: stable member order, timestamps,
# numeric ownership and modes, with no implementation-specific pax headers.
dcent_create_deterministic_tar() {
    dcent_release_output="$1"
    dcent_release_base="$2"
    dcent_release_top="$3"

    dcent_release_provenance_init || return 1
    command -v tar >/dev/null 2>&1 || dcent_release_error "tar is required" || return 1
    tar --help 2>/dev/null | grep -q -- '--sort' ||
        dcent_release_error "GNU tar with --sort=name is required" || return 1
    [ -d "$dcent_release_base/$dcent_release_top" ] ||
        dcent_release_error "archive root is missing: $dcent_release_base/$dcent_release_top" || return 1

    if ! dcent_release_unsafe_entry=$(find "$dcent_release_base/$dcent_release_top" \
        ! -type f ! -type d -print -quit); then
        dcent_release_error "failed to inspect archive member types" || return 1
    fi
    [ -z "$dcent_release_unsafe_entry" ] ||
        dcent_release_error "unsupported archive member type: $dcent_release_unsafe_entry" || return 1

    # GNU tar can encode multiply-linked regular files as hardlink members,
    # which the OTA parser intentionally refuses. Reject them at the producer.
    if ! dcent_release_hardlinked_entry=$(find "$dcent_release_base/$dcent_release_top" \
        -type f -links +1 -print -quit); then
        dcent_release_error "failed to inspect archive link counts" || return 1
    fi
    [ -z "$dcent_release_hardlinked_entry" ] ||
        dcent_release_error "multiply-linked archive member is forbidden: $dcent_release_hardlinked_entry" || return 1

    find "$dcent_release_base/$dcent_release_top" -type d -exec chmod 0755 {} + ||
        dcent_release_error "failed to normalize archive directory modes" || return 1
    find "$dcent_release_base/$dcent_release_top" -type f -exec chmod 0644 {} + ||
        dcent_release_error "failed to normalize archive file modes" || return 1
    LC_ALL=C TZ=UTC tar \
        --sort=name \
        --format=ustar \
        --mtime="@${SOURCE_DATE_EPOCH}" \
        --owner=0 --group=0 --numeric-owner \
        --mode='u+rwX,go+rX,go-w' \
        -cf "$dcent_release_output" \
        -C "$dcent_release_base" "$dcent_release_top"
}
