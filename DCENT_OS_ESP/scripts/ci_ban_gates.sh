#!/usr/bin/env bash
# Source-text ban gates for DCENT_OS-for-ESP.
#
# Keep this hardware-free and toolchain-free: it must run from GitHub Actions,
# local `make verify`, and the root DCENT_OS pre-commit hook.

set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TMPDIR="${TMPDIR:-/tmp}/dcentos-esp-ban-gates.$$"

mkdir -p "$TMPDIR"
trap 'rm -rf "$TMPDIR"' EXIT

cd "$PROJECT_ROOT"

fail=0

gate() {
    local label=$1
    local status=$2
    if [ "$status" -ne 0 ]; then
        echo "::error::BAN-GATE: $label"
        fail=1
    else
        echo "ok: $label"
    fi
}

# [1] BM1370 job-id extraction mask regression. The correct mask is
# `(id & 0xf0) >> 1`; the old broken mask `id & 0x70` dropped bit 7
# and lost most nonces.
if grep -rEn 'job_id[^=]*=[^/]*\bid\b *& *0x70' \
     dcentaxe-asic/src dcentaxe-mining/src >"$TMPDIR/g1" 2>/dev/null; then
    cat "$TMPDIR/g1"
    gate "BM1370 job-id mask must be (id & 0xf0) >> 1, never id & 0x70" 1
else
    gate "BM1370 job-id mask must be (id & 0xf0) >> 1, never id & 0x70" 0
fi

# [2] BIP320 version-rolling reject-all guard. Re-adding a
# `version_bits_raw != 0` / `== 0` reject discards rolled work.
if grep -rEn 'version_bits_raw *(!=|==) *0' \
     dcentaxe-asic/src dcentaxe-mining/src dcentaxe/src 2>/dev/null \
     | grep -vE ':[0-9]+: *(///|//|\*)' >"$TMPDIR/g2"; then
    cat "$TMPDIR/g2"
    gate "no version_bits_raw reject-all guard (BIP320 rolled work must survive)" 1
else
    gate "no version_bits_raw reject-all guard (BIP320 rolled work must survive)" 0
fi

# [3] Canonical BIP320 mask constant must stay 0x1FFFE000.
if grep -rqE 'STRATUM_DEFAULT_VERSION_MASK: *u32 *= *0x1FFFE000' \
     dcentaxe-asic/src/common.rs \
   && grep -rqE 'BIP320_DEFAULT_VERSION_MASK: *u32 *= *0x1FFFE000' \
     dcentaxe-mining/src/dispatcher.rs; then
    gate "canonical BIP320 mask constants are 0x1FFFE000" 0
else
    gate "canonical BIP320 mask constants are 0x1FFFE000" 1
fi

# [4] SV2 set_test_keys must stay #[cfg(test)]-gated: a release build must
# never ship a shared deterministic transport key.
if awk '
      /pub fn set_test_keys/ {
        if (prev !~ /#\[cfg\(test\)\]/) {
          print FILENAME":"FNR": ungated set_test_keys"; found=1
        }
      }
      { prev=$0 }
      END { exit found?1:0 }
    ' dcentaxe-stratum-v2/src/*.rs; then
    gate "SV2 set_test_keys stays #[cfg(test)]-gated" 0
else
    gate "SV2 set_test_keys stays #[cfg(test)]-gated" 1
fi

# [5] AOTA-1: a signature-capable build must stay fail-closed against an
# unauthenticated caller flipping allow_unsigned_ota and flashing an unsigned
# image. Both fail-closed legs must be present.
if grep -q 'crate::ota_signature::owner_action_authorized' dcentaxe/src/api.rs 2>/dev/null \
     && grep -q 'crate::ota_signature::ota_signature_enforced' dcentaxe/src/api.rs 2>/dev/null; then
    gate "AOTA-1 fail-closed legs present (owner_action_authorized + ota_signature_enforced)" 0
else
    gate "AOTA-1 fail-closed legs present (owner_action_authorized + ota_signature_enforced)" 1
fi

# [6] AOTA-4: owner-claim skip flag must not be revived as a write path.
# `claim_skip` may only be READ, never SET by a handler.
if grep -rEn 'set_u8\("claim_skip"|set_str\("claim_skip"|\.set[^(]*\("claim_skip"' \
     dcentaxe/src 2>/dev/null >"$TMPDIR/g6"; then
    cat "$TMPDIR/g6"
    gate "claim_skip is read-only (never written by a handler)" 1
else
    gate "claim_skip is read-only (never written by a handler)" 0
fi

# [7] Main task stack must stay >= 24576. The OLED carousel overflows a
# smaller stack.
sz=$(grep -E '^CONFIG_ESP_MAIN_TASK_STACK_SIZE=' sdkconfig.defaults \
       | head -1 | cut -d= -f2 | tr -dc '0-9')
if [ -n "$sz" ] && [ "$sz" -ge 24576 ]; then
    gate "CONFIG_ESP_MAIN_TASK_STACK_SIZE >= 24576 (got $sz)" 0
else
    gate "CONFIG_ESP_MAIN_TASK_STACK_SIZE >= 24576 (got '${sz:-unset}')" 1
fi

# [8] panic=abort must stay pinned for release: an unwinding panic while
# holding a SharedState Mutex poisons it.
if awk '
      /^\[/ { in_rel = ($0 ~ /^\[profile\.release\]/) }
      in_rel && /panic *= *"abort"/ { ok = 1 }
      END { exit ok?0:1 }
    ' Cargo.toml; then
    gate "release profile pins panic = \"abort\"" 0
else
    gate "release profile pins panic = \"abort\"" 1
fi

# [9] Mesh owner-auth must never regress to the old placeholder string equality.
# `tag == expected_token` (the scaffold's plain compare) re-opens a passwordless
# air-gap-control bypass AND a timing oracle on the owner key.
if grep -rEn 'tag *== *expected_token' \
     dcentaxe-lora/src/mesh.rs dcentaxe-lora/src/auth.rs 2>/dev/null \
     | grep -vE ':[0-9]+: *(///|//|\*|!)' >"$TMPDIR/g9"; then
    cat "$TMPDIR/g9"
    gate "no placeholder string-equality owner-auth (use the constant-time MAC)" 1
else
    gate "no placeholder string-equality owner-auth (use the constant-time MAC)" 0
fi

# [10] The mesh owner-auth verify must use the subtle-backed constant-time
# `Mac::verify_slice`, so a byte-compare timing oracle can never leak the key.
if grep -rq 'verify_slice' dcentaxe-lora/src/auth.rs 2>/dev/null; then
    gate "mesh owner-auth uses constant-time verify_slice" 0
else
    gate "mesh owner-auth uses constant-time verify_slice" 1
fi

# [11] Every mesh transmit must pass the region duty-cycle governor. If the LoRa
# radio task exists, it MUST reference the airtime governor `try_acquire` — a
# beacon/relay TX path that skips it can bust the EU 1% / NA dwell envelope.
if [ -f dcentaxe/src/lora_task.rs ]; then
    if grep -rq 'try_acquire' dcentaxe/src/lora_task.rs 2>/dev/null; then
        gate "lora_task duty-governs every transmit (try_acquire present)" 0
    else
        gate "lora_task duty-governs every transmit (try_acquire present)" 1
    fi
else
    gate "lora_task duty-gate (n/a — lora feature not wired here)" 0
fi

echo "----"
if [ "$fail" -ne 0 ]; then
    echo "One or more ban-gates fired. A fixed anti-pattern was re-introduced."
    exit 1
fi

echo "All ban-gates clean."
