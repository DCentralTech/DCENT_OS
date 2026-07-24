#!/bin/sh
# Host-safe proof that every historical CV1835 mutation/recovery route is retired.

set -eu

SCRIPT_DIR=$(CDPATH= cd "$(dirname "$0")" && pwd)
PROJECT_DIR=$(CDPATH= cd "$SCRIPT_DIR/.." && pwd)
REPO_ROOT=$(CDPATH= cd "$PROJECT_DIR/../.." && pwd)
BOARD_DIR="$PROJECT_DIR/br2_external_dcentos/board/cvitek/cv1835-s19jpro"
OVERLAY="$BOARD_DIR/rootfs-overlay"
API_SOURCE="$PROJECT_DIR/dcentrald/dcentrald-api/src/routes/restore_to_stock.rs"
API_TEST="$PROJECT_DIR/dcentrald/dcentrald-api/tests/restore_to_stock_routes.rs"
BUILD_DRIVER="$SCRIPT_DIR/build_in_docker.sh"
STANDALONE_BUILD="$SCRIPT_DIR/build_cv1835_s19jpro.sh"
NAME_TEST="$SCRIPT_DIR/test_firmware_release_name.sh"
UPDATER="$SCRIPT_DIR/safe_sysupgrade_cv_emmc.sh"
REVERT="$SCRIPT_DIR/revert_to_stock_cv1835.sh"
POST_BUILD="$BOARD_DIR/post-build.sh"
POST_IMAGE="$BOARD_DIR/post-image.sh"
BOARD_README="$BOARD_DIR/README.md"
DEFCONFIG="$PROJECT_DIR/br2_external_dcentos/configs/dcentos_cv1835_s19jpro_defconfig"
BOOTCMD="$BOARD_DIR/uboot-bootcmd.txt"
S99VERIFY="$OVERLAY/etc/init.d/S99verify"
LINUX_FRAGMENT="$BOARD_DIR/linux-config-fragment.cfg"
HAL_FACTORY="$PROJECT_DIR/dcentrald/dcentrald-hal/src/platform/mod.rs"
HAL_CVITEK="$PROJECT_DIR/dcentrald/dcentrald-hal/src/platform/cvitek.rs"
DAEMON_MAIN="$PROJECT_DIR/dcentrald/dcentrald/src/main.rs"
CARGO_BUILDER="$SCRIPT_DIR/build-dcentrald.sh"
SOURCE_CLOSURE="$SCRIPT_DIR/source_closure.py"
MATRIX_JSON="$PROJECT_DIR/docs/architecture/hardware_enablement_matrix.json"
SCHEMA_HARDWARE="$REPO_ROOT/projects/dcent-schema/src/hardware.rs"
SCHEMA_CAPABILITY="$REPO_ROOT/projects/dcent-schema/src/capability.rs"
TOOLBOX="$REPO_ROOT/projects/dcent-toolbox/src/dcent_toolbox"
TOOLBOX_INSTALLER="$TOOLBOX/core/installer.py"
TOOLBOX_CLI="$TOOLBOX/cli/commands/install.py"
TOOLBOX_ROUTES="$TOOLBOX/tui/install/routes.py"
TOOLBOX_READINESS="$TOOLBOX/core/flash_readiness.py"
TOOLBOX_REVERT="$TOOLBOX/tui/widgets/revert_to_stock_wizard.py"
WORK=$(mktemp -d "${TMPDIR:-/tmp}/dcent-cv1835-retirement.XXXXXX")
trap 'rm -rf "$WORK"' EXIT HUP INT TERM

PASS=0
FAIL=0

ok() {
    PASS=$((PASS + 1))
    printf 'ok %d - %s\n' "$PASS" "$1"
}

not_ok() {
    FAIL=$((FAIL + 1))
    printf 'not ok - %s\n' "$1" >&2
}

assert_file() {
    if [ -f "$2" ]; then ok "$1"; else not_ok "$1 (missing $2)"; fi
}

assert_absent() {
    if [ ! -e "$2" ] && [ ! -L "$2" ]; then
        ok "$1"
    else
        not_ok "$1 (unexpected $2)"
    fi
}

assert_matches() {
    if grep -Eq -- "$2" "$3"; then ok "$1"; else not_ok "$1"; fi
}

assert_not_matches() {
    if grep -En -- "$2" "$3" >"$WORK/unexpected.out" 2>/dev/null; then
        not_ok "$1"
        cat "$WORK/unexpected.out" >&2
    else
        ok "$1"
    fi
}

assert_eq() {
    if [ "$2" = "$3" ]; then
        ok "$1"
    else
        not_ok "$1 (expected=$2 actual=$3)"
    fi
}

assert_builtin_refusal() {
    label=$1
    script=$2
    expected_messages=$3

    assert_file "$label exists" "$script"
    if sh -n "$script"; then ok "$label parses"; else not_ok "$label parses"; fi
    unexpected=$(awk '
        /^[[:space:]]*#/ || /^[[:space:]]*$/ { next }
        $1 != "printf" && $1 != "exit" { print NR ":" $0 }
    ' "$script")
    assert_eq "$label executable grammar is printf plus exit" "" "$unexpected"
    assert_eq "$label message count" "$expected_messages" "$(grep -Ec '^printf ' "$script")"
    assert_eq "$label has one unconditional refusal exit" 1 "$(grep -Ec '^exit 78$' "$script")"

    set +e
    PATH="$WORK/empty-path" /bin/sh "$script" --help ignored >"$WORK/refusal.out" 2>&1
    rc=$?
    set -e
    assert_eq "$label refuses every argument without external commands" 78 "$rc"
}

assert_builtin_refusal 'CV updater' "$UPDATER" 3
assert_builtin_refusal 'CV stock revert' "$REVERT" 3
assert_builtin_refusal 'target uninstall alias' "$OVERLAY/uninstall.sh" 2
assert_builtin_refusal 'standalone CV build entry point' "$STANDALONE_BUILD" 3
assert_builtin_refusal 'CV Buildroot post-build hook' "$POST_BUILD" 2
assert_builtin_refusal 'CV Buildroot post-image hook' "$POST_IMAGE" 2
assert_matches 'CV post-build declares typed non-activating build policy' \
    '^# DCENT_BUILD_POLICY=not-implemented-refusal$' "$POST_BUILD"

assert_matches 'updater pins the held FIP digest' \
    '874efb83b18a5cfbf76f1a9b514438813ced1aa279a115678c3cd9c50a66fd2e' "$UPDATER"
assert_matches 'updater records BuiltInVolatile mutation denial' \
    'BuiltInVolatile/mutation-denied' "$UPDATER"
assert_matches 'updater records p2 selector geometry without implementing it' \
    'LBA 40960 \(0xa000\), 2048 sectors' "$UPDATER"

assert_absent 'guessed fw_env.config is absent' "$OVERLAY/etc/fw_env.config"
assert_absent 'unadmitted CV1835 Buildroot defconfig is absent' "$DEFCONFIG"
assert_eq 'board maturity is evidence only' \
    'evidence-only-no-build-or-runtime-ownership' \
    "$(cat "$OVERLAY/etc/dcentos/board_status")"
assert_eq 'storage maturity is explicitly not implemented' \
    'emmc-update-not-implemented-builtin-volatile-p2-selector' \
    "$(cat "$OVERLAY/etc/dcentos/storage_status")"

for init_name in S37board_setup S46post-install S50dropbear S82dcentrald S99upgrade
do
    init_script="$OVERLAY/etc/init.d/$init_name"
    assert_file "$init_name containment shadow exists" "$init_script"
    if sh -n "$init_script"; then ok "$init_name parses"; else not_ok "$init_name parses"; fi
    set +e
    PATH="$WORK/empty-path" /bin/sh "$init_script" start >"$WORK/$init_name.out" 2>&1
    rc=$?
    set -e
    assert_eq "$init_name start is an external-command-free refusal/no-op" 0 "$rc"
done
assert_matches 'SSH start is refused' 'SSH start refused' "$WORK/S50dropbear.out"
assert_matches 'daemon hardware ownership is refused' \
    'runtime hardware ownership NOT IMPLEMENTED' "$WORK/S82dcentrald.out"
assert_matches 'boot-time persistent update is refused' \
    'persistent update NOT IMPLEMENTED' "$WORK/S99upgrade.out"

assert_matches 'historical boot recipe is visibly rejected' \
    '^# REJECTED HISTORICAL PROPOSAL - DO NOT APPLY$' "$BOOTCMD"
assert_not_matches 'boot record contains no active mutation or boot command' \
    '^[[:space:]]*(setenv|saveenv|mmc|ext4load|fatload|booti|bootm|reset)([[:space:]]|$)' "$BOOTCMD"
assert_not_matches 'recovery page exposes no mutation or reboot recipe' \
    'fw_printenv|fw_setenv|flash_erase|nandwrite|nanddump|reboot|sysupgrade|/dev/mmc' \
    "$OVERLAY/root/web/static/recovery.html"
assert_matches 'recovery page denies an on-target recovery service' \
    'No DCENT_OS on-target recovery or diagnostic service is implemented' \
    "$OVERLAY/root/web/static/recovery.html"

assert_matches 'CV V14 remains a red NOT_IMPLEMENTED result' \
    'emit_check V14 false "CV1835 persistent update NOT IMPLEMENTED: BuiltInVolatile environment is mutation-denied and no p2 marker-write transaction exists"' \
    "$S99VERIFY"
assert_not_matches 'CV verifier no longer claims bootcount/factory recovery authority' \
    'U-Boot bootcount|bootcount per|factory_kernel' "$S99VERIFY"

set +e
bash "$BUILD_DRIVER" --target cv1835-s19jpro >"$WORK/build-driver.out" 2>&1
build_rc=$?
set -e
assert_eq 'generic build driver refuses CV before Docker/build work' 78 "$build_rc"
assert_matches 'generic build refusal denies every artifact lane' \
    'has no firmware, sysupgrade, or supported artifact build lane' "$WORK/build-driver.out"
assert_not_matches 'generic build driver contains no CV firmware package authority' \
    'BR_DEFCONFIG="dcentos_cv1835|TARBALL_NAME="dcentos-sysupgrade-cv1835' "$BUILD_DRIVER"
assert_not_matches 'generic build driver does not redirect around its own refusal' \
    'use scripts/build_cv1835_s19jpro\.sh' "$BUILD_DRIVER"
assert_not_matches 'source-closure target set grants no CV build membership' \
    '^[[:space:]]*"cv1835-s19jpro":[[:space:]]*\(' "$SOURCE_CLOSURE"
assert_not_matches 'standalone Cargo builder advertises no CV target' \
    '^[[:space:]]*cvitek\)|Valid targets:.*cvitek|^[#[:space:]]*cvitek[[:space:]]+-' \
    "$CARGO_BUILDER"
assert_matches 'board documentation denies build publication' \
    'Build publication, runtime hardware' "$BOARD_README"
assert_matches 'board documentation records the defconfig admission boundary' \
    'No `dcentos_cv1835_s19jpro_defconfig` is committed' "$BOARD_README"
assert_not_matches 'board documentation advertises no CV artifact output' \
    'emits dcentos-offline-analysis|builds the explicitly non-installable|materializes exact committed' \
    "$BOARD_README"
assert_matches 'kernel fragment is explicitly evidence-only' \
    'Evidence ledger only\. DCENT_OS has no admitted CV1835 Buildroot' "$LINUX_FRAGMENT"
assert_not_matches 'kernel fragment names no deleted defconfig or admitted userspace' \
    'the defconfig|see defconfig|our ARM32|our Linaro|userspace to run' "$LINUX_FRAGMENT"

assert_matches 'shared matrix encodes no CV artifact' \
    '"board_target":"cv1835-s19jpro".*"artifact_kind":"none".*"artifact_maturity":"not_implemented"' \
    "$MATRIX_JSON"
assert_matches 'hardware enablement matrix uses schema 2 for the new wire values' \
    '^[[:space:]]*"schema":[[:space:]]*2,' "$MATRIX_JSON"
assert_matches 'shared schema has a typed absent-artifact kind' \
    '^[[:space:]]*None,$' "$SCHEMA_HARDWARE"
assert_matches 'shared schema has typed not-implemented artifact maturity' \
    '^[[:space:]]*NotImplemented,$' "$SCHEMA_HARDWARE"
assert_not_matches 'capability schema contains no CV deployment unlock' \
    'cv1835-emmc-proven|Cv1835EmmcProven' "$SCHEMA_CAPABILITY"

assert_matches 'HAL refuses CV before constructor/MMIO mutation' \
    'CV1835 runtime NOT IMPLEMENTED: automatic CVitek HAL construction and pinmux mutation are disabled' \
    "$HAL_FACTORY"
assert_not_matches 'automatic platform detection cannot construct the CV HAL' \
    'return Ok\(Box::new\(cvitek::CViTekPlatform::new\(\)\?\)\)' "$HAL_FACTORY"
assert_matches 'CV HAL modules are crate-private evidence, not public runtime API' \
    '^pub\(crate\) mod cvitek;' "$HAL_FACTORY"
assert_matches 'direct CV constructor is an unconditional typed refusal' \
    'CV1835 runtime NOT IMPLEMENTED: reverse-engineered register evidence is retained' \
    "$HAL_CVITEK"
assert_not_matches 'direct CV constructor has no pinmux, UART-table, probe, or env authority' \
    'replay_pinmux\(\)\?|select_uart_table_cv1835\(\)\?|std::env::var\(CV1835_ACCEPT_UNVERIFIED_ENV\)' \
    "$HAL_CVITEK"
assert_not_matches 'daemon safe-off exposes no CV power mutation' \
    'CvitekDisablePsu|platform::cvitek::disable_psu' "$DAEMON_MAIN"
assert_matches 'safe-off leaves fans unchanged when power cut is unproven' \
    'if rc != 0 \{' "$DAEMON_MAIN"
assert_matches 'safe-off failure returns before fan mutation' \
    'power cut was not proven; fans are left unchanged' "$DAEMON_MAIN"

assert_absent 'speculative toolbox CV artifact classifier is absent' \
    "$TOOLBOX/core/cv1835_artifact_state.py"
assert_not_matches 'toolbox planner registers no CV install fact or unlock' \
    'board_family="cv1835"|cv1835-emmc-proven|DCENT_CV1835_EMMC_PROVEN' \
    "$TOOLBOX_INSTALLER"
assert_not_matches 'toolbox CLI exposes no CV install unlock' \
    'accept-cv1835-emmc-lab|accept_cv1835_emmc_lab|DCENT_CV1835_EMMC_PROVEN' \
    "$TOOLBOX_CLI"
assert_not_matches 'toolbox TUI exposes no CV route or env unlock' \
    'cv1835-runtime-only-lab|emmc-lab|DCENT_CV1835_EMMC_PROVEN' \
    "$TOOLBOX_ROUTES"
assert_not_matches 'toolbox readiness advertises no CV artifact or route' \
    'dcentos-sysupgrade-cv1835|cv1835-[a-z0-9-]*emmc-lab|DCENT_CV1835_EMMC_PROVEN' \
    "$TOOLBOX_READINESS"
assert_not_matches 'toolbox revert wizard exposes no CV executable route' \
    '^[[:space:]]*"cv1835(-[^"]*)?":[[:space:]]*"scripts/' "$TOOLBOX_REVERT"

if sh "$NAME_TEST" >"$WORK/name-test.out"; then
    ok 'firmware release-name suite rejects CV and retains other families'
else
    not_ok 'firmware release-name suite rejects CV and retains other families'
    cat "$WORK/name-test.out" >&2
fi

assert_not_matches 'restore API contains no guessed CV signature/device/helper' \
    'cv1835-bm1362|/dev/mmcblk0boot0|/dev/mmcblk0p2|revert_to_stock_cv1835_s19j\.sh' \
    "$API_SOURCE"
assert_matches 'restore tests pin CV profile absence' \
    'profile_for\("cv1835-bm1362"\)\.is_none\(\)' "$API_TEST"
assert_matches 'restore tests prevent speculative CV contract regression' \
    'fn cv1835_speculative_restore_profile_cannot_reappear\(\)' "$API_TEST"

printf 'CV1835 brick-vector retirement tests: %d passed, %d failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
