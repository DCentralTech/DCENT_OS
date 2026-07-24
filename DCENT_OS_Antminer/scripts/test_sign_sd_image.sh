#!/bin/sh
# Adversarial host-side tests for manifest-bound SD-image signing.
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
SIGNER="$SCRIPT_DIR/sign_sd_image.sh"
TEST_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/dcentos-sd-signing.XXXXXX")
cleanup() {
    chmod -R u+w "$TEST_ROOT" 2>/dev/null || true
    rm -rf "$TEST_ROOT"
}
trap cleanup EXIT HUP INT TERM

fail_test() {
    echo "SD image signing test failed: $*" >&2
    exit 1
}

command -v openssl >/dev/null 2>&1 || fail_test "openssl is unavailable"
command -v sha256sum >/dev/null 2>&1 || fail_test "sha256sum is unavailable"
PYTHON=''
for candidate in python3 python; do
    if command -v "$candidate" >/dev/null 2>&1 &&
        "$candidate" -c \
            'import sys; raise SystemExit(0 if sys.version_info >= (3, 10) else 1)' \
            >/dev/null 2>&1; then
        PYTHON=$candidate
        break
    fi
done
[ -n "$PYTHON" ] || fail_test "Python 3.10 or newer is unavailable"

is_link_like() {
    "$PYTHON" - "$1" <<'PY'
from pathlib import Path
import stat
import sys

metadata = Path(sys.argv[1]).lstat()
is_reparse = bool(
    getattr(metadata, "st_file_attributes", 0)
    & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
)
raise SystemExit(0 if stat.S_ISLNK(metadata.st_mode) or is_reparse else 1)
PY
}

openssl genpkey -algorithm Ed25519 -out "$TEST_ROOT/private.pem" >/dev/null 2>&1
openssl pkey -in "$TEST_ROOT/private.pem" -pubout \
    -out "$TEST_ROOT/public.pem" >/dev/null 2>&1
openssl genpkey -algorithm Ed25519 -out "$TEST_ROOT/wrong-private.pem" >/dev/null 2>&1
openssl pkey -in "$TEST_ROOT/wrong-private.pem" -pubout \
    -out "$TEST_ROOT/wrong-public.pem" >/dev/null 2>&1
if [ "$("$PYTHON" -c 'import os; print(os.name)')" = nt ]; then
    "$PYTHON" - "$SCRIPT_DIR" "$TEST_ROOT/private.pem" \
        "$TEST_ROOT/wrong-private.pem" <<'PY'
from pathlib import Path
import sys

sys.path.insert(0, sys.argv[1])
import release_set_publication as release_io

for value in sys.argv[2:]:
    path = Path(value)
    release_io.set_windows_file_acl(path, release_io.WINDOWS_PRIVATE_FILE_SDDL)
    release_io.require_private_windows_acl(path, "test private key")
PY
else
    chmod 600 "$TEST_ROOT/private.pem" "$TEST_ROOT/wrong-private.pem"
fi

write_manifest() {
    image=$1
    manifest=$2
    digest=${3:-$(sha256sum "$image" | awk '{print $1}')}
    complete=${4:-true}
    size=$(wc -c < "$image" | tr -d '[:space:]')
    cat > "$manifest" <<EOF
{
  "schema": "dcentos.am2_s19jpro_sd_image_manifest.v2",
  "target": "am2-s19jpro-sd",
  "image": "$(basename "$image")",
  "image_size_bytes": $size,
  "image_sha256": "$digest",
  "boot_artifacts_complete": $complete,
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
}

printf 'synthetic complete SD image\n' > "$TEST_ROOT/complete.img"
write_manifest "$TEST_ROOT/complete.img" "$TEST_ROOT/complete.img.manifest.json"

if "$SIGNER" "$TEST_ROOT/complete.img" >/dev/null 2>&1; then
    fail_test "missing signing authority was accepted without explicit lab intent"
fi
[ ! -e "$TEST_ROOT/complete.img.sig" ] || fail_test "missing-key refusal left output"

"$SIGNER" "$TEST_ROOT/complete.img" --allow-unsigned-lab >/dev/null \
    || fail_test "explicit unsigned-lab state was rejected"
[ ! -e "$TEST_ROOT/complete.img.sig" ] || fail_test "unsigned lab produced a signature"

printf 'stale-signature-sentinel\n' > "$TEST_ROOT/complete.img.sig"
if "$SIGNER" "$TEST_ROOT/complete.img" --allow-unsigned-lab >/dev/null 2>&1; then
    fail_test "unsigned lab accepted a stale sibling signature"
fi
[ "$(cat "$TEST_ROOT/complete.img.sig")" = stale-signature-sentinel ] \
    || fail_test "stale-signature refusal mutated existing bytes"
rm "$TEST_ROOT/complete.img.sig"

if "$SIGNER" "$TEST_ROOT/complete.img" --key "$TEST_ROOT/private.pem" \
    >/dev/null 2>&1; then
    fail_test "private key without trusted public key was accepted"
fi

write_manifest "$TEST_ROOT/complete.img" "$TEST_ROOT/complete.img.manifest.json" \
    "0000000000000000000000000000000000000000000000000000000000000000"
if "$SIGNER" "$TEST_ROOT/complete.img" --key "$TEST_ROOT/private.pem" \
    --pubkey "$TEST_ROOT/public.pem" >/dev/null 2>&1; then
    fail_test "manifest with wrong image digest was accepted"
fi
[ ! -e "$TEST_ROOT/complete.img.sig" ] || fail_test "digest refusal left output"

write_manifest "$TEST_ROOT/complete.img" "$TEST_ROOT/complete.img.manifest.json"
"$SIGNER" "$TEST_ROOT/complete.img" --key "$TEST_ROOT/private.pem" \
    --pubkey "$TEST_ROOT/public.pem" >/dev/null \
    || fail_test "valid manifest-bound signing failed"
[ "$(wc -c < "$TEST_ROOT/complete.img.sig" | tr -d '[:space:]')" = 64 ] \
    || fail_test "signature length is not 64 bytes"
openssl pkeyutl -verify -rawin -pubin -inkey "$TEST_ROOT/public.pem" \
    -sigfile "$TEST_ROOT/complete.img.sig" -in "$TEST_ROOT/complete.img" \
    >/dev/null || fail_test "published signature does not verify"
"$SIGNER" "$TEST_ROOT/complete.img" --key "$TEST_ROOT/private.pem" \
    --pubkey "$TEST_ROOT/public.pem" >/dev/null \
    || fail_test "exact retry did not reconcile"

printf 'synthetic VNish image\n' > "$TEST_ROOT/vnish.img"
vnish_size=$(wc -c < "$TEST_ROOT/vnish.img" | tr -d '[:space:]')
vnish_sha=$(sha256sum "$TEST_ROOT/vnish.img" | awk '{print $1}')
cat > "$TEST_ROOT/vnish.manifest.json" <<EOF
{
  "schema": "dcentos.am3_bb_vnish_sd_image_manifest.v1",
  "target": "am3-bb-s19jpro-vnish-bootbin-sd",
  "image": "dcentos-am3-bb-s19jpro-vnish-bootbin.img",
  "image_size_bytes": $vnish_size,
  "image_sha256": "$vnish_sha",
  "boot_artifacts_complete": true,
  "dtb_gate_bypassed": true,
  "boot_bin_match_reference": false,
  "bootbin_pin_override": true,
  "vendor_blob_unaudited": true,
  "boot_bin_rsa_verify_gate": "OPEN",
  "artifacts": {
    "boot.bin": true,
    "uImage": true,
    "devicetree.dtb": true,
    "uEnv.txt": true,
    "update.image.gz": true
  }
}
EOF
if "$SIGNER" "$TEST_ROOT/vnish.img" \
    --manifest "$TEST_ROOT/vnish.manifest.json" --check-only \
    >/dev/null 2>&1; then
    fail_test "unsafe VNish prototype was accepted for release signing"
fi
if bash "$SCRIPT_DIR/build_am3_bb_sd_vnish_bootbin_image.sh" --sign \
    >/dev/null 2>&1; then
    fail_test "VNish builder accepted release signing while trust gates are open"
fi
if grep -q 'release_ed25519.pub' \
    "$SCRIPT_DIR/build_am3_bb_sd_vnish_bootbin_image.sh"; then
    fail_test "VNish builder still copies a mutable public-key sidecar"
fi

printf 'synthetic S9 piggyback image\n' > "$TEST_ROOT/s9-piggyback.img"
s9_size=$(wc -c < "$TEST_ROOT/s9-piggyback.img" | tr -d '[:space:]')
s9_sha=$(sha256sum "$TEST_ROOT/s9-piggyback.img" | awk '{print $1}')
cat > "$TEST_ROOT/s9-piggyback.img.manifest.json" <<EOF
{
  "schema": "dcentos.am1_s9_sd_image_manifest.v1",
  "target": "am1-s9-sd-piggyback",
  "image": "dcentos-sd.img",
  "image_size_bytes": $s9_size,
  "image_sha256": "$s9_sha",
  "boot_artifacts_complete": true,
  "artifacts": {
    "BOOT.bin": false,
    "uImage": true,
    "devicetree.dtb": true,
    "uEnv.txt": true,
    "bitstream": true,
    "rootfs": true
  }
}
EOF
"$SIGNER" "$TEST_ROOT/s9-piggyback.img" --check-only >/dev/null \
    || fail_test "S9 piggyback manifest policy was rejected"

if "$SIGNER" "$TEST_ROOT/complete.img" --key "$TEST_ROOT/private.pem" \
    --pubkey "$TEST_ROOT/wrong-public.pem" \
    --output-sig "$TEST_ROOT/wrong-key.sig" >/dev/null 2>&1; then
    fail_test "mismatched trusted public key was accepted"
fi
[ ! -e "$TEST_ROOT/wrong-key.sig" ] || fail_test "wrong-key refusal left output"

printf 'preserve-collision\n' > "$TEST_ROOT/collision.sig"
if "$SIGNER" "$TEST_ROOT/complete.img" --key "$TEST_ROOT/private.pem" \
    --pubkey "$TEST_ROOT/public.pem" \
    --output-sig "$TEST_ROOT/collision.sig" >/dev/null 2>&1; then
    fail_test "conflicting signature output was overwritten"
fi
[ "$(cat "$TEST_ROOT/collision.sig")" = preserve-collision ] \
    || fail_test "conflicting output bytes changed"

write_manifest "$TEST_ROOT/complete.img" "$TEST_ROOT/complete.img.manifest.json" \
    "$(sha256sum "$TEST_ROOT/complete.img" | awk '{print $1}')" false
if "$SIGNER" "$TEST_ROOT/complete.img" --key "$TEST_ROOT/private.pem" \
    --pubkey "$TEST_ROOT/public.pem" \
    --output-sig "$TEST_ROOT/incomplete.sig" >/dev/null 2>&1; then
    fail_test "incomplete manifest was accepted"
fi
[ ! -e "$TEST_ROOT/incomplete.sig" ] || fail_test "incomplete refusal left output"
write_manifest "$TEST_ROOT/complete.img" "$TEST_ROOT/complete.img.manifest.json"

cp "$TEST_ROOT/complete.img.manifest.json" "$TEST_ROOT/complete.manifest.json"
if "$SIGNER" "$TEST_ROOT/complete.img" --key "$TEST_ROOT/private.pem" \
    --pubkey "$TEST_ROOT/public.pem" \
    --output-sig "$TEST_ROOT/ambiguous.sig" >/dev/null 2>&1; then
    fail_test "ambiguous sibling manifests were accepted"
fi
[ ! -e "$TEST_ROOT/ambiguous.sig" ] || fail_test "ambiguous refusal left output"
rm "$TEST_ROOT/complete.manifest.json"

if ln -s "complete.img" "$TEST_ROOT/image-link.img" 2>/dev/null &&
    is_link_like "$TEST_ROOT/image-link.img"; then
    if "$SIGNER" "$TEST_ROOT/image-link.img" --manifest \
        "$TEST_ROOT/complete.img.manifest.json" \
        --key "$TEST_ROOT/private.pem" --pubkey "$TEST_ROOT/public.pem" \
        --output-sig "$TEST_ROOT/image-link.sig" >/dev/null 2>&1; then
        fail_test "symlinked image was accepted"
    fi
    [ ! -e "$TEST_ROOT/image-link.sig" ] || fail_test "symlink refusal left output"
fi

cp "$TEST_ROOT/complete.img.manifest.json" "$TEST_ROOT/manifest-real.json"
if ln -s "manifest-real.json" "$TEST_ROOT/manifest-link.json" 2>/dev/null &&
    is_link_like "$TEST_ROOT/manifest-link.json"; then
    if "$SIGNER" "$TEST_ROOT/complete.img" --manifest \
        "$TEST_ROOT/manifest-link.json" \
        --key "$TEST_ROOT/private.pem" --pubkey "$TEST_ROOT/public.pem" \
        --output-sig "$TEST_ROOT/manifest-link.sig" >/dev/null 2>&1; then
        fail_test "symlinked manifest was accepted"
    fi
    [ ! -e "$TEST_ROOT/manifest-link.sig" ] || fail_test "manifest refusal left output"
fi

cp "$TEST_ROOT/complete.img" "$TEST_ROOT/hardlinked.img"
write_manifest "$TEST_ROOT/hardlinked.img" "$TEST_ROOT/hardlinked.img.manifest.json"
if ln "$TEST_ROOT/hardlinked.img" "$TEST_ROOT/hardlinked-alias.img" 2>/dev/null &&
    [ "$(stat -c '%h' "$TEST_ROOT/hardlinked.img" 2>/dev/null || echo 1)" -gt 1 ]; then
    if "$SIGNER" "$TEST_ROOT/hardlinked.img" \
        --key "$TEST_ROOT/private.pem" --pubkey "$TEST_ROOT/public.pem" \
        --output-sig "$TEST_ROOT/hardlinked.sig" >/dev/null 2>&1; then
        fail_test "multiply-linked image was accepted"
    fi
    [ ! -e "$TEST_ROOT/hardlinked.sig" ] || fail_test "hardlinked image left output"
fi

cp "$TEST_ROOT/complete.img.manifest.json" "$TEST_ROOT/hard-manifest.json"
if ln "$TEST_ROOT/hard-manifest.json" "$TEST_ROOT/hard-manifest-alias.json" \
    2>/dev/null &&
    [ "$(stat -c '%h' "$TEST_ROOT/hard-manifest.json" 2>/dev/null || echo 1)" -gt 1 ]; then
    if "$SIGNER" "$TEST_ROOT/complete.img" \
        --manifest "$TEST_ROOT/hard-manifest.json" \
        --key "$TEST_ROOT/private.pem" --pubkey "$TEST_ROOT/public.pem" \
        --output-sig "$TEST_ROOT/hard-manifest.sig" >/dev/null 2>&1; then
        fail_test "multiply-linked manifest was accepted"
    fi
    [ ! -e "$TEST_ROOT/hard-manifest.sig" ] || fail_test "hardlinked manifest left output"
fi

image_before=$(sha256sum "$TEST_ROOT/complete.img" | awk '{print $1}')
if "$SIGNER" "$TEST_ROOT/complete.img" \
    --key "$TEST_ROOT/private.pem" --pubkey "$TEST_ROOT/public.pem" \
    --output-sig "$TEST_ROOT/complete.img" >/dev/null 2>&1; then
    fail_test "signature output aliasing the image was accepted"
fi
[ "$(sha256sum "$TEST_ROOT/complete.img" | awk '{print $1}')" = "$image_before" ] \
    || fail_test "output-alias refusal mutated the image"

printf 'symlink-target-sentinel\n' > "$TEST_ROOT/symlink-target"
if ln -s "symlink-target" "$TEST_ROOT/output-link.sig" 2>/dev/null &&
    is_link_like "$TEST_ROOT/output-link.sig"; then
    if "$SIGNER" "$TEST_ROOT/complete.img" \
        --key "$TEST_ROOT/private.pem" --pubkey "$TEST_ROOT/public.pem" \
        --output-sig "$TEST_ROOT/output-link.sig" >/dev/null 2>&1; then
        fail_test "symlinked output was accepted"
    fi
    [ "$(cat "$TEST_ROOT/symlink-target")" = symlink-target-sentinel ] \
        || fail_test "symlink-output refusal mutated its target"
fi

printf 'manifest race image\n' > "$TEST_ROOT/race.img"
write_manifest "$TEST_ROOT/race.img" "$TEST_ROOT/race.img.manifest.json"
"$PYTHON" - "$SCRIPT_DIR/sign_sd_image.py" "$TEST_ROOT" <<'PY' \
    || fail_test "manifest pin race regression failed"
import argparse
import importlib.util
import os
from pathlib import Path
import sys

module_path = Path(sys.argv[1])
root = Path(sys.argv[2])
spec = importlib.util.spec_from_file_location("dcent_sd_signing_test", module_path)
if spec is None or spec.loader is None:
    raise SystemExit("cannot import SD signer")
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)

manifest = root / "race.img.manifest.json"
output = root / "race.sig"
original_publish = module.release_io.publish_regular_file_noreplace
mutation_attempted = False
mutation_denied = False

def mutate_manifest_then_publish(*args, **kwargs):
    global mutation_attempted, mutation_denied
    mutation_attempted = True
    try:
        manifest.write_text('{"changed":true}\n', encoding="utf-8")
    except PermissionError:
        mutation_denied = True
        raise module.exact_signer.SigningError("manifest mutation denied by pin")
    return original_publish(*args, **kwargs)

module.release_io.publish_regular_file_noreplace = mutate_manifest_then_publish
try:
    try:
        module.sign_sd_image(
            argparse.Namespace(
                image=str(root / "race.img"),
                key=str(root / "private.pem"),
                pubkey=str(root / "public.pem"),
                output_sig=str(output),
                manifest=str(manifest),
                check_only=False,
                allow_unsigned_lab=False,
            )
        )
    except (module.exact_signer.SigningError, OSError):
        pass
    else:
        raise SystemExit("manifest mutation crossed the signing commit boundary")
finally:
    module.release_io.publish_regular_file_noreplace = original_publish

if not mutation_attempted or output.exists():
    raise SystemExit("manifest race injection did not fail closed")
if os.name == "nt" and not mutation_denied:
    raise SystemExit("Windows manifest write was not denied by the active pin")
PY

printf 'durability failure image\n' > "$TEST_ROOT/durability.img"
write_manifest "$TEST_ROOT/durability.img" "$TEST_ROOT/durability.img.manifest.json"
"$PYTHON" - "$SCRIPT_DIR/sign_sd_image.py" "$TEST_ROOT" <<'PY' \
    || fail_test "input durability failure regression failed"
import argparse
import importlib.util
from pathlib import Path
import sys

module_path = Path(sys.argv[1])
root = Path(sys.argv[2])
spec = importlib.util.spec_from_file_location("dcent_sd_durability_test", module_path)
if spec is None or spec.loader is None:
    raise SystemExit("cannot import SD signer")
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)

output = root / "durability.sig"
original_flush = module.exact_signer.PinnedFile.flush
flush_injected = False

def fail_manifest_flush(self):
    global flush_injected
    if self.label == "SD image signing manifest":
        flush_injected = True
        raise OSError("injected manifest flush failure")
    return original_flush(self)

module.exact_signer.PinnedFile.flush = fail_manifest_flush
try:
    try:
        module.sign_sd_image(
            argparse.Namespace(
                image=str(root / "durability.img"),
                key=str(root / "private.pem"),
                pubkey=str(root / "public.pem"),
                output_sig=str(output),
                manifest=str(root / "durability.img.manifest.json"),
                check_only=False,
                allow_unsigned_lab=False,
            )
        )
    except OSError:
        pass
    else:
        raise SystemExit("input durability failure was converted to success")
finally:
    module.exact_signer.PinnedFile.flush = original_flush

if not flush_injected or output.exists():
    raise SystemExit("input durability failure did not fail before publication")
PY

if [ "$("$PYTHON" -c 'import os; print(os.name)')" = posix ]; then
    [ "$(stat -c '%a' "$TEST_ROOT/complete.img.sig")" = 644 ] \
        || fail_test "signature mode is not canonical 0644"
    chmod 644 "$TEST_ROOT/private.pem"
    if "$SIGNER" "$TEST_ROOT/complete.img" --key "$TEST_ROOT/private.pem" \
        --pubkey "$TEST_ROOT/public.pem" \
        --output-sig "$TEST_ROOT/unsafe-key.sig" >/dev/null 2>&1; then
        fail_test "group/world-readable private key was accepted"
    fi
    [ ! -e "$TEST_ROOT/unsafe-key.sig" ] || fail_test "unsafe-key refusal left output"
fi

echo "SD image signing tests passed"
