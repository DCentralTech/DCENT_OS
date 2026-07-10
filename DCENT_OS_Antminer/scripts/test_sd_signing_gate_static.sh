#!/bin/sh
#
# Offline functional self-test for the raw SD image signing gate.

set -eu

SCRIPT_DIR=$(CDPATH= cd "$(dirname "$0")" && pwd)
# shellcheck source=lib/sd_image_signing_gate.sh
. "$SCRIPT_DIR/lib/sd_image_signing_gate.sh"

tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT INT TERM

complete_img="$tmpdir/complete.img"
complete_manifest="$complete_img.manifest.json"
incomplete_img="$tmpdir/incomplete.img"
incomplete_manifest="$incomplete_img.manifest.json"
inconsistent_img="$tmpdir/inconsistent.img"
inconsistent_manifest="$inconsistent_img.manifest.json"
missing_img="$tmpdir/missing-manifest.img"
lab_img="$tmpdir/lab.img"
lab_manifest="$lab_img.manifest.json"

: > "$complete_img"
cat > "$complete_manifest" <<'EOF'
{
  "boot_artifacts_complete": true,
  "artifacts": {
    "BOOT.bin": true,
    "uImage": true,
    "devicetree.dtb": true,
    "uEnv.txt": true,
    "bitstream": true,
    "rootfs": true
  }
}
EOF

: > "$incomplete_img"
cat > "$incomplete_manifest" <<'EOF'
{
  "boot_artifacts_complete": false,
  "artifacts": {
    "BOOT.bin": false,
    "uImage": true,
    "devicetree.dtb": true,
    "uEnv.txt": false,
    "bitstream": true,
    "rootfs": true
  }
}
EOF

: > "$inconsistent_img"
cat > "$inconsistent_manifest" <<'EOF'
{
  "boot_artifacts_complete": true,
  "artifacts": {
    "BOOT.bin": true,
    "uImage": true,
    "devicetree.dtb": false,
    "uEnv.txt": true,
    "bitstream": true,
    "rootfs": true
  }
}
EOF

: > "$missing_img"

: > "$lab_img"
cat > "$lab_manifest" <<'EOF'
{
  "boot_artifacts_complete": false,
  "artifacts": {
    "BOOT.bin": false,
    "uImage": true,
    "devicetree.dtb": true,
    "uEnv.txt": false,
    "bitstream": true,
    "rootfs": true
  }
}
EOF

dcent_sd_require_complete_manifest_for_signing "$complete_img" "$complete_manifest"

if dcent_sd_require_complete_manifest_for_signing "$incomplete_img" "$incomplete_manifest" >/dev/null 2>&1; then
    echo "FAIL: incomplete manifest was accepted for release signing" >&2
    exit 1
fi

if dcent_sd_require_complete_manifest_for_signing "$inconsistent_img" "$inconsistent_manifest" >/dev/null 2>&1; then
    echo "FAIL: inconsistent manifest was accepted for release signing" >&2
    exit 1
fi

if dcent_sd_require_complete_manifest_for_signing "$missing_img" "$missing_img.manifest.json" >/dev/null 2>&1; then
    echo "FAIL: missing manifest was accepted for release signing" >&2
    exit 1
fi

renamed=$(dcent_sd_mark_incomplete_lab_image "$lab_img" "$lab_manifest")
case "$renamed" in
    *-UNSIGNED-LAB-ROOTFS-ONLY.img) ;;
    *)
        echo "FAIL: lab image was not relabelled: $renamed" >&2
        exit 1
        ;;
esac

[ -f "$renamed" ] || { echo "FAIL: relabelled lab image missing" >&2; exit 1; }
[ -f "$renamed.manifest.json" ] || { echo "FAIL: relabelled lab manifest missing" >&2; exit 1; }
[ ! -f "$lab_img" ] || { echo "FAIL: original lab image still exists" >&2; exit 1; }

echo "sd signing gate static self-test passed"
