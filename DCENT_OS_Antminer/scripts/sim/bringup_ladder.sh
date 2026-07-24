#!/bin/sh
set -eu

usage() {
    echo "usage: $0 --model <slug> --tier <0..4>" >&2
    exit 2
}

run_exact_cargo_test() {
    exact_test=$1
    shift
    list_output=$(cargo test "$@" "$exact_test" -- --list)
    match_count=$(
        printf '%s\n' "$list_output" |
            awk -v expected="$exact_test: test" '$0 == expected { count += 1 } END { print count + 0 }'
    )
    if [ "$match_count" -ne 1 ]; then
        echo "exact simulator test '$exact_test' resolved $match_count times, expected 1" >&2
        printf '%s\n' "$list_output" >&2
        exit 1
    fi
    cargo test "$@" "$exact_test" -- --exact --include-ignored
}

run_t2_model_proof() {
    case "$1" in
        s9) test_name='s9_reaches_headless_t2_through_legacy_fpga_fifo' ;;
        s17) test_name='s17_reaches_headless_t2' ;;
        s17pro) test_name='s17pro_reaches_headless_t2' ;;
        t17) test_name='t17_reaches_headless_t2' ;;
        s19pro) test_name='s19pro_reaches_headless_t2' ;;
        s19jpro) test_name='s19jpro_reaches_headless_t2' ;;
        s19xp) test_name='s19xp_reaches_headless_t2' ;;
        s19kpro) test_name='s19kpro_reaches_headless_t2' ;;
        s21) test_name='s21_reaches_headless_t2' ;;
        s21pro) test_name='s21pro_reaches_headless_t2' ;;
        *) echo "$1 has no declared T2 proof" >&2; exit 1 ;;
    esac
    run_exact_cargo_test "$test_name" --locked --offline -p dcentrald \
        --features sim-hal --test sim_s19pro_t2
}

run_t3_model_proof() {
    case "$1" in
        s9)
            tests='s9_init_is_an_exact_ordered_byte_match
s9_open_core_uses_all_114_activation_slots_and_enters_mining_mode'
            ;;
        s17)
            tests='s17_init_is_an_exact_ordered_byte_match
s17_enumeration_is_an_exact_saleae_frame_match'
            ;;
        s17pro) tests='s17pro_init_is_an_exact_ordered_byte_match' ;;
        t17) tests='t17_init_is_an_exact_ordered_byte_match' ;;
        s19jpro) tests='s19jpro_init_contains_provenance_backed_structural_sequence' ;;
        s19xp) tests='s19xp_init_contains_provenance_backed_structural_sequence' ;;
        s19kpro) tests='s19kpro_init_contains_provenance_backed_structural_sequence' ;;
        s21) tests='s21_init_contains_provenance_backed_structural_sequence' ;;
        s21pro) tests='s21pro_init_contains_provenance_backed_structural_sequence' ;;
        *) echo "$1 has no declared T3 vector proof" >&2; exit 1 ;;
    esac
    for test_name in $tests; do
        run_exact_cargo_test "$test_name" --locked --offline -p dcentrald-asic \
            --features sim-hal --test golden_init_trace
    done
}

model=''
tier=''
while [ "$#" -gt 0 ]; do
    case "$1" in
        --model) [ "$#" -ge 2 ] || usage; model=$2; shift 2 ;;
        --tier) [ "$#" -ge 2 ] || usage; tier=$2; shift 2 ;;
        *) usage ;;
    esac
done
[ -n "$model" ] && [ -n "$tier" ] || usage

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
project_dir=$(CDPATH= cd -- "$script_dir/../.." && pwd)
cd "$project_dir/dcentrald"

if command -v python3 >/dev/null 2>&1; then
    python_bin=python3
elif command -v python >/dev/null 2>&1; then
    python_bin=python
else
    echo "python3 or python is required for simulator tier validation" >&2
    exit 1
fi
"$python_bin" "$script_dir/check_sim_tier_honesty.py" \
    --model "$model" --tier "$tier"
case "$tier" in
    0) cargo check --locked --offline -p dcentrald-asic ;;
    1) cargo test --locked --offline -p dcentrald-asic --lib ;;
    2) run_t2_model_proof "$model" ;;
    3) run_t3_model_proof "$model" ;;
    4)
        "$script_dir/full_offline_model_proof.sh" --model "$model"
        ;;
    *) usage ;;
esac
echo "OFFLINE_SIM_TIER_OK model=$model tier=$tier"
