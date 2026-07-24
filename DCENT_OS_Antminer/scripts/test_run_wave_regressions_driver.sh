#!/bin/sh
# Exercise the wave-regression command construction without compiling Rust.

set -eu

SCRIPT_DIR=$(CDPATH= cd "$(dirname "$0")" && pwd)
PROJECT_DIR=$(CDPATH= cd "$SCRIPT_DIR/.." && pwd)
RUNNER="$SCRIPT_DIR/run_wave_regressions.sh"
TMPDIR_TEST=$(mktemp -d "${TMPDIR:-/tmp}/dcent-wave-driver.XXXXXX")
trap 'rm -rf "$TMPDIR_TEST"' EXIT HUP INT TERM

assert_eq() {
    expected=$1
    actual=$2
    description=$3
    if [ "$expected" != "$actual" ]; then
        printf 'FAIL: %s (expected=%s actual=%s)\n' "$description" "$expected" "$actual" >&2
        exit 1
    fi
}

assert_all_locked() {
    log=$1
    prefix=$2
    expected_count=$3

    actual_count=$(wc -l < "$log" | tr -d ' ')
    assert_eq "$expected_count" "$actual_count" "$prefix invocation count"

    unlocked_count=$(awk -v prefix="$prefix" '
        BEGIN {
            expected = "test --locked "
            if (prefix != "") {
                expected = prefix " " expected
            }
        }
        index($0, expected) != 1 { count += 1 }
        END { print count + 0 }
    ' "$log")
    assert_eq 0 "$unlocked_count" "$prefix always uses cargo test --locked"
}

assert_exact_contracts() {
    log=$1
    for exact_test in \
        watchdog::tests::try_close_magic_is_the_only_magic_close_path \
        fan::tests::fan_variant_topology_pins_physical_tach_and_pwm_channels \
        xadc::tests::xadc_conversions_reject_non_finite_results; do
        list_count=$(awk -v exact_test="$exact_test" '
            index($0, exact_test) > 0 && index($0, "-- --list") > 0 {
                count += 1
            }
            END { print count + 0 }
        ' "$log")
        execute_count=$(awk -v exact_test="$exact_test" '
            index($0, exact_test) > 0 &&
                index($0, "-- --exact --include-ignored") > 0 {
                count += 1
            }
            END { print count + 0 }
        ' "$log")
        assert_eq 1 "$list_count" "$exact_test inventory count"
        assert_eq 1 "$execute_count" "$exact_test execution count"
    done
}

assert_exact_failure() {
    mode=$1
    expected_status=$2
    log="$TMPDIR_TEST/cargo-$mode.log"
    if FAKE_TOOL_LOG="$log" \
        FAKE_CARGO_MODE="$mode" \
        PATH="$TMPDIR_TEST/cargo-bin:/usr/bin:/bin" \
            sh "$RUNNER" >/dev/null 2>&1; then
        observed_status=0
    else
        observed_status=$?
    fi
    assert_eq "$expected_status" "$observed_status" "$mode status"
    execution_count=$(awk '
        index($0, "watchdog::tests::try_close_magic_is_the_only_magic_close_path") > 0 &&
            index($0, "-- --exact --include-ignored") > 0 {
            count += 1
        }
        END { print count + 0 }
    ' "$log")
    if [ "$mode" = test_failure ]; then
        assert_eq 1 "$execution_count" "$mode reaches exact execution once"
    else
        assert_eq 0 "$execution_count" "$mode never executes exact test"
    fi
}

wave_count=$(find "$PROJECT_DIR/dcentrald/dcentrald/tests" -maxdepth 1 -type f -name 'wave*.rs' | wc -l | tr -d ' ')
if [ "$wave_count" -eq 0 ]; then
    printf 'FAIL: fixture repository has no wave regression pins\n' >&2
    exit 1
fi
expected_count=$((wave_count + 9))

mkdir -p "$TMPDIR_TEST/rustup-bin" "$TMPDIR_TEST/cargo-bin"

cat > "$TMPDIR_TEST/rustup-bin/rustup" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >> "$FAKE_TOOL_LOG"
is_list=0
exact_test=
for argument in "$@"; do
    case "$argument" in
        *::*) exact_test=$argument ;;
        --list) is_list=1 ;;
    esac
done
if [ "$is_list" -eq 1 ]; then
    printf '%s: test\n' "$exact_test"
fi
EOF
chmod +x "$TMPDIR_TEST/rustup-bin/rustup"

FAKE_TOOL_LOG="$TMPDIR_TEST/rustup.log" \
PATH="$TMPDIR_TEST/rustup-bin:/usr/bin:/bin" \
DCENT_RUST_TOOLCHAIN=fixture-toolchain \
    sh "$RUNNER" > "$TMPDIR_TEST/rustup.out"
assert_all_locked "$TMPDIR_TEST/rustup.log" \
    'run fixture-toolchain cargo' "$expected_count"
assert_exact_contracts "$TMPDIR_TEST/rustup.log"

cat > "$TMPDIR_TEST/cargo-bin/cargo" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >> "$FAKE_TOOL_LOG"
is_list=0
is_exact=0
exact_test=
for argument in "$@"; do
    case "$argument" in
        *::*) exact_test=$argument ;;
        --list) is_list=1 ;;
        --exact) is_exact=1 ;;
    esac
done
if [ "$is_list" -eq 1 ]; then
    case "${FAKE_CARGO_MODE:-one}" in
        one|test_failure) printf '%s: test\n' "$exact_test" ;;
        zero) ;;
        duplicate)
            printf '%s: test\n' "$exact_test"
            printf '%s: test\n' "$exact_test"
            ;;
        list_failure) exit 41 ;;
        *) exit 91 ;;
    esac
elif [ "$is_exact" -eq 1 ] && [ "${FAKE_CARGO_MODE:-one}" = test_failure ]; then
    exit 42
fi
EOF
chmod +x "$TMPDIR_TEST/cargo-bin/cargo"

FAKE_TOOL_LOG="$TMPDIR_TEST/cargo.log" \
PATH="$TMPDIR_TEST/cargo-bin:/usr/bin:/bin" \
    sh "$RUNNER" > "$TMPDIR_TEST/cargo.out"
assert_all_locked "$TMPDIR_TEST/cargo.log" '' "$expected_count"
assert_exact_contracts "$TMPDIR_TEST/cargo.log"

assert_exact_failure zero 1
assert_exact_failure duplicate 1
assert_exact_failure list_failure 41
assert_exact_failure test_failure 42

printf 'PASS: wave regression driver uses locked exact contracts (%s invocations per path)\n' \
    "$expected_count"
