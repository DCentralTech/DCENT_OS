#!/bin/sh

set -eu

script_dir=$(CDPATH= cd "$(dirname "$0")" && pwd)
runner="$script_dir/run_exact_cargo_test.sh"
fake_bin="$script_dir/test-fixtures/fake-cargo-exact-runner"
test_name='fixture::exact_contract'
test_log=$(mktemp)
trap 'rm -f -- "$test_log"' EXIT HUP INT TERM
chmod +x "$fake_bin/cargo"

run_fixture() {
    FAKE_CARGO_MODE=$1 \
    FAKE_CARGO_LOG=$test_log \
    PATH="$fake_bin:$PATH" \
        sh "$runner" "$test_name" \
            --locked -p fixture-package --lib --target x86_64-unknown-linux-gnu
}

expect_status() {
    expected_status=$1
    mode=$2
    : > "$test_log"
    if run_fixture "$mode" >/dev/null 2>&1; then
        observed_status=0
    else
        observed_status=$?
    fi
    if [ "$observed_status" -ne "$expected_status" ]; then
        printf 'ERROR: mode %s returned %s, expected %s\n' \
            "$mode" "$observed_status" "$expected_status" >&2
        exit 1
    fi
}

expect_no_execution() {
    mode=$1
    if [ -s "$test_log" ]; then
        printf 'ERROR: mode %s executed after invalid inventory evidence\n' \
            "$mode" >&2
        exit 1
    fi
}

expect_one_execution() {
    mode=$1
    execution_count=$(
        awk '$0 == "executed" { count += 1 } END { print count + 0 }' \
            "$test_log"
    )
    if [ "$execution_count" -ne 1 ]; then
        printf 'ERROR: mode %s executed %s times, expected 1\n' \
            "$mode" "$execution_count" >&2
        exit 1
    fi
}

expect_status 1 zero
expect_no_execution zero
expect_status 1 duplicate
expect_no_execution duplicate
expect_status 41 list_failure
expect_no_execution list_failure
expect_status 0 one
expect_one_execution one
expect_status 0 ignored
expect_one_execution ignored
expect_status 42 test_failure
expect_one_execution test_failure

printf 'Exact Cargo test runner behavioral contracts passed\n'
