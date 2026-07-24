#!/bin/sh
# Canonical pre-extraction admission policy for DCENT_OS sysupgrade archives.
#
# This helper is sourced by host release tooling and installed into Zynq
# images for the target-side sysupgrade consumers.  It deliberately validates
# the archive envelope before callers extract the package or verify its
# signature.  Cryptographic authorization remains the caller's responsibility.

DCENT_SYSUPGRADE_ARCHIVE_MAX_MEMBERS=32
DCENT_SYSUPGRADE_MANIFEST_MAX_BYTES=65536

dcent_sysupgrade_archive_admit() (
    dcent_archive_path=${1:-}
    dcent_expected_board=${2:-}
    dcent_archive_scratch=${3:-${TMPDIR:-/tmp}}

    dcent_archive_error() {
        echo "Error: sysupgrade archive admission: $*" >&2
        exit 1
    }

    [ -n "$dcent_archive_path" ] || dcent_archive_error "archive path is required"
    [ -f "$dcent_archive_path" ] || dcent_archive_error "archive is not a regular file: $dcent_archive_path"
    [ -d "$dcent_archive_scratch" ] || dcent_archive_error "scratch directory is unavailable: $dcent_archive_scratch"

    case "$dcent_expected_board" in
        "") ;;
        *[!A-Za-z0-9._-]*|.*|-*)
            dcent_archive_error "expected board target is not a canonical identifier: $dcent_expected_board"
            ;;
    esac

    umask 077
    dcent_archive_token="dcent-sysupgrade-archive.$$.${PPID:-0}"
    dcent_member_list="$dcent_archive_scratch/$dcent_archive_token.members"
    dcent_member_types="$dcent_archive_scratch/$dcent_archive_token.types"
    dcent_manifest="$dcent_archive_scratch/$dcent_archive_token.manifest"
    dcent_manifest_status="$dcent_archive_scratch/$dcent_archive_token.manifest-status"
    dcent_manifest_paths="$dcent_archive_scratch/$dcent_archive_token.manifest-paths"
    dcent_payload_paths="$dcent_archive_scratch/$dcent_archive_token.payload-paths"
    trap 'rm -f "$dcent_member_list" "$dcent_member_types" "$dcent_manifest" "$dcent_manifest_status" "$dcent_manifest_paths" "$dcent_payload_paths"' EXIT
    trap 'exit 1' HUP INT TERM

    tar tf "$dcent_archive_path" >"$dcent_member_list" 2>/dev/null ||
        dcent_archive_error "archive cannot be listed"
    tar tvf "$dcent_archive_path" 2>/dev/null |
        awk '{
            type = substr($0, 1, 1)
            # BusyBox tar renders a hardlink with regular-file mode bits and
            # an explicit "name -> target" suffix, while GNU tar uses type
            # character "h".  Canonical leaf names contain no spaces/arrows,
            # so this distinction is unambiguous for admitted envelopes.
            if (type == "-" && index($0, " -> ")) type = "h"
            print type
        }' >"$dcent_member_types" ||
        dcent_archive_error "archive member types cannot be listed"

    dcent_member_count=$(awk 'END { print NR + 0 }' "$dcent_member_list")
    dcent_type_count=$(awk 'END { print NR + 0 }' "$dcent_member_types")
    [ "$dcent_member_count" -gt 0 ] || dcent_archive_error "archive has no members"
    [ "$dcent_member_count" -le "$DCENT_SYSUPGRADE_ARCHIVE_MAX_MEMBERS" ] ||
        dcent_archive_error "archive has $dcent_member_count members; maximum is $DCENT_SYSUPGRADE_ARCHIVE_MAX_MEMBERS"
    [ "$dcent_type_count" = "$dcent_member_count" ] ||
        dcent_archive_error "member/type listing counts disagree"

    dcent_duplicate=$(awk '
        seen[$0]++ { print $0; exit }
    ' "$dcent_member_list")
    [ -z "$dcent_duplicate" ] ||
        dcent_archive_error "duplicate archive member: $dcent_duplicate"

    dcent_logical_duplicate=$(awk '
        {
            logical = $0
            sub(/\/$/, "", logical)
            if (seen[logical]++) { print logical; exit }
        }
    ' "$dcent_member_list")
    [ -z "$dcent_logical_duplicate" ] ||
        dcent_archive_error "duplicate logical archive member: $dcent_logical_duplicate"

    if [ -z "$dcent_expected_board" ]; then
        dcent_prefixes=$(awk '
            {
                name = $0
                sub(/\/$/, "", name)
                split(name, parts, "/")
                if (parts[1] ~ /^sysupgrade-[A-Za-z0-9][A-Za-z0-9._-]*$/) {
                    seen[parts[1]] = 1
                }
            }
            END { for (prefix in seen) print prefix }
        ' "$dcent_member_list")
        dcent_prefix_count=$(printf '%s\n' "$dcent_prefixes" | awk 'NF { count++ } END { print count + 0 }')
        [ "$dcent_prefix_count" = 1 ] ||
            dcent_archive_error "archive must contain exactly one sysupgrade-<target> prefix"
        dcent_prefix=$dcent_prefixes
        dcent_expected_board=${dcent_prefix#sysupgrade-}
    else
        dcent_prefix="sysupgrade-$dcent_expected_board"
    fi

    # The canonical envelope is one explicit directory entry followed only by
    # flat, named leaves.  Reject './' aliases, nesting, backslashes, duplicate
    # directory spellings, foreign prefixes, and all unknown leaf names.
    awk -v prefix="$dcent_prefix" '
        BEGIN { bad = 0; directory_count = 0 }
        {
            name = $0
            if (name == prefix "/") {
                directory_count++
                next
            }
            if (index(name, "\\") || name ~ /^\.\// || name ~ /^\// ||
                name ~ /(^|\/)\.\.?($|\/)/) {
                printf "Error: sysupgrade archive admission: non-canonical member path: %s\n", name > "/dev/stderr"
                bad = 1
                next
            }
            if (index(name, prefix "/") != 1) {
                printf "Error: sysupgrade archive admission: member is outside expected %s/ prefix: %s\n", prefix, name > "/dev/stderr"
                bad = 1
                next
            }
            leaf = substr(name, length(prefix) + 2)
            if (leaf == "" || index(leaf, "/")) {
                printf "Error: sysupgrade archive admission: nested or empty member path: %s\n", name > "/dev/stderr"
                bad = 1
                next
            }
            if (leaf != "kernel" && leaf != "root" && leaf != "METADATA" &&
                leaf != "SHA256SUMS" && leaf != "MANIFEST.json" &&
                leaf != "MANIFEST.sig" && leaf != "release_ed25519.pub" &&
                leaf != "fpga_bitstream.bit") {
                printf "Error: sysupgrade archive admission: unknown member leaf: %s\n", leaf > "/dev/stderr"
                bad = 1
            }
        }
        END {
            if (directory_count != 1) {
                printf "Error: sysupgrade archive admission: expected exactly one canonical %s/ directory member (found %d)\n", prefix, directory_count > "/dev/stderr"
                bad = 1
            }
            exit bad ? 1 : 0
        }
    ' "$dcent_member_list" || exit 1

    awk -v prefix="$dcent_prefix" '
        NR == FNR { member_type[FNR] = $0; next }
        {
            expected = ($0 == prefix "/") ? "d" : "-"
            if (member_type[FNR] != expected) {
                printf "Error: sysupgrade archive admission: unsafe type %s for member %s (expected %s)\n", member_type[FNR], $0, expected > "/dev/stderr"
                bad = 1
            }
        }
        END { exit bad ? 1 : 0 }
    ' "$dcent_member_types" "$dcent_member_list" || exit 1

    dcent_manifest_member="$dcent_prefix/MANIFEST.json"
    dcent_manifest_count=$(awk -v member="$dcent_manifest_member" '$0 == member { count++ } END { print count + 0 }' "$dcent_member_list")
    [ "$dcent_manifest_count" = 1 ] ||
        dcent_archive_error "archive must contain exactly one regular $dcent_manifest_member"

    # Bound the only member read before full extraction.  Capturing one byte
    # beyond the limit distinguishes an oversized manifest without allowing it
    # to consume unbounded scratch space.
    (
        dcent_manifest_tar_status=0
        tar -xOf "$dcent_archive_path" "$dcent_manifest_member" 2>/dev/null ||
            dcent_manifest_tar_status=$?
        printf '%s\n' "$dcent_manifest_tar_status" >"$dcent_manifest_status"
    ) | dd bs=$((DCENT_SYSUPGRADE_MANIFEST_MAX_BYTES + 1)) count=1 of="$dcent_manifest" 2>/dev/null
    [ -r "$dcent_manifest_status" ] || dcent_archive_error "manifest extraction status is unavailable"
    [ "$(cat "$dcent_manifest_status")" = 0 ] || dcent_archive_error "MANIFEST.json cannot be read from archive"
    dcent_manifest_size=$(wc -c <"$dcent_manifest" | tr -d '[:space:]')
    [ "$dcent_manifest_size" -gt 0 ] || dcent_archive_error "MANIFEST.json is empty"
    [ "$dcent_manifest_size" -le "$DCENT_SYSUPGRADE_MANIFEST_MAX_BYTES" ] ||
        dcent_archive_error "MANIFEST.json exceeds $DCENT_SYSUPGRADE_MANIFEST_MAX_BYTES bytes"

    # Authority profile v1 is deliberately escape-free.  The small target
    # readers inspect literal field spellings; accepting JSON escapes would
    # let semantic parsers and byte-oriented cardinality checks disagree on
    # decoded keys or values.  A future profile may relax this only together
    # with one duplicate-preserving typed parser across every consumer.
    if LC_ALL=C grep -F '\' "$dcent_manifest" >/dev/null 2>&1; then
        dcent_archive_error "MANIFEST.json is not canonical: authority profile v1 forbids JSON escape sequences"
    fi

    dcent_path_key_count=$(awk '
        {
            line = $0
            while ((position = index(line, "\"path\"")) > 0) {
                count++
                line = substr(line, position + 6)
            }
        }
        END { print count + 0 }
    ' "$dcent_manifest")
    sed -n 's/.*"path"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$dcent_manifest" >"$dcent_manifest_paths"
    dcent_parsed_path_count=$(awk 'END { print NR + 0 }' "$dcent_manifest_paths")
    [ "$dcent_path_key_count" = "$dcent_parsed_path_count" ] ||
        dcent_archive_error "MANIFEST.json payload paths are not in canonical one-path-per-line form"

    awk -v prefix="$dcent_prefix" '
        {
            path = $0
            if (seen[path]++) {
                printf "Error: sysupgrade archive admission: duplicate manifest payload path: %s\n", path > "/dev/stderr"
                bad = 1
                next
            }
            if (index(path, prefix "/") != 1) {
                printf "Error: sysupgrade archive admission: manifest payload path is outside expected prefix: %s\n", path > "/dev/stderr"
                bad = 1
                next
            }
            leaf = substr(path, length(prefix) + 2)
            if (leaf != "kernel" && leaf != "root" && leaf != "METADATA" &&
                leaf != "release_ed25519.pub" && leaf != "fpga_bitstream.bit") {
                printf "Error: sysupgrade archive admission: unknown manifest payload path: %s\n", path > "/dev/stderr"
                bad = 1
            }
        }
        END { exit bad ? 1 : 0 }
    ' "$dcent_manifest_paths" || exit 1

    awk -v prefix="$dcent_prefix" '
        $0 == prefix "/kernel" || $0 == prefix "/root" ||
        $0 == prefix "/METADATA" || $0 == prefix "/release_ed25519.pub" ||
        $0 == prefix "/fpga_bitstream.bit" { print }
    ' "$dcent_member_list" >"$dcent_payload_paths"

    dcent_metadata_member="$dcent_prefix/METADATA"
    dcent_metadata_count=$(awk -v member="$dcent_metadata_member" '
        $0 == member { count++ }
        END { print count + 0 }
    ' "$dcent_member_list")
    [ "$dcent_metadata_count" = 1 ] ||
        dcent_archive_error "archive must contain exactly one regular $dcent_metadata_member"

    awk '
        NR == FNR { archive[$0]++; next }
        { manifest[$0]++ }
        END {
            for (path in archive) {
                if (manifest[path] != 1) {
                    printf "Error: sysupgrade archive admission: archive payload is not declared exactly once in MANIFEST.json: %s\n", path > "/dev/stderr"
                    bad = 1
                }
            }
            for (path in manifest) {
                if (archive[path] != 1) {
                    printf "Error: sysupgrade archive admission: MANIFEST.json declares an absent archive payload: %s\n", path > "/dev/stderr"
                    bad = 1
                }
            }
            exit bad ? 1 : 0
        }
    ' "$dcent_payload_paths" "$dcent_manifest_paths" || exit 1

    exit 0
)
