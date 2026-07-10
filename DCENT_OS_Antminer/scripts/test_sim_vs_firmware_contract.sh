#!/bin/sh
# Host-only simulator-vs-firmware contract gate. This never contacts miners.
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
DCENTOS_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
PROJECTS_ROOT=$(CDPATH= cd -- "$DCENTOS_ROOT/.." && pwd)

failures=0

pass() {
    printf 'PASS: %s\n' "$1"
}

fail() {
    printf 'FAIL: %s\n' "$1" >&2
    failures=$((failures + 1))
}

require_file() {
    if [ -f "$1" ]; then
        pass "$2"
    else
        fail "$2 (missing $1)"
    fi
}

require_literal() {
    file=$1
    literal=$2
    label=$3
    if [ ! -f "$file" ]; then
        fail "$label (missing $file)"
        return
    fi
    if grep -Fq "$literal" "$file"; then
        pass "$label"
    else
        fail "$label (missing literal: $literal)"
    fi
}

require_regex() {
    file=$1
    pattern=$2
    label=$3
    if [ ! -f "$file" ]; then
        fail "$label (missing $file)"
        return
    fi
    if grep -Eq "$pattern" "$file"; then
        pass "$label"
    else
        fail "$label (missing pattern: $pattern)"
    fi
}

profiles="$PROJECTS_ROOT/dcent-toolbox/src/dcent_toolbox/simulators/profiles.py"
sim_tests="$PROJECTS_ROOT/dcent-toolbox/tests/test_simulators.py"
simulate_command_tests="$PROJECTS_ROOT/dcent-toolbox/tests/test_simulate_command.py"
board_catalog="$PROJECTS_ROOT/dcent-toolbox/src/dcent_toolbox/core/board_catalog.py"
model_rs="$DCENTOS_ROOT/dcentrald/dcentrald/src/model.rs"
cgminer_rs="$DCENTOS_ROOT/dcentrald/dcentrald-api/src/cgminer.rs"
esp_api_info="$PROJECTS_ROOT/dcentos-esp/dcentaxe/src/api_system_info.rs"
esp_api_rs="$PROJECTS_ROOT/dcentos-esp/dcentaxe/src/api.rs"
esp_core_guards="$PROJECTS_ROOT/dcentos-esp/dcentaxe-core/src/lib.rs"

require_file "$profiles" "toolbox simulator profiles source is present"
require_file "$sim_tests" "toolbox simulator transport tests are present"
require_file "$simulate_command_tests" "toolbox simulate CLI tests are present"
require_file "$board_catalog" "toolbox board catalog source is present"
require_file "$model_rs" "dcentrald board-target chip resolver source is present"
require_file "$cgminer_rs" "dcentrald cgminer firmware surface source is present"
require_file "$esp_api_info" "ESP /api/system/info serde source is present"
require_file "$esp_api_rs" "ESP /api/system/info handler source is present"
require_file "$esp_core_guards" "ESP host wire-contract guards are present"

# Public-beta simulator profiles must map to the same chip identities firmware
# resolves from /etc/dcentos/board_target. This prevents the offline simulator
# from becoming a coherent, but wrong, stand-in for the promoted firmware tier.
require_literal "$profiles" '("am1-s9", "stock", "Antminer S9", "BM1387"' \
    "sim profile pins am1-s9 to Antminer S9 / BM1387"
require_literal "$profiles" '("am2-s19jpro-zynq", "braiinsos", "Antminer S19j Pro", "BM1362"' \
    "sim profile pins am2-s19jpro-zynq to Antminer S19j Pro / BM1362"
require_literal "$model_rs" '"am1s9" => "BM1387"' \
    "firmware resolver maps am1-s9 board_target to BM1387"
require_literal "$model_rs" '"am2s19j" | "am2s19jpro" | "am2s19jprozynq"' \
    "firmware resolver recognizes am2-s19jpro-zynq as the BM1362 family"
require_literal "$model_rs" 'assert_eq!(board_target_chip_label("am1-s9"), Some("BM1387"));' \
    "firmware host test pins am1-s9 chip reconciliation"
require_literal "$model_rs" 'board_target_chip_label("am2-s19jpro-zynq"),' \
    "firmware host test pins am2-s19jpro-zynq chip reconciliation"

# The toolbox simulator must keep its profile list joined to the catalog and
# reachable through the simulate command. Those tests are the executable side of
# this static contract when the toolbox suite is run.
require_literal "$sim_tests" 'def test_default_profiles_seed_from_catalog():' \
    "toolbox test keeps simulator profiles joined to board_catalog"
require_literal "$sim_tests" 'assert p.version_reply()["VERSION"][0]["Type"] == p.model' \
    "toolbox test keeps CGMiner model identity coherent"
require_literal "$simulate_command_tests" '"am1-s9",' \
    "simulate CLI matrix includes am1-s9"
require_literal "$simulate_command_tests" '"am2-s19jpro-zynq",' \
    "simulate CLI matrix includes am2-s19jpro-zynq"
require_literal "$simulate_command_tests" '"profile", build_default_profiles(), ids=lambda p: p.board_target' \
    "simulate CLI self-test parametrizes every default profile"

# CGMiner-compatible Antminer contract: the simulator and firmware both expose
# the read envelopes and key accounting fields external fleet tools consume.
for key in '"SUMMARY"' '"POOLS"' '"DEVS"' '"Accepted"' '"Hardware Errors"'; do
    require_literal "$profiles" "$key" "sim cgminer profile emits $key"
    require_literal "$cgminer_rs" "$key" "firmware cgminer surface emits $key"
done
require_literal "$sim_tests" 'async def test_cgminer_reads_over_loopback_socket():' \
    "toolbox loopback test exercises real CGMiner transport"
require_literal "$sim_tests" 'summary["SUMMARY"][0]["Accepted"] == profile.accepted' \
    "toolbox CGMiner loopback test pins accepted-share field"

# AxeOS/Bitaxe contract: the simulator's /api/system/info body must use the
# same wire key names the ESP firmware keeps stable for AxeOS-compatible tools.
for key in '"boardTarget"' '"ASICModel"' '"hashRate"'; do
    require_literal "$profiles" "$key" "sim AxeOS profile emits $key"
done
require_literal "$esp_api_info" '#[serde(rename_all = "camelCase")]' \
    "ESP SystemInfoResponse keeps camelCase boardTarget derivation"
require_literal "$esp_api_info" '#[serde(rename = "ASICModel")]' \
    "ESP SystemInfoResponse pins ASICModel rename"
require_literal "$esp_api_info" '#[serde(rename = "hashRate")]' \
    "ESP SystemInfoResponse pins hashRate rename"
require_literal "$esp_api_info" "pub board_target: &'static str" \
    "ESP SystemInfoResponse carries board_target for boardTarget"
require_literal "$esp_api_rs" '"boardTarget": BUILD_BOARD_TARGET' \
    "ESP legacy json path still emits boardTarget from build target"
require_literal "$esp_core_guards" 'mod api_wire_contract_guards' \
    "ESP host tests include /api/system/info wire-contract guards"
require_literal "$sim_tests" 'async def test_axeos_info_over_loopback_socket():' \
    "toolbox loopback test exercises real AxeOS HTTP transport"
require_literal "$sim_tests" 'body["hashRate"] == profile.hashrate_ghs' \
    "toolbox AxeOS loopback test pins hashRate as GH/s"

# The default simulator set must include at least one Antminer/cgminer profile
# and one Bitaxe/AxeOS profile so both firmware surfaces stay represented.
require_literal "$profiles" 'mtd_count=3, hashrate_ghs=13500.0, board_count=3, speaks_cgminer=True))' \
    "default simulator set includes a CGMiner-speaking Antminer"
require_literal "$profiles" 'make="bitaxe", speaks_cgminer=False, speaks_axeos=True, speaks_ssh=False,' \
    "default simulator set includes an AxeOS-speaking Bitaxe"

if [ "$failures" -ne 0 ]; then
    printf '\nsim-vs-firmware contract failed: %s failure(s)\n' "$failures" >&2
    exit 1
fi

printf '\nsim-vs-firmware contract passed.\n'
