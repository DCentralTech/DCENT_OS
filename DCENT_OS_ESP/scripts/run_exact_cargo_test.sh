#!/bin/sh
# Prove that one fully qualified Rust test exists before executing it. Cargo
# otherwise exits successfully when a positional filter matches zero tests.

set -eu

if [ "$#" -lt 2 ]; then
    printf 'Usage: %s FULLY_QUALIFIED_TEST CARGO_TEST_ARGS...\n' "$0" >&2
    exit 2
fi

exact_test=$1
shift
case "$exact_test" in
    *::* ) ;;
    * )
        printf 'ERROR: exact Rust test name must be fully qualified: %s\n' \
            "$exact_test" >&2
        exit 2
        ;;
esac

list_output=$(cargo test "$@" "$exact_test" -- --list)
match_count=$(
    printf '%s\n' "$list_output" |
        awk -v expected="$exact_test: test" \
            '$0 == expected { count += 1 } END { print count + 0 }'
)
if [ "$match_count" -ne 1 ]; then
    printf 'ERROR: expected exactly one Rust test named %s, found %s\n' \
        "$exact_test" "$match_count" >&2
    printf '%s\n' "$list_output" >&2
    exit 1
fi

cargo test "$@" "$exact_test" -- --exact --include-ignored
