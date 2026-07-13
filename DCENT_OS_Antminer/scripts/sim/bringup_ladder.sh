#!/bin/sh
set -eu

usage() {
    echo "usage: $0 --model <slug> --tier <0..4>" >&2
    exit 2
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

python "$script_dir/check_sim_tier_honesty.py"
case "$tier" in
    0) cargo check -p dcentrald-asic ;;
    1) cargo test -p dcentrald-asic --lib ;;
    2)
        case "$model" in
            s9|s17|s17pro|t17|s19pro|s19jpro|s19xp|s19kpro|s21|s21pro) ;;
            *) echo "$model has no declared T2 proof" >&2; exit 1 ;;
        esac
        cargo test -p dcentrald --features sim-hal --test sim_s19pro_t2
        ;;
    3)
        case "$model" in
            s9|s17|s17pro|t17|s19pro|s19jpro|s19xp|s19kpro|s21|s21pro)
                cargo test -p dcentrald-asic --features sim-hal --test golden_init_trace "$model"
                ;;
            *) echo "$model has no declared T3 vector proof" >&2; exit 1 ;;
        esac
        ;;
    4)
        "$script_dir/full_offline_model_proof.sh" --model "$model"
        ;;
    *) usage ;;
esac
echo "OFFLINE_SIM_TIER_OK model=$model tier=$tier"
