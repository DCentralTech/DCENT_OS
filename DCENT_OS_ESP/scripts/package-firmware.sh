#!/bin/sh

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)

TARGET_DIR=${TARGET_DIR:-${1:-C:/bt/xtensa-esp32s3-espidf/release}}
BOARD_TARGET=${BOARD_TARGET:-${2:-}}
VERSION=${VERSION:-${3:-}}
OUT_DIR=${OUT_DIR:-${4:-}}
DEVICE_MODEL=${DCENT_DEVICE_MODEL:-}
ESPTOOL=${ESPTOOL:-python}
ESPTOOL_MODULE=${ESPTOOL_MODULE:-esptool}
PYTHON=${PYTHON:-python}
ELF_PATH=${ELF_PATH:-}
PARTITIONS_CSV=${PARTITIONS_CSV:-$ROOT_DIR/partitions.csv}
OTA_APP_PARTITION=${DCENT_OTA_APP_PARTITION:-ota_0}
SIGNING_KEY_PEM=${DCENT_OTA_PRIVATE_KEY_PEM:-}
SIGNING_KEY_ID=${DCENT_OTA_KEY_ID:-}
PUBLIC_KEY_HEX=${DCENT_OTA_PUBLIC_KEY_HEX:-}
ENFORCE_SIGNED_OTA=${DCENT_ENFORCE_SIGNED_OTA:-}

if [ -z "$BOARD_TARGET" ]; then
    printf '%s\n' "BOARD_TARGET is required" >&2
    exit 1
fi

device_model_for_board_target() {
    case "$1" in
        bitaxe-max) printf '%s\n' "max" ;;
        bitaxe-ultra) printf '%s\n' "ultra" ;;
        bitaxe-supra) printf '%s\n' "supra" ;;
        bitaxe-gamma) printf '%s\n' "gamma" ;;
        bitaxe-gamma-duo) printf '%s\n' "gammaduo" ;;
        bitaxe-gt) printf '%s\n' "gammaturbo" ;;
        bitaxe-touch) printf '%s\n' "touch" ;;
        bitaxe-gt-touch) printf '%s\n' "gt_touch" ;;
        bitaxe-hex-ultra) printf '%s\n' "hexultra" ;;
        bitaxe-hex-supra) printf '%s\n' "suprahex" ;;
        nerdnos) printf '%s\n' "nerdnos" ;;
        nerdaxe) printf '%s\n' "nerdaxe" ;;
        nerdqaxe-plus) printf '%s\n' "nerdqaxeplus" ;;
        nerdqaxe-pp) printf '%s\n' "nerdqaxepp" ;;
        dcent-axe-bm1397) printf '%s\n' "dcentaxe_bm1397" ;;
        dcent-axe-quad-bm1397) printf '%s\n' "dcentaxe_quad_bm1397" ;;
        dcent-axe-hex-bm1397) printf '%s\n' "dcentaxe_hex_bm1397" ;;
        *)
            printf '%s\n' "Unknown BOARD_TARGET '$1'" >&2
            exit 1
            ;;
    esac
}

DEFAULT_DEVICE_MODEL=$(device_model_for_board_target "$BOARD_TARGET")

if [ -z "$VERSION" ]; then
    VERSION=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT_DIR/Cargo.toml" | sed -n '1p')
fi

if [ -z "$OUT_DIR" ]; then
    OUT_DIR="$ROOT_DIR/dist/$BOARD_TARGET"
fi

if [ -z "$DEVICE_MODEL" ]; then
    DEVICE_MODEL="$DEFAULT_DEVICE_MODEL"
fi

if [ -z "$ELF_PATH" ]; then
    ELF_PATH="$TARGET_DIR/dcentaxe"
fi

BOOTLOADER=""
for candidate in "$TARGET_DIR"/build/esp-idf-sys-*/out/build/bootloader/bootloader.bin; do
    if [ -f "$candidate" ]; then
        BOOTLOADER=$candidate
        break
    fi
done

PARTITION_TABLE=""
for candidate in "$TARGET_DIR"/build/esp-idf-sys-*/out/build/partition_table/partition-table.bin; do
    if [ -f "$candidate" ]; then
        PARTITION_TABLE=$candidate
        break
    fi
done

OTA_DATA=""
for candidate in "$TARGET_DIR"/build/esp-idf-sys-*/out/build/ota_data_initial.bin; do
    if [ -f "$candidate" ]; then
        OTA_DATA=$candidate
        break
    fi
done

require_path() {
    if [ ! -f "$1" ]; then
        printf '%s\n' "$2 not found: $1" >&2
        exit 1
    fi
}

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        printf '%s\n' "No SHA-256 tool available (need sha256sum or shasum)" >&2
        exit 1
    fi
}

ota_slot_size_bytes() {
    "$PYTHON" -c '
import csv
import sys

path, partition = sys.argv[1], sys.argv[2]
with open(path, newline="") as handle:
    rows = (
        line for line in handle
        if line.strip() and not line.lstrip().startswith("#")
    )
    for row in csv.reader(rows):
        cols = [col.strip() for col in row]
        if len(cols) >= 5 and cols[0] == partition and cols[1] == "app":
            print(int(cols[4], 0))
            break
    else:
        raise SystemExit(f"{partition} app partition not found in {path}")
' "$PARTITIONS_CSV" "$OTA_APP_PARTITION"
}

partition_offset_bytes() {
    "$PYTHON" -c '
import csv
import sys

path, partition = sys.argv[1], sys.argv[2]
with open(path, newline="") as handle:
    rows = (
        line for line in handle
        if line.strip() and not line.lstrip().startswith("#")
    )
    for row in csv.reader(rows):
        cols = [col.strip() for col in row]
        if len(cols) >= 5 and cols[0] == partition:
            print(int(cols[3], 0))
            break
    else:
        raise SystemExit(f"{partition} partition not found in {path}")
' "$PARTITIONS_CSV" "$1"
}

sign_ota_metadata() {
    message=$1
    if [ -z "$SIGNING_KEY_PEM" ] || [ ! -f "$SIGNING_KEY_PEM" ]; then
        printf '%s\n' ""
        return 0
    fi
    if ! command -v openssl >/dev/null 2>&1; then
        printf '%s\n' "openssl not available for OTA signing" >&2
        exit 1
    fi
    msg_file=$(mktemp)
    sig_file=$(mktemp)
    printf '%s' "$message" >"$msg_file"
    openssl pkeyutl -sign -rawin -inkey "$SIGNING_KEY_PEM" -in "$msg_file" -out "$sig_file" >/dev/null 2>&1
    xxd -p -c 512 "$sig_file" | tr -d '\n'
    rm -f "$msg_file" "$sig_file"
}

if [ -n "$ENFORCE_SIGNED_OTA" ]; then
    if [ -z "$SIGNING_KEY_PEM" ] || [ ! -f "$SIGNING_KEY_PEM" ]; then
        printf '%s\n' "Signed OTA packaging requires DCENT_OTA_PRIVATE_KEY_PEM" >&2
        exit 1
    fi
    if [ -z "$PUBLIC_KEY_HEX" ]; then
        printf '%s\n' "Signed OTA packaging requires DCENT_OTA_PUBLIC_KEY_HEX" >&2
        exit 1
    fi
    if [ -z "$SIGNING_KEY_ID" ]; then
        printf '%s\n' "Signed OTA packaging requires DCENT_OTA_KEY_ID" >&2
        exit 1
    fi
fi

require_path "$ELF_PATH" "ELF"
require_path "$BOOTLOADER" "Bootloader"
require_path "$PARTITION_TABLE" "Partition table"
require_path "$OTA_DATA" "OTA data"
require_path "$PARTITIONS_CSV" "Partition CSV"
OTA_APP_OFFSET=$(partition_offset_bytes "$OTA_APP_PARTITION")
OTA_DATA_OFFSET=$(partition_offset_bytes "otadata")

if [ -n "$SIGNING_KEY_PEM" ] && [ -n "$PUBLIC_KEY_HEX" ]; then
    DERIVED_PUBLIC=$(openssl pkey -in "$SIGNING_KEY_PEM" -pubout -outform DER | tail -c 32 | xxd -p -c 256 | tr -d '\n')
    EXPECTED_PUBLIC=$(printf '%s' "$PUBLIC_KEY_HEX" | tr '[:upper:]' '[:lower:]')
    if [ "$DERIVED_PUBLIC" != "$EXPECTED_PUBLIC" ]; then
        printf '%s\n' "Signing private key does not match DCENT_OTA_PUBLIC_KEY_HEX" >&2
        exit 1
    fi
fi

mkdir -p "$OUT_DIR"

PREFIX=${DCENT_RELEASE_STEM:-dcentaxe-$BOARD_TARGET-$VERSION}
UPDATE_BIN="$OUT_DIR/$PREFIX-update.bin"
FACTORY_BIN="$OUT_DIR/$PREFIX-factory.bin"
MANIFEST_PATH="$OUT_DIR/$PREFIX-manifest.json"
CHECKSUMS_PATH="$OUT_DIR/$PREFIX-SHA256SUMS.txt"

"$ESPTOOL" -m "$ESPTOOL_MODULE" --chip esp32s3 elf2image --output "$UPDATE_BIN" "$ELF_PATH"
"$ESPTOOL" -m "$ESPTOOL_MODULE" --chip esp32s3 merge_bin --flash_mode dio --flash_size 16MB --flash_freq 80m \
    0x0 "$BOOTLOADER" \
    0x8000 "$PARTITION_TABLE" \
    "$OTA_DATA_OFFSET" "$OTA_DATA" \
    "$OTA_APP_OFFSET" "$UPDATE_BIN" \
    -o "$FACTORY_BIN"

UPDATE_SHA=$(sha256_file "$UPDATE_BIN")
FACTORY_SHA=$(sha256_file "$FACTORY_BIN")
BOOTLOADER_SHA=$(sha256_file "$BOOTLOADER")
PARTITION_TABLE_SHA=$(sha256_file "$PARTITION_TABLE")
OTA_DATA_SHA=$(sha256_file "$OTA_DATA")
CREATED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
UPDATE_SIZE=$(wc -c <"$UPDATE_BIN" | tr -d ' ')
FACTORY_SIZE=$(wc -c <"$FACTORY_BIN" | tr -d ' ')
BOOTLOADER_SIZE=$(wc -c <"$BOOTLOADER" | tr -d ' ')
PARTITION_TABLE_SIZE=$(wc -c <"$PARTITION_TABLE" | tr -d ' ')
OTA_DATA_SIZE=$(wc -c <"$OTA_DATA" | tr -d ' ')
OTA_SLOT_SIZE=$(ota_slot_size_bytes)
# XPH-7: compute the slot-fit verdict instead of hard-coding `true` in the
# manifest. The hard exit-1 guard below stays load-bearing (an over-slot image
# must NEVER ship), but `UPDATE_FITS_SLOT` is now derived from the same size
# comparison so a future refactor that removes the exit can't leave a stale,
# dishonest `"updateFitsSlot": true` for a truncated image.
if [ "$UPDATE_SIZE" -gt "$OTA_SLOT_SIZE" ]; then
    UPDATE_FITS_SLOT=false
    printf '%s\n' "Update image exceeds $OTA_APP_PARTITION slot: $UPDATE_SIZE > $OTA_SLOT_SIZE bytes" >&2
    exit 1
else
    UPDATE_FITS_SLOT=true
fi
OTA_MESSAGE=$(printf 'schema=2\nboard_target=%s\ndevice_model=%s\nversion=%s\nsize=%s\nsha256=%s\n' "$BOARD_TARGET" "$(printf '%s' "$DEVICE_MODEL" | tr '[:upper:]' '[:lower:]')" "$VERSION" "$UPDATE_SIZE" "$UPDATE_SHA")
BUNDLE_MESSAGE=$(printf 'schema=2\nboard_target=%s\ndevice_model=%s\nversion=%s\nupdate_size=%s\nupdate_sha256=%s\nfactory_size=%s\nfactory_sha256=%s\n' "$BOARD_TARGET" "$(printf '%s' "$DEVICE_MODEL" | tr '[:upper:]' '[:lower:]')" "$VERSION" "$UPDATE_SIZE" "$UPDATE_SHA" "$FACTORY_SIZE" "$FACTORY_SHA")
OTA_SIGNATURE=$(sign_ota_metadata "$OTA_MESSAGE")
BUNDLE_SIGNATURE=$(sign_ota_metadata "$BUNDLE_MESSAGE")
if [ -n "$ENFORCE_SIGNED_OTA" ] && [ -z "$OTA_SIGNATURE" ]; then
    printf '%s\n' "Signed OTA packaging required but no signature was produced" >&2
    exit 1
fi
if { [ -n "$OTA_SIGNATURE" ] || [ -n "$BUNDLE_SIGNATURE" ]; } && [ -z "$SIGNING_KEY_ID" ]; then
    printf '%s\n' "Signed OTA packaging requires DCENT_OTA_KEY_ID" >&2
    exit 1
fi

cat >"$MANIFEST_PATH" <<EOF
{
  "schema": 1,
  "product": "DCENT_axe",
  "family": "bitaxe",
  "packageType": "esp32-factory-and-ota-bundle",
  "boardTarget": "$BOARD_TARGET",
  "deviceModel": "$DEVICE_MODEL",
  "version": "$VERSION",
  "createdAtUtc": "$CREATED_AT",
  "ota": {
    "appPartition": "$OTA_APP_PARTITION",
    "slotSize": $OTA_SLOT_SIZE,
    "updateFitsSlot": $UPDATE_FITS_SLOT
  },
  "signatureAlgorithm": $(if [ -n "$BUNDLE_SIGNATURE" ]; then printf '"ed25519"'; else printf 'null'; fi),
  "keyId": $(if [ -n "$BUNDLE_SIGNATURE" ] && [ -n "$SIGNING_KEY_ID" ]; then printf '"%s"' "$SIGNING_KEY_ID"; else printf 'null'; fi),
  "signature": $(if [ -n "$BUNDLE_SIGNATURE" ]; then printf '"%s"' "$BUNDLE_SIGNATURE"; else printf 'null'; fi),
  "_signatureNote": "AOTA-5: the top-level 'signature' covers factory_size+factory_sha256 (the full factory image) and is NOT verified by on-device firmware — DCENT_axe only enforces 'otaSignature' over the schema-2 OTA update message (size+sha of the update payload) at the /api/system/OTA handler. The bundle 'signature' is for a serial-flash verifier (DCENT Toolbox) to check factory_sha256 against the compiled key before flashing factory.bin. Do not present a factory install as device-verified on the strength of this field alone.",
  "bundleSignatureVerifiedOnDevice": false,
  "deviceEnforcedSignature": "otaSignature",
  "factorySha256": "$FACTORY_SHA",
  "factoryFlashMap": [
    {
      "name": "bootloader",
      "offset": 0,
      "size": $BOOTLOADER_SIZE,
      "sha256": "$BOOTLOADER_SHA"
    },
    {
      "name": "partition-table",
      "offset": 32768,
      "size": $PARTITION_TABLE_SIZE,
      "sha256": "$PARTITION_TABLE_SHA"
    },
    {
      "name": "ota-data-initial",
      "offset": $OTA_DATA_OFFSET,
      "size": $OTA_DATA_SIZE,
      "sha256": "$OTA_DATA_SHA"
    },
    {
      "name": "update",
      "offset": $OTA_APP_OFFSET,
      "size": $UPDATE_SIZE,
      "sha256": "$UPDATE_SHA"
    }
  ],
  "otaSignatureAlgorithm": $(if [ -n "$OTA_SIGNATURE" ]; then printf '"ed25519"'; else printf 'null'; fi),
  "otaKeyId": $(if [ -n "$OTA_SIGNATURE" ] && [ -n "$SIGNING_KEY_ID" ]; then printf '"%s"' "$SIGNING_KEY_ID"; else printf 'null'; fi),
  "otaSignature": $(if [ -n "$OTA_SIGNATURE" ]; then printf '"%s"' "$OTA_SIGNATURE"; else printf 'null'; fi),
  "payloads": [
    {
      "name": "factory",
      "path": "$(basename "$FACTORY_BIN")",
      "size": $FACTORY_SIZE,
      "sha256": "$FACTORY_SHA"
    },
    {
      "name": "update",
      "path": "$(basename "$UPDATE_BIN")",
      "size": $UPDATE_SIZE,
      "sha256": "$UPDATE_SHA"
    },
    {
      "name": "manifest",
      "path": "$(basename "$MANIFEST_PATH")",
      "size": null,
      "sha256": null
    }
  ],
  "toolbox": {
    "installCommand": "dcent flash --serial <port> -f $(basename "$FACTORY_BIN")",
    "updateCommand": "dcent ota update <ip> -f $(basename "$UPDATE_BIN")",
    "uploadEndpoint": "/api/system/OTA",
    "boardTargetHeader": "X-DCENT-Board-Target",
    "deviceModelHeader": "X-DCENT-Device-Model",
    "requiresInactiveSlot": true
  }
}
EOF

MANIFEST_SHA=$(sha256_file "$MANIFEST_PATH")

cat >"$CHECKSUMS_PATH" <<EOF
$FACTORY_SHA  $(basename "$FACTORY_BIN")
$UPDATE_SHA  $(basename "$UPDATE_BIN")
$MANIFEST_SHA  $(basename "$MANIFEST_PATH")
EOF

printf '%s\n' "Factory package: $FACTORY_BIN"
printf '%s\n' "Update package:  $UPDATE_BIN"
printf '%s\n' "Manifest:        $MANIFEST_PATH"
printf '%s\n' "Checksums:       $CHECKSUMS_PATH"
