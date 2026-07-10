#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Manifest-driven OTA package gate for DCENT_axe.
#
# Default mode is offline only: validate a generated package manifest and its
# payload files, then print the exact live-upload headers that would be used.
# Network, serial flashing, reboot waits, and rejection probes are disabled
# unless an operator explicitly opts in with the variables below.
#
# Inputs:
#   MANIFEST                 Path to *-manifest.json. If omitted, the script
#                            searches DIST_DIR (default: dist) and optional
#                            BOARD for exactly one manifest.
#   DIST_DIR                 Package root to search when MANIFEST is omitted.
#   BOARD                    Board target used to narrow manifest discovery.
#
# Live gates:
#   DCENT_OTA_E2E_LIVE=1     Allow HTTP contact with a device.
#   IP                       Target miner IP or host, required for live mode.
#   AUTH                     Bearer token value, required for live mode.
#   DCENT_OTA_E2E_UPLOAD=1   POST the update payload from the manifest.
#   DCENT_OTA_E2E_BAD_SIGNATURE=1
#                            Before upload, POST the same payload with a bad
#                            signature and require HTTP 400/401/403.
#   DCENT_OTA_E2E_FLASH=1    Flash the factory payload over serial.
#   COM_PORT                 Serial port for espflash.
#   DCENT_OTA_E2E_FLASH_CONFIRM=flash-<boardTarget>
#                            Required confirmation for serial flash.
#
# This script consumes the current package manifest shape emitted by
# package-firmware.sh/ps1 and uses the dashboard OTA headers:
#   X-DCENT-Board-Target, X-DCENT-Device-Model, X-DCENT-Payload-SHA256,
#   X-DCENT-Payload-Size, X-DCENT-Version, X-DCENT-Key-Id, X-DCENT-Signature.

set -eu

log() { printf '==> %s\n' "$*" >&2; }
fail() { printf '!!! %s\n' "$*" >&2; exit 1; }

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)

MANIFEST=${MANIFEST:-}
DIST_DIR=${DIST_DIR:-dist}
BOARD=${BOARD:-}

find_manifest() {
    search_root=$DIST_DIR
    if [ -n "$BOARD" ]; then
        search_root=$DIST_DIR/$BOARD
    fi

    if [ ! -d "$search_root" ]; then
        fail "Manifest search directory not found: $search_root"
    fi

    set -- $(find "$search_root" -type f -name '*-manifest.json' | sort)
    if [ "$#" -ne 1 ]; then
        find "$search_root" -type f -name '*-manifest.json' | sort >&2 || true
        fail "Expected exactly one manifest under $search_root; set MANIFEST explicitly"
    fi
    printf '%s\n' "$1"
}

if [ -z "$MANIFEST" ]; then
    cd "$ROOT_DIR"
    MANIFEST=$(find_manifest)
else
    case "$MANIFEST" in
        /*) ;;
        *) MANIFEST=$PWD/$MANIFEST ;;
    esac
fi

plan_file=$(mktemp)
trap 'rm -f "$plan_file" "$body_file"' EXIT
body_file=$(mktemp)

PYTHON=${PYTHON:-python}
"$PYTHON" "$SCRIPT_DIR/verify_ota_package.py" "$MANIFEST"

python - "$MANIFEST" "$plan_file" <<'PY'
import hashlib
import json
import os
import shlex
import sys

manifest_path, plan_path = sys.argv[1], sys.argv[2]
manifest_path = os.path.abspath(manifest_path)
base = os.path.dirname(manifest_path)

with open(manifest_path, encoding="ascii") as handle:
    manifest = json.load(handle)

def require(condition, message):
    if not condition:
        raise SystemExit(message)

require(manifest.get("schema") == 1, "manifest schema must be 1")
require(manifest.get("product") == "DCENT_axe", "manifest product mismatch")
require(manifest.get("family") == "bitaxe", "manifest family mismatch")
require(
    manifest.get("packageType") == "esp32-factory-and-ota-bundle",
    "manifest packageType mismatch",
)

board_target = manifest.get("boardTarget")
device_model = manifest.get("deviceModel")
version = manifest.get("version")
require(board_target, "manifest boardTarget is required")
require(device_model, "manifest deviceModel is required")
require(version, "manifest version is required")

ota = manifest.get("ota") or {}
require(ota.get("updateFitsSlot") is True, "manifest says OTA update does not fit slot")

payloads = {entry.get("name"): entry for entry in manifest.get("payloads", [])}
require("update" in payloads, "manifest missing update payload")
require("factory" in payloads, "manifest missing factory payload")

def resolve_payload(name):
    payload = payloads[name]
    rel = payload.get("path")
    require(rel, f"{name} payload path is required")
    path = os.path.abspath(os.path.join(base, rel))
    require(os.path.commonpath([base, path]) == base, f"{name} payload escapes manifest directory")
    require(os.path.isfile(path), f"{name} payload not found: {path}")
    data = open(path, "rb").read()
    size = len(data)
    sha = hashlib.sha256(data).hexdigest()
    require(payload.get("size") == size, f"{name} payload size mismatch")
    require(payload.get("sha256") == sha, f"{name} payload sha256 mismatch")
    return path, size, sha

update_path, update_size, update_sha = resolve_payload("update")
factory_path, factory_size, factory_sha = resolve_payload("factory")

slot_size = ota.get("slotSize")
if slot_size is not None:
    require(update_size <= int(slot_size), "update payload exceeds manifest OTA slot size")

endpoint = ((manifest.get("toolbox") or {}).get("uploadEndpoint")) or "/api/system/OTA"
key_id = manifest.get("otaKeyId") or manifest.get("keyId") or ""
signature = manifest.get("otaSignature") or ""

values = {
    "MANIFEST_ABS": manifest_path,
    "BOARD_TARGET": board_target,
    "DEVICE_MODEL": str(device_model).lower(),
    "VERSION": str(version),
    "ENDPOINT": endpoint,
    "UPDATE_PATH": update_path,
    "UPDATE_SIZE": str(update_size),
    "UPDATE_SHA": update_sha,
    "OTA_KEY_ID": str(key_id),
    "OTA_SIGNATURE": str(signature),
    "FACTORY_PATH": factory_path,
    "FACTORY_SIZE": str(factory_size),
    "FACTORY_SHA": factory_sha,
}

with open(plan_path, "w", encoding="ascii") as out:
    for key, value in values.items():
        out.write(f"{key}={shlex.quote(value)}\n")
PY

# shellcheck disable=SC1090
. "$plan_file"

log "Validated manifest: $MANIFEST_ABS"
log "Board target: $BOARD_TARGET; device model: $DEVICE_MODEL; version: $VERSION"
log "Update payload: $UPDATE_PATH ($UPDATE_SIZE bytes, sha256=$UPDATE_SHA)"

cat <<EOF
OTA upload plan:
  endpoint: $ENDPOINT
  X-DCENT-Board-Target: $BOARD_TARGET
  X-DCENT-Device-Model: $DEVICE_MODEL
  X-DCENT-Payload-SHA256: $UPDATE_SHA
  X-DCENT-Payload-Size: $UPDATE_SIZE
  X-DCENT-Version: $VERSION
  X-DCENT-Key-Id: $OTA_KEY_ID
  X-DCENT-Signature: <manifest otaSignature>
EOF

if [ "${DCENT_OTA_E2E_LIVE:-0}" != "1" ]; then
    log "Offline manifest/header validation passed. Set DCENT_OTA_E2E_LIVE=1 for device contact."
    exit 0
fi

: "${IP:?set IP for live OTA test}"
: "${AUTH:?set AUTH to the bearer token value for live OTA test}"
BASE="http://$IP"

info_json() {
    curl -sS \
        -H "Authorization: Bearer $AUTH" \
        -H "X-Requested-With: dcent-ota-e2e" \
        "$BASE/api/system/info"
}

json_version() {
    python -c 'import json,sys; print((json.load(sys.stdin).get("version") or ""))'
}

wait_for_version() {
    wanted=$1
    deadline=$(( $(date +%s) + ${DCENT_OTA_E2E_WAIT_SECONDS:-180} ))
    log "Waiting for /api/system/info version=$wanted"
    while [ "$(date +%s)" -lt "$deadline" ]; do
        if version=$(info_json 2>/dev/null | json_version 2>/dev/null); then
            if [ "$version" = "$wanted" ]; then
                log "Device reports version=$version"
                return 0
            fi
        fi
        sleep 3
    done
    fail "Timed out waiting for version=$wanted"
}

post_update() {
    signature=$1
    expected=$2
    curl -sS -o "$body_file" -w '%{http_code}' \
        -X POST \
        -H "Authorization: Bearer $AUTH" \
        -H "X-Requested-With: dcent-ota-e2e" \
        -H "Content-Type: application/octet-stream" \
        -H "X-DCENT-Board-Target: $BOARD_TARGET" \
        -H "X-DCENT-Device-Model: $DEVICE_MODEL" \
        -H "X-DCENT-Payload-SHA256: $UPDATE_SHA" \
        -H "X-DCENT-Payload-Size: $UPDATE_SIZE" \
        -H "X-DCENT-Version: $VERSION" \
        -H "X-DCENT-Key-Id: $OTA_KEY_ID" \
        -H "X-DCENT-Signature: $signature" \
        --data-binary "@$UPDATE_PATH" \
        "$BASE$ENDPOINT" >"$body_file.status"
    status=$(cat "$body_file.status")
    rm -f "$body_file.status"

    case " $expected " in
        *" $status "*) return 0 ;;
        *)
            cat "$body_file" >&2 || true
            fail "Unexpected OTA HTTP status $status; expected one of:$expected"
            ;;
    esac
}

if [ "${DCENT_OTA_E2E_FLASH:-0}" = "1" ]; then
    : "${COM_PORT:?set COM_PORT for serial flash}"
    expected_confirm="flash-$BOARD_TARGET"
    if [ "${DCENT_OTA_E2E_FLASH_CONFIRM:-}" != "$expected_confirm" ]; then
        fail "Set DCENT_OTA_E2E_FLASH_CONFIRM=$expected_confirm to allow serial flash"
    fi
    log "Flashing merged factory payload over serial port $COM_PORT"
    espflash write-bin --port "$COM_PORT" 0x0 "$FACTORY_PATH"
    wait_for_version "$VERSION"
fi

if [ "${DCENT_OTA_E2E_BAD_SIGNATURE:-0}" = "1" ]; then
    [ -n "$OTA_KEY_ID" ] || fail "Live bad-signature probe requires manifest otaKeyId/keyId"
    bad_sig=00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000
    log "Posting bad-signature probe; expecting rejection"
    post_update "$bad_sig" " 400 401 403 "
    log "Bad-signature probe rejected"
fi

if [ "${DCENT_OTA_E2E_UPLOAD:-0}" = "1" ]; then
    [ -n "$OTA_KEY_ID" ] || fail "Live OTA upload requires manifest otaKeyId/keyId"
    [ -n "$OTA_SIGNATURE" ] || fail "Live OTA upload requires manifest otaSignature"
    log "Posting signed OTA update from manifest"
    post_update "$OTA_SIGNATURE" " 200 202 "
    wait_for_version "$VERSION"
else
    log "Live device contact enabled, but upload skipped. Set DCENT_OTA_E2E_UPLOAD=1 to POST update."
fi

log "OTA manifest-driven gate passed"
