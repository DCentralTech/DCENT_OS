#!/bin/sh
# Offline source contract for Amlogic boot/runtime/crash power containment.

set -u

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PROJECT_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
S37="$PROJECT_DIR/br2_external_dcentos/board/amlogic/rootfs-overlay/etc/init.d/S37board_setup"
ALIAS="$PROJECT_DIR/br2_external_dcentos/board/amlogic/rootfs-overlay/etc/init.d/S37bitmainer_setup"
S82="$PROJECT_DIR/br2_external_dcentos/board/amlogic/rootfs-overlay/etc/init.d/S82dcentrald"
LATCH="$PROJECT_DIR/br2_external_dcentos/board/common/rootfs-overlay/usr/libexec/dcentos/dcentrald-session-latch.sh"
INIT="$PROJECT_DIR/dcentrald/dcentos-init/src/main.rs"
API_REBOOT="$PROJECT_DIR/dcentrald/dcentrald-api/src/rest/late.rs"
HAL="$PROJECT_DIR/dcentrald/dcentrald-hal/src/platform/amlogic/mod.rs"
RECOVERY="$PROJECT_DIR/br2_external_dcentos/board/amlogic/rootfs-overlay/root/web/static/recovery.html"
DIAGNOSTIC="$PROJECT_DIR/br2_external_dcentos/board/amlogic/rootfs-overlay/root/web/static/diagnostic.html"
WEB_SERVER="$PROJECT_DIR/br2_external_dcentos/board/amlogic/rootfs-overlay/root/web/server.py"
FAILURES=0

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    FAILURES=$((FAILURES + 1))
}

pass() {
    printf 'PASS: %s\n' "$*"
}

require_literal() {
    FILE=$1
    LITERAL=$2
    LABEL=$3
    if grep -Fq "$LITERAL" "$FILE"; then
        pass "$LABEL"
    else
        fail "$LABEL"
    fi
}

for script in "$S37" "$ALIAS" "$S82" "$LATCH"; do
    if sh -n "$script"; then
        pass "$(basename "$script") is POSIX-shell parseable"
    else
        fail "$(basename "$script") is not POSIX-shell parseable"
    fi
done

require_literal "$S37" 'GPIO437 is PWR_EN and is active HIGH: 1=ON, 0=OFF' \
    'S37 documents the verified active-high polarity'
require_literal "$S37" 'set_gpio_direction_checked "$PWR_GPIO" low out' \
    'S37 uses glitch-free direction=low for the first power-gate mutation'
require_literal "$S37" 'set_gpio_value_checked "$PWR_GPIO" 0' \
    'S37 requires checked GPIO437-low value readback'
require_literal "$S37" 'write_and_check "$ACTIVE_LOW_PATH" 0' \
    'S37 pins and checks GPIO437 raw active-high mode'
require_literal "$S37" 'FAN_BOOT_DUTY_NS=30000' \
    'S37 boot cooling is pinned to the PWM30 ceiling'
require_literal "$S37" 'fan0_period_ns=%s' \
    'S37 receipt records the complete PWM period/duty/enable state'
require_literal "$S37" 'for GPIO in 476 477' \
    'S37 configures both management-fabric pinmux guards together'
require_literal "$S37" 'configure_input_gpio "$GPIO" || return 1' \
    'S37 GPIO476/477 setup uses checked input direction'
require_literal "$S37" 'schema=dcentos.amlogic-safe-state/v1' \
    'S37 emits a versioned boot-safe receipt'
require_literal "$S37" 'gpio_active_low=0' \
    'S37 receipt records raw active-high GPIO semantics'
require_literal "$S37" 'physical_rail_measured=false' \
    'S37 receipt does not overclaim electrical rail proof'
require_literal "$S37" 'write_receipt runtime-handoff' \
    'S37 records an explicit boot-to-runtime handoff state'
require_literal "$S37" 'verify_boot_safe_receipt || return 1' \
    'terminal-safe receipts cannot be promoted back into runtime handoff'

if grep -Eq 'active LOW.*437|437.*active LOW|FAN_BOOT_DUTY_NS=100000' "$S37"; then
    fail 'S37 retains inverted GPIO437 polarity or 100-percent boot PWM'
else
    pass 'S37 contains neither inverted polarity nor 100-percent boot PWM'
fi

if grep -Fq '/etc/init.d/S37board_setup start' "$ALIAS"; then
    fail 'S37 compatibility alias still duplicates canonical initialization'
else
    pass 'S37 compatibility alias cannot execute board initialization twice'
fi

require_literal "$S82" 'verify_amlogic_boot_safe_state' \
    'S82 requires the checked boot-safe receipt'
require_literal "$S82" 'mark-runtime-handoff' \
    'S82 publishes a runtime handoff before supervisor launch'
require_literal "$S82" 'refusing a competing one-shot safety owner while dcentrald is live' \
    'S82 safety refuses a competing live owner'
require_literal "$S82" 'emergency_hardware_safe_state' \
    'S82 has a monotonic post-owner emergency SafeOff path'
require_literal "$S82" 'physical rail remains unmeasured' \
    'S82 stop reporting preserves the software-vs-electrical evidence boundary'
require_literal "$S82" 'period/duty/enable evidence failed' \
    'S82 prelaunch cooling checks full PWM state rather than duty alone'
require_literal "$S82" 'HAD_VERIFIED_CHILD' \
    'S82 gives the verified parent a bounded terminal-receipt interval'
require_literal "$S82" '[ "$STATE" = Z ] || return 0' \
    'S82 distinguishes a non-executing zombie from a possible live owner'
if grep -Fq 'kill -TERM "$WRAPPER_PID"' "$S82" \
    || grep -Fq 'kill -9 "$WRAPPER_PID"' "$S82"; then
    fail 'S82 can signal a wrapper PID without start-time identity'
else
    pass 'S82 never signals an unverified wrapper PID'
fi

require_literal "$LATCH" 'execute the platform' \
    'session supervisor documents cut-before-journal ordering'
require_literal "$LATCH" 'REASON=${REASON}-safeoff-failed' \
    'session supervisor records emergency-cut failure in terminal status'
require_literal "$LATCH" '[ "$SAFETY_OK" -eq 1 ] || return 1' \
    'session supervisor cannot return success after emergency-cut failure'
SAFETY_LINE=$(grep -n -F 'if "$SAFETY_SCRIPT" safety' "$LATCH" | head -n 1 | cut -d: -f1)
PROMOTE_LINE=$(grep -n -F 'if ! promote_unresolved "$REASON"' "$LATCH" | head -n 1 | cut -d: -f1)
if [ -n "$SAFETY_LINE" ] && [ -n "$PROMOTE_LINE" ] && [ "$SAFETY_LINE" -lt "$PROMOTE_LINE" ]; then
    pass 'post-exit emergency cut precedes potentially blocking journal promotion'
else
    fail 'session supervisor can delay emergency cut behind journal promotion'
fi

STOP_LINE=$(grep -n -F 'run_init_scripts("stop");' "$INIT" | head -n 1 | cut -d: -f1)
TERM_LINE=$(grep -n -F 'libc::kill(-1, libc::SIGTERM)' "$INIT" | head -n 1 | cut -d: -f1)
KILL_LINE=$(grep -n -F 'libc::kill(-1, libc::SIGKILL)' "$INIT" | head -n 1 | cut -d: -f1)
if [ -n "$STOP_LINE" ] && [ -n "$TERM_LINE" ] && [ -n "$KILL_LINE" ] \
    && [ "$STOP_LINE" -lt "$TERM_LINE" ] && [ "$TERM_LINE" -lt "$KILL_LINE" ]; then
    pass 'PID 1 runs typed service teardown before residual TERM/KILL'
else
    fail 'PID 1 can kill the hardware owner before its typed stop path'
fi

require_literal "$HAL" 'parse_amlogic_boot_safe_handoff' \
    'HAL parses the versioned handoff as typed evidence'
require_literal "$HAL" 'validate_amlogic_boot_safe_handoff(expected)?;' \
    'NoPic admission revalidates the boot handoff before plug GPIO mutation'
require_literal "$HAL" '"state", "runtime-handoff"' \
    'HAL accepts only the supervisor-published runtime handoff state'
require_literal "$HAL" '"physical_rail_measured", "false"' \
    'HAL pins the non-electrical evidence grade'
require_literal "$HAL" 'let active_low = read_live("/sys/class/gpio/gpio437/active_low")?;' \
    'HAL revalidates raw active-high mode before runtime admission'

require_literal "$API_REBOOT" 'Transfer authority before claiming acceptance' \
    'API observes init request acceptance before reporting success'
API_HANDLER_BODY=$(sed -n '/^pub(super) async fn post_action_reboot/,/^pub(super) enum RebootRequestError/p' "$API_REBOOT")
API_PRE_TRANSFER=$(printf '%s\n' "$API_HANDLER_BODY" | sed '/trigger_system_reboot().await/q')
if printf '%s\n' "$API_PRE_TRANSFER" | grep -Eq 'push_rest_audit_free|tracing::'; then
    fail 'API can block on audit or logging before init receives reboot authority'
else
    pass 'API transfers reboot authority before audit and logging'
fi
if printf '%s\n' "$API_HANDLER_BODY" | grep -Eq 'tokio::spawn|tokio::time::sleep|Command::new\("dd"\)|Command::new\("sync"\)'; then
    fail 'API can delay or block before transferring reboot authority to init'
else
    pass 'API has no delay, detached task, entropy write, or sync before init acceptance'
fi
API_REBOOT_BODY=$(sed -n '/^pub(super) async fn trigger_system_reboot()/,/^\/\/\/ POST \/api\/action\/sleep/p' "$API_REBOOT")
if printf '%s\n' "$API_REBOOT_BODY" | grep -Eq '/proc/sysrq-trigger|\.arg\("-f"\)|REBOOT_FALLBACK_GRACE_SECS'; then
    fail 'API retains a competing sysrq, forced-reboot, or timer authority'
else
    pass 'API has no competing sysrq, forced-reboot, or timer authority'
fi
require_literal "$INIT" 'libc::clock_nanosleep(' \
    'PID 1 shutdown watchdog uses a monotonic absolute sleep'
require_literal "$INIT" 'libc::TIMER_ABSTIME' \
    'PID 1 shutdown deadline cannot be reset by EINTR retries'
ARM_LINE=$(grep -n -F 'arm_emergency_watchdog(rb_action, SHUTDOWN_WATCHDOG_MS);' "$INIT" | head -n 1 | cut -d: -f1)
SHUTDOWN_LOG_LINE=$(grep -n -F '"[init] Shutdown requested (signal {} -> {})"' "$INIT" | head -n 1 | cut -d: -f1)
ORDERLY_LINE=$(grep -n -F 'do_shutdown();' "$INIT" | head -n 1 | cut -d: -f1)
if [ -n "$ARM_LINE" ] && [ -n "$SHUTDOWN_LOG_LINE" ] && [ -n "$ORDERLY_LINE" ] \
    && [ "$ARM_LINE" -lt "$SHUTDOWN_LOG_LINE" ] && [ "$SHUTDOWN_LOG_LINE" -lt "$ORDERLY_LINE" ]; then
    pass 'PID 1 arms its terminal deadline before shutdown logging or orderly work'
else
    fail 'PID 1 can block before its terminal shutdown deadline exists'
fi
WATCHDOG_BODY=$(sed -n '/^fn arm_emergency_watchdog/,/^fn unmount_all/p' "$INIT")
WATCHDOG_SPAWN_LINE=$(printf '%s\n' "$WATCHDOG_BODY" | grep -n -F '.spawn(move ||' | head -n 1 | cut -d: -f1)
WATCHDOG_SYSRQ_LINE=$(printf '%s\n' "$WATCHDOG_BODY" | grep -n -F 'write_kernel_control_byte(b"/proc/sys/kernel/sysrq' | head -n 1 | cut -d: -f1)
if [ -n "$WATCHDOG_SPAWN_LINE" ] && [ -n "$WATCHDOG_SYSRQ_LINE" ] \
    && [ "$WATCHDOG_SPAWN_LINE" -lt "$WATCHDOG_SYSRQ_LINE" ]; then
    pass 'PID 1 creates the deadline thread before best-effort sysrq preparation'
else
    fail 'PID 1 can block in sysrq preparation before creating its deadline thread'
fi
EMERGENCY_BODY=$(sed -n '/^fn emergency_kernel_action/,/^fn install_signal_handlers/p' "$INIT")
if printf '%s\n' "$EMERGENCY_BODY" | grep -Eq 'libc::sync|eprintln!|println!|fs::write'; then
    fail 'PID 1 emergency path can block on sync, logging, or std::fs before reboot'
else
    pass 'PID 1 emergency path reaches terminal kernel action without sync, logging, or std::fs'
fi
SYSRQ_LINE=$(printf '%s\n' "$EMERGENCY_BODY" | grep -n -F 'write_kernel_control_byte(b"/proc/sysrq-trigger' | head -n 1 | cut -d: -f1)
RAW_REBOOT_LINE=$(printf '%s\n' "$EMERGENCY_BODY" | grep -n -F 'libc::reboot(rb_action)' | head -n 1 | cut -d: -f1)
if [ -n "$SYSRQ_LINE" ] && [ -n "$RAW_REBOOT_LINE" ] && [ "$SYSRQ_LINE" -lt "$RAW_REBOOT_LINE" ]; then
    pass 'PID 1 terminal deadline uses immediate sysrq before orderly reboot fallback'
else
    fail 'PID 1 terminal deadline can block in orderly reboot before immediate sysrq'
fi

require_literal "$RECOVERY" '/etc/init.d/S37board_setup start' \
    'Amlogic manual recovery re-establishes the checked boot baseline'
require_literal "$WEB_SERVER" 'manual_resolution_required' \
    'Amlogic web control reports the manual recovery policy'
if grep -Fq 'Run guarded restart' "$RECOVERY" \
    || grep -Fq 'Run guarded restart' "$DIAGNOSTIC" \
    || grep -Fq '["/etc/init.d/S82dcentrald", "restart"]' "$WEB_SERVER"; then
    fail 'Amlogic web recovery still advertises or executes forbidden restart'
else
    pass 'Amlogic web recovery does not advertise or execute forbidden restart'
fi

if [ "$FAILURES" -ne 0 ]; then
    printf 'Amlogic boot-safe-state contract failed: %s failure(s)\n' "$FAILURES" >&2
    exit 1
fi

printf 'Amlogic boot-safe-state contract passed.\n'
