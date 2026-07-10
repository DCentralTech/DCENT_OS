#!/bin/sh
#
# Shared package/staging gate for dcentrald version consistency.

dcent_version_gate_truthy() {
    case "${1:-}" in
        1|true|TRUE|yes|YES|y|Y) return 0 ;;
        *) return 1 ;;
    esac
}

dcent_version_gate_release_status() {
    case "${1:-release}" in
        release|production|stable) return 0 ;;
        *) return 1 ;;
    esac
}

dcent_version_gate_read_first_line() {
    sed -n 's/^[[:space:]]*//;s/[[:space:]]*$//;/^$/!{p;q;}' "$1"
}

dcent_version_gate_cargo_version() {
    cargo_toml=$1
    [ -f "$cargo_toml" ] || return 1

    awk '
        /^\[workspace.package\]/ { in_pkg = 1; next }
        /^\[/ { in_pkg = 0 }
        in_pkg && /^[[:space:]]*version[[:space:]]*=/ {
            gsub(/"/, "", $0)
            sub(/.*=[[:space:]]*/, "", $0)
            sub(/[[:space:]]*$/, "", $0)
            print $0
            exit
        }
    ' "$cargo_toml"
}

dcent_version_gate_staged_version() {
    target_dir=$1

    for candidate in \
        "$target_dir/etc/dcentos-version" \
        "$target_dir/etc/dcentos/version" \
        "$target_dir/etc/dcentos/package_version"
    do
        if [ -f "$candidate" ]; then
            value=$(dcent_version_gate_read_first_line "$candidate")
            if [ -n "$value" ]; then
                printf '%s:%s\n' "$value" "$candidate"
                return 0
            fi
        fi
    done

    if [ -n "${DCENT_PACKAGE_VERSION:-}" ]; then
        printf '%s:%s\n' "$DCENT_PACKAGE_VERSION" "DCENT_PACKAGE_VERSION"
        return 0
    fi

    return 1
}

dcent_version_gate_binary_versions() {
    bin=$1

    if command -v strings >/dev/null 2>&1; then
        strings "$bin" 2>/dev/null
    else
        LC_ALL=C grep -a 'dcentrald/' "$bin" 2>/dev/null || true
    fi | sed -n 's/.*\(dcentrald\/[vV]*[0-9][0-9.+:-]*\).*/\1/p' | sort -u
    # W13.F1: tightened continuation class (dropped letters) to prevent
    # over-match into adjacent .rodata strings (e.g. "ConfigPool" landing
    # next to "dcentrald/0.9.0" caused false-positive packaging failures).
}

dcent_version_gate_warn_or_fail() {
    label=$1
    reason=$2

    if ! dcent_version_gate_release_status "${DCENT_PACKAGE_STATUS:-release}" \
        && dcent_version_gate_truthy "${DCENT_ALLOW_UNSIGNED_SYSUPGRADE:-0}"; then
        echo "$label: WARNING: $reason" >&2
        echo "$label: WARNING: bypassed only because DCENT_PACKAGE_STATUS=${DCENT_PACKAGE_STATUS:-} and DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1" >&2
        return 0
    fi

    echo "$label: ERROR: $reason" >&2
    echo "$label: ERROR: refusing to ship staged dcentrald with inconsistent version metadata" >&2
    echo "$label: ERROR: lab bypass requires non-release DCENT_PACKAGE_STATUS plus DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1" >&2
    exit 1
}

dcent_require_dcentrald_version_match() {
    target_dir=$1
    bin=$2
    label=${3:-dcentrald version gate}
    cargo_toml=${4:-}

    [ -f "$bin" ] || return 0

    staged_pair=$(dcent_version_gate_staged_version "$target_dir" || true)
    if [ -z "$staged_pair" ]; then
        dcent_version_gate_warn_or_fail "$label" "no staged /etc/dcentos-version or equivalent metadata found for $bin"
        return 0
    fi
    staged_version=${staged_pair%%:*}
    staged_source=${staged_pair#*:}

    cargo_version=""
    if [ -n "$cargo_toml" ] && [ -f "$cargo_toml" ]; then
        cargo_version=$(dcent_version_gate_cargo_version "$cargo_toml" || true)
        if [ -z "$cargo_version" ]; then
            dcent_version_gate_warn_or_fail "$label" "could not read workspace.package.version from $cargo_toml"
            return 0
        fi
        if [ "$staged_version" != "$cargo_version" ]; then
            dcent_version_gate_warn_or_fail "$label" "staged version $staged_version from $staged_source != Cargo workspace version $cargo_version"
            return 0
        fi
    fi

    expected_version=$staged_version
    [ -n "$cargo_version" ] && expected_version=$cargo_version
    expected_string="dcentrald/$expected_version"

    versions=$(dcent_version_gate_binary_versions "$bin" || true)
    version_count=$(printf '%s\n' "$versions" | sed '/^$/d' | wc -l | tr -d ' ')
    case "$version_count" in
        1) ;;
        0)
            dcent_version_gate_warn_or_fail "$label" "no dcentrald/<version> string found in $bin; expected $expected_string"
            return 0
            ;;
        *)
            dcent_version_gate_warn_or_fail "$label" "multiple dcentrald/<version> strings found in $bin: $(printf '%s' "$versions" | tr '\n' ' ')"
            return 0
            ;;
    esac

    actual_string=$(printf '%s\n' "$versions" | sed '/^$/d' | head -n 1)
    if [ "$actual_string" != "$expected_string" ]; then
        dcent_version_gate_warn_or_fail "$label" "binary string $actual_string != expected $expected_string from $staged_source"
        return 0
    fi

    echo "$label: dcentrald version gate passed ($actual_string matches $staged_source)"
}
