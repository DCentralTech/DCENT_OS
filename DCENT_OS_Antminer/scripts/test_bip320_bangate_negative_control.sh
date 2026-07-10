#!/bin/sh
#
# Negative control for ci_offline_gates.sh W6.5-6. It runs the real
# bip320_rejection_guard_check() function against temporary source fixtures that
# reintroduce the banned `version_bits_raw != 0 { continue; }` guard.

set -eu

SCRIPT_DIR=$(CDPATH= cd "$(dirname "$0")" && pwd)
GATE="$SCRIPT_DIR/ci_offline_gates.sh"

[ -f "$GATE" ] || {
    echo "missing ci_offline_gates.sh: $GATE" >&2
    exit 1
}

TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT INT TERM

one_line="$TMPDIR/bip320-oneline.rs"
two_line="$TMPDIR/bip320-twoline.rs"
comment_only="$TMPDIR/bip320-comment-only.rs"
runner="$TMPDIR/bip320-runner.sh"
out="$TMPDIR/bip320.out"

cat > "$one_line" <<'EOF_ONELINE'
fn submit_nonce(nr: Nonce) {
    loop {
        if nr.version_bits_raw != 0 { continue; }
        break;
    }
}
EOF_ONELINE

cat > "$two_line" <<'EOF_TWOLINE'
fn submit_nonce(nr: Nonce) {
    loop {
        if nr.version_bits_raw != 0 {
            continue;
        }
        break;
    }
}
EOF_TWOLINE

cat > "$comment_only" <<'EOF_COMMENT'
// if nr.version_bits_raw != 0 { continue; }
/// if nr.version_bits_raw != 0 {
///     continue;
/// }
EOF_COMMENT

{
    cat <<'EOF_RUNNER'
set -eu
failures=0
pass() {
    printf 'PASS: %s\n' "$*"
}
fail() {
    printf 'FAIL: %s\n' "$*" >&2
    failures=$((failures + 1))
}
EOF_RUNNER
    sed -n '/^bip320_rejection_guard_check() {$/,/^bip320_rejection_guard_check$/p' "$GATE"
    printf '%s\n' 'printf "failures=%s\n" "$failures"'
} > "$runner"

targets=$(printf '%s\n%s\n%s\n' "$one_line" "$two_line" "$comment_only")
DCENT_BIP320_REJECTION_GUARD_TARGETS="$targets" sh "$runner" > "$out" 2>&1 || {
    cat "$out" >&2
    exit 1
}

grep -F "W6.5-6 bip320-rejection-guard" "$out" >/dev/null || {
    cat "$out" >&2
    echo "missing BIP320 ban-gate failure label" >&2
    exit 1
}
grep -F "$one_line" "$out" >/dev/null || {
    cat "$out" >&2
    echo "one-line banned guard was not reported" >&2
    exit 1
}
grep -F "$two_line" "$out" >/dev/null || {
    cat "$out" >&2
    echo "two-line banned guard was not reported" >&2
    exit 1
}
if grep -F "$comment_only" "$out" >/dev/null; then
    cat "$out" >&2
    echo "comment-only BIP320 prose was reported as code" >&2
    exit 1
fi
grep -F "failures=1" "$out" >/dev/null || {
    cat "$out" >&2
    echo "negative control did not produce exactly one gate failure" >&2
    exit 1
}

printf 'BIP320_BANGATE_NEGATIVE_CONTROL_OK\n'
