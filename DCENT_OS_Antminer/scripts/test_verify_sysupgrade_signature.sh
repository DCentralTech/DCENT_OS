#!/bin/sh
# Offline behavioral regression test for the signed sysupgrade contract.

set -eu

SCRIPT_DIR=$(CDPATH= cd "$(dirname "$0")" && pwd)
PROJECT_DIR=$(CDPATH= cd "$SCRIPT_DIR/.." && pwd)
VERIFY="$PROJECT_DIR/scripts/verify_sysupgrade_signature.sh"

for command_name in date od openssl sha256sum tail tar; do
    command -v "$command_name" >/dev/null 2>&1 || {
        echo "SKIP: $command_name is required" >&2
        exit 77
    }
done
PYTHON=''
for python_candidate in python3 python; do
    if command -v "$python_candidate" >/dev/null 2>&1 &&
        "$python_candidate" -c \
            'import sys; raise SystemExit(sys.version_info < (3, 8))' \
            >/dev/null 2>&1; then
        PYTHON=$python_candidate
        break
    fi
done
[ -n "$PYTHON" ] || {
    echo "SKIP: Python 3.8 or newer is required" >&2
    exit 77
}

TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT INT TERM

BOARD=am1-s9
PREFIX="sysupgrade-$BOARD"
PAYLOAD_DIR="$TMPDIR/$PREFIX"
PRIVATE_KEY="$TMPDIR/release.key"
PUBLIC_KEY="$PAYLOAD_DIR/release_ed25519.pub"
INTERMEDIATE_PRIVATE_KEY="$TMPDIR/intermediate.key"
INTERMEDIATE_PUBLIC_KEY="$TMPDIR/intermediate.pub"
PACKAGE="$TMPDIR/package.tar"

mkdir -p "$PAYLOAD_DIR"
printf '\320\015\376\355kernel\n' > "$PAYLOAD_DIR/kernel"
printf 'hsqsroot\n' > "$PAYLOAD_DIR/root"
printf 'board=%s\n' "$BOARD" > "$PAYLOAD_DIR/METADATA"

openssl genpkey -algorithm Ed25519 -out "$PRIVATE_KEY" >/dev/null 2>&1
openssl pkey -in "$PRIVATE_KEY" -pubout -out "$PUBLIC_KEY" >/dev/null 2>&1

file_size() {
    wc -c < "$1" | tr -d ' '
}

file_sha256() {
    sha256sum "$1" | awk '{print $1}'
}

write_manifest() {
    installable=$1
    maturity=$2
    manifest_board=$3
    manifest_target=$4
    extra_authority_fields=${5:-}
    signing_key=${6:-$PRIVATE_KEY}

    bitstream_block=""
    if [ -f "$PAYLOAD_DIR/fpga_bitstream.bit" ]; then
        bitstream_block=",
    \"bitstream\": {\"path\": \"$PREFIX/fpga_bitstream.bit\", \"size\": $(file_size "$PAYLOAD_DIR/fpga_bitstream.bit"), \"sha256\": \"$(file_sha256 "$PAYLOAD_DIR/fpga_bitstream.bit")\"}"
    fi

    cat > "$PAYLOAD_DIR/MANIFEST.json" <<EOF
{
  "schema": 1,
  "manifest_profile": "dcentos.sysupgrade-authority/v1",
  "product": "DCENT_OS",
  "package_type": "sysupgrade",
  "installable": $installable,
  "artifact_maturity": "$maturity",
  "board": "$manifest_board",
  "board_target": "$manifest_target",
  "version": "contract-test",
  "status": "release"${extra_authority_fields},
  "payloads": {
    "kernel": {"path": "$PREFIX/kernel", "size": $(file_size "$PAYLOAD_DIR/kernel"), "sha256": "$(file_sha256 "$PAYLOAD_DIR/kernel")"},
    "rootfs": {"path": "$PREFIX/root", "size": $(file_size "$PAYLOAD_DIR/root"), "sha256": "$(file_sha256 "$PAYLOAD_DIR/root")"},
    "metadata": {"path": "$PREFIX/METADATA", "size": $(file_size "$PAYLOAD_DIR/METADATA"), "sha256": "$(file_sha256 "$PAYLOAD_DIR/METADATA")"},
    "verification_key": {"path": "$PREFIX/release_ed25519.pub", "size": $(file_size "$PUBLIC_KEY"), "sha256": "$(file_sha256 "$PUBLIC_KEY")"}${bitstream_block}
  }
}
EOF
    openssl pkeyutl -sign -rawin \
        -inkey "$signing_key" \
        -in "$PAYLOAD_DIR/MANIFEST.json" \
        -out "$PAYLOAD_DIR/MANIFEST.sig"
    repack
}

public_key_raw_hex() {
    openssl pkey -pubin -in "$1" -outform DER 2>/dev/null \
        | tail -c 32 \
        | od -An -v -tx1 \
        | tr -d ' \n'
}

file_hex() {
    od -An -v -tx1 "$1" | tr -d ' \n'
}

write_valid_two_level_manifest() {
    openssl genpkey -algorithm Ed25519 -out "$INTERMEDIATE_PRIVATE_KEY" >/dev/null 2>&1
    openssl pkey -in "$INTERMEDIATE_PRIVATE_KEY" -pubout \
        -out "$INTERMEDIATE_PUBLIC_KEY" >/dev/null 2>&1

    root_hex=$(public_key_raw_hex "$PUBLIC_KEY")
    intermediate_hex=$(public_key_raw_hex "$INTERMEDIATE_PUBLIC_KEY")
    now=$(date +%s)
    not_before=$((now - 60))
    not_after=$((now + 3600))
    serial=contract-test-intermediate
    cert_message="$TMPDIR/intermediate-cert.message"
    cert_signature="$TMPDIR/intermediate-cert.sig"
    printf 'schema=1\ntype=ota-intermediate-cert\nroot=%s\nintermediate=%s\nnot_before=%s\nnot_after=%s\nserial=%s\n' \
        "$root_hex" "$intermediate_hex" "$not_before" "$not_after" "$serial" \
        > "$cert_message"
    openssl pkeyutl -sign -rawin -inkey "$PRIVATE_KEY" \
        -in "$cert_message" -out "$cert_signature"
    root_signature_hex=$(file_hex "$cert_signature")

    chain_fields=",
  \"ota_intermediate_cert\": {
    \"root_key_hex\": \"$root_hex\",
    \"intermediate_key_hex\": \"$intermediate_hex\",
    \"not_before\": $not_before,
    \"not_after\": $not_after,
    \"serial\": \"$serial\",
    \"root_signature_hex\": \"$root_signature_hex\"
  }"
    write_manifest true experimental "$BOARD" "$BOARD" \
        "$chain_fields" "$INTERMEDIATE_PRIVATE_KEY"
}

repack() {
    rm -f "$PACKAGE"
    tar -C "$TMPDIR" -cf "$PACKAGE" "$PREFIX"
}

repack_with_unsafe_root() {
    unsafe_type=$1
    "$PYTHON" - "$PACKAGE" "$TMPDIR" "$PREFIX" "$unsafe_type" <<'PY'
from pathlib import Path
import sys
import tarfile

package = Path(sys.argv[1])
root = Path(sys.argv[2])
prefix = sys.argv[3]
unsafe_type = sys.argv[4]
source = root / prefix

with tarfile.open(package, "w:") as archive:
    archive.add(source, arcname=prefix, recursive=False)
    for path in sorted(source.rglob("*")):
        name = f"{prefix}/{path.relative_to(source).as_posix()}"
        if name != f"{prefix}/root":
            archive.add(path, arcname=name, recursive=False)
            continue
        member = tarfile.TarInfo(name)
        member.mode = 0o644
        if unsafe_type == "symlink":
            member.type = tarfile.SYMTYPE
            member.linkname = "kernel"
        elif unsafe_type == "hardlink":
            member.type = tarfile.LNKTYPE
            member.linkname = f"{prefix}/kernel"
        elif unsafe_type == "fifo":
            member.type = tarfile.FIFOTYPE
        else:
            raise SystemExit(f"unknown unsafe type: {unsafe_type}")
        archive.addfile(member)
PY
}

resign_and_repack() {
    openssl pkeyutl -sign -rawin -inkey "$PRIVATE_KEY" \
        -in "$PAYLOAD_DIR/MANIFEST.json" -out "$PAYLOAD_DIR/MANIFEST.sig"
    repack
}

expect_rejection() {
    label=$1
    expected_message=${2:-}
    rejection_output="$TMPDIR/rejection-output"
    if sh "$VERIFY" "$PACKAGE" "$PUBLIC_KEY" "$BOARD" >"$rejection_output" 2>&1; then
        echo "FAIL: verifier accepted $label" >&2
        exit 1
    fi
    if [ -n "$expected_message" ] && ! grep -F "$expected_message" "$rejection_output" >/dev/null 2>&1; then
        echo "FAIL: verifier rejected $label for the wrong reason" >&2
        cat "$rejection_output" >&2
        exit 1
    fi
    echo "PASS: verifier rejects $label"
}

expect_busybox_tar_rejection() {
    label=$1
    expected_message=$2
    command -v busybox >/dev/null 2>&1 || return 0
    busybox_shim="$TMPDIR/busybox-shim"
    busybox_output="$TMPDIR/busybox-rejection-output"
    mkdir -p "$busybox_shim"
    ln -sf "$(command -v busybox)" "$busybox_shim/tar"
    if env PATH="$busybox_shim:$PATH" sh "$VERIFY" "$PACKAGE" "$PUBLIC_KEY" "$BOARD" >"$busybox_output" 2>&1; then
        echo "FAIL: BusyBox tar verifier accepted $label" >&2
        exit 1
    fi
    if ! grep -F "$expected_message" "$busybox_output" >/dev/null 2>&1; then
        echo "FAIL: BusyBox tar verifier rejected $label for the wrong reason" >&2
        cat "$busybox_output" >&2
        exit 1
    fi
    echo "PASS: BusyBox tar verifier rejects $label"
}

write_manifest true experimental "$BOARD" "$BOARD"
sh "$VERIFY" "$PACKAGE" "$PUBLIC_KEY" "$BOARD" >/dev/null
echo "PASS: verifier accepts an exact, signed, installable experimental bundle"

sed 's/"status": "release"/"status": "lab_unsigned"/' \
    "$PAYLOAD_DIR/MANIFEST.json" > "$PAYLOAD_DIR/MANIFEST.json.tmp"
mv "$PAYLOAD_DIR/MANIFEST.json.tmp" "$PAYLOAD_DIR/MANIFEST.json"
resign_and_repack
expect_rejection \
    "a signed authority-v1 bundle with status=lab_unsigned" \
    "authority-v1 forbids status=lab_unsigned"

write_manifest true experimental "$BOARD" "$BOARD"
sed 's#dcentos.sysupgrade-authority/v1#dcentos.sysupgrade-unsigned-lab/v1#' \
    "$PAYLOAD_DIR/MANIFEST.json" > "$PAYLOAD_DIR/MANIFEST.json.tmp"
mv "$PAYLOAD_DIR/MANIFEST.json.tmp" "$PAYLOAD_DIR/MANIFEST.json"
resign_and_repack
expect_rejection \
    "a signed unsigned-lab/v1 bundle" \
    "profile 'dcentos.sysupgrade-unsigned-lab/v1' is unsupported"

write_manifest true experimental "$BOARD" "$BOARD"
awk '{
    print
    if ($0 ~ /"product"[[:space:]]*:/) print "  \"product\": \"NOT_DCENT_OS\","
}' "$PAYLOAD_DIR/MANIFEST.json" > "$PAYLOAD_DIR/MANIFEST.json.tmp"
mv "$PAYLOAD_DIR/MANIFEST.json.tmp" "$PAYLOAD_DIR/MANIFEST.json"
resign_and_repack
expect_rejection \
    "a signed manifest with a duplicate root authority field" \
    "duplicate decoded JSON member name: 'product'"

write_manifest true experimental "$BOARD" "$BOARD"
awk '{
    print
    if ($0 ~ /"status"[[:space:]]*:/) print "  \"status\": \"lab_unsigned\","
}' "$PAYLOAD_DIR/MANIFEST.json" > "$PAYLOAD_DIR/MANIFEST.json.tmp"
mv "$PAYLOAD_DIR/MANIFEST.json.tmp" "$PAYLOAD_DIR/MANIFEST.json"
resign_and_repack
expect_rejection \
    "a signed manifest with duplicate status fields" \
    "duplicate decoded JSON member name: 'status'"

write_manifest true experimental "$BOARD" "$BOARD"
"$PYTHON" - "$PAYLOAD_DIR/MANIFEST.json" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
text = path.read_text(encoding="utf-8")
text = text.replace(
    '  "version": "contract-test",',
    '  "version": "contract-test",\n  "\\u0076ersion": "0.0.0",',
    1,
)
path.write_text(text, encoding="utf-8")
PY
resign_and_repack
expect_rejection \
    "a signed manifest with a Unicode-escaped decoded duplicate key" \
    "authority profile v1 forbids JSON escape sequences"

write_manifest true experimental "$BOARD" "$BOARD"
"$PYTHON" - "$PAYLOAD_DIR/MANIFEST.json" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
text = path.read_text(encoding="utf-8").replace(
    '"status": "release"', '"status": "rele\\u0061se"', 1
)
path.write_text(text, encoding="utf-8")
PY
resign_and_repack
expect_rejection \
    "a signed manifest with an escaped authority value" \
    "authority profile v1 forbids JSON escape sequences"

write_manifest true experimental "$BOARD" "$BOARD"
"$PYTHON" - "$PAYLOAD_DIR/MANIFEST.json" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
text = path.read_text(encoding="utf-8")
text = text.replace('"sha256":', '"\\u0073ha256": "00", "sha256":', 1)
path.write_text(text, encoding="utf-8")
PY
resign_and_repack
expect_rejection \
    "a signed manifest with a nested Unicode-escaped decoded duplicate" \
    "authority profile v1 forbids JSON escape sequences"

write_manifest true experimental "$BOARD" "$BOARD"
sed '/"status"[[:space:]]*:/d' \
    "$PAYLOAD_DIR/MANIFEST.json" > "$PAYLOAD_DIR/MANIFEST.json.tmp"
mv "$PAYLOAD_DIR/MANIFEST.json.tmp" "$PAYLOAD_DIR/MANIFEST.json"
resign_and_repack
expect_rejection \
    "a signed authority-v1 manifest without status" \
    "exactly one 'status' field"

write_manifest true experimental "$BOARD" "$BOARD"
sed 's/"status": "release"/"status": " release"/' \
    "$PAYLOAD_DIR/MANIFEST.json" > "$PAYLOAD_DIR/MANIFEST.json.tmp"
mv "$PAYLOAD_DIR/MANIFEST.json.tmp" "$PAYLOAD_DIR/MANIFEST.json"
resign_and_repack
expect_rejection \
    "a signed authority-v1 manifest with whitespace-padded status" \
    "status must not contain surrounding whitespace"

write_manifest true experimental "$BOARD" "$BOARD"
sed 's/"version": "contract-test"/"version": " contract-test"/' \
    "$PAYLOAD_DIR/MANIFEST.json" > "$PAYLOAD_DIR/MANIFEST.json.tmp"
mv "$PAYLOAD_DIR/MANIFEST.json.tmp" "$PAYLOAD_DIR/MANIFEST.json"
resign_and_repack
expect_rejection \
    "a signed authority-v1 manifest with whitespace-padded version" \
    "version must not contain surrounding whitespace"

write_valid_two_level_manifest
expect_rejection \
    "a cryptographically valid intermediate-signed authority-v1 bundle" \
    "direct release-root signature"

write_manifest true experimental "$BOARD" "$BOARD" ',
  "ota_revoked_intermediates": []'
expect_rejection \
    "an authority-v1 bundle carrying inert certificate revocation claims" \
    "direct release-root signature"

printf 'optional-fpga-bitstream\n' > "$PAYLOAD_DIR/fpga_bitstream.bit"
write_manifest true experimental "$BOARD" "$BOARD"
sh "$VERIFY" "$PACKAGE" "$PUBLIC_KEY" "$BOARD" >/dev/null
echo "PASS: verifier accepts the optional bitstream only when it is manifest-declared and hashed"
rm -f "$PAYLOAD_DIR/fpga_bitstream.bit"

write_manifest false experimental "$BOARD" "$BOARD"
expect_rejection "a signed installable=false bundle"

sed 's/"installable": false/"installable": "true"/' \
    "$PAYLOAD_DIR/MANIFEST.json" > "$PAYLOAD_DIR/MANIFEST.json.tmp"
mv "$PAYLOAD_DIR/MANIFEST.json.tmp" "$PAYLOAD_DIR/MANIFEST.json"
openssl pkeyutl -sign -rawin -inkey "$PRIVATE_KEY" \
    -in "$PAYLOAD_DIR/MANIFEST.json" -out "$PAYLOAD_DIR/MANIFEST.sig"
rm -f "$PACKAGE"
tar -C "$TMPDIR" -cf "$PACKAGE" "$PREFIX"
expect_rejection "a signed string-valued installable field"

write_manifest true production "$BOARD" "$BOARD"
expect_rejection \
    "a signed artifact whose maturity conflicts with target policy" \
    "does not match the experimental sysupgrade policy"

write_manifest true experimental "$BOARD" "$BOARD"
kernel_sha=$(file_sha256 "$PAYLOAD_DIR/kernel")
kernel_sha_upper=$(printf '%s' "$kernel_sha" | tr 'a-f' 'A-F')
sed "s/$kernel_sha/$kernel_sha_upper/" \
    "$PAYLOAD_DIR/MANIFEST.json" > "$PAYLOAD_DIR/MANIFEST.json.tmp"
mv "$PAYLOAD_DIR/MANIFEST.json.tmp" "$PAYLOAD_DIR/MANIFEST.json"
resign_and_repack
expect_rejection \
    "an uppercase manifest payload digest" \
    "sha256 is not exactly 64 lowercase hex characters"

write_manifest true experimental "$BOARD" "$BOARD"
sed "s#\"$PREFIX/kernel\"#\" $PREFIX/kernel\"#" \
    "$PAYLOAD_DIR/MANIFEST.json" > "$PAYLOAD_DIR/MANIFEST.json.tmp"
mv "$PAYLOAD_DIR/MANIFEST.json.tmp" "$PAYLOAD_DIR/MANIFEST.json"
resign_and_repack
expect_rejection \
    "a whitespace-padded manifest payload path" \
    "manifest payload path is outside expected prefix"

write_manifest true experimental "$BOARD" "$BOARD"
sed '/"metadata"[[:space:]]*:/d' \
    "$PAYLOAD_DIR/MANIFEST.json" > "$PAYLOAD_DIR/MANIFEST.json.tmp"
mv "$PAYLOAD_DIR/MANIFEST.json.tmp" "$PAYLOAD_DIR/MANIFEST.json"
rm -f "$PAYLOAD_DIR/METADATA"
resign_and_repack
expect_rejection \
    "an archive and manifest that both omit METADATA" \
    "archive must contain exactly one regular $PREFIX/METADATA"
printf 'board=%s\n' "$BOARD" > "$PAYLOAD_DIR/METADATA"

write_manifest true experimental "$BOARD" am2-s19j
expect_rejection "conflicting signed board and board_target identities"

write_manifest true experimental am2-s19j am2-s19j
expect_rejection "a signed target that conflicts with the package directory"

write_manifest true experimental "$BOARD" "$BOARD"
sed '/"manifest_profile":/d' "$PAYLOAD_DIR/MANIFEST.json" \
    > "$PAYLOAD_DIR/MANIFEST.json.tmp"
mv "$PAYLOAD_DIR/MANIFEST.json.tmp" "$PAYLOAD_DIR/MANIFEST.json"
openssl pkeyutl -sign -rawin -inkey "$PRIVATE_KEY" \
    -in "$PAYLOAD_DIR/MANIFEST.json" -out "$PAYLOAD_DIR/MANIFEST.sig"
rm -f "$PACKAGE"
tar -C "$TMPDIR" -cf "$PACKAGE" "$PREFIX"
expect_rejection "a signed legacy manifest without an authority profile"

# Archive structure is admitted before extraction or signature verification.
# Keep the manifest/signature valid while mutating only the tar envelope so a
# weaker late cryptographic failure cannot accidentally satisfy these tests.
write_manifest true experimental "$BOARD" "$BOARD"
printf 'unmanifested-bitstream\n' > "$PAYLOAD_DIR/fpga_bitstream.bit"
repack
expect_rejection "an allowed optional leaf omitted from the manifest" "archive payload is not declared exactly once"
rm -f "$PAYLOAD_DIR/fpga_bitstream.bit"

printf 'unknown\n' > "$PAYLOAD_DIR/NOTES.txt"
repack
expect_rejection "an unknown flat leaf" "unknown member leaf: NOTES.txt"
rm -f "$PAYLOAD_DIR/NOTES.txt"

mkdir "$PAYLOAD_DIR/nested"
printf 'nested\n' > "$PAYLOAD_DIR/nested/payload"
repack
expect_rejection "a nested member" "nested or empty member path"
rm -rf "$PAYLOAD_DIR/nested"

repack
"$PYTHON" - "$PACKAGE" "$PREFIX" <<'PY'
import io
import sys
import tarfile

payload = b"backslash\n"
with tarfile.open(sys.argv[1], "a:") as archive:
    member = tarfile.TarInfo(f"{sys.argv[2]}/bad\\leaf")
    member.size = len(payload)
    archive.addfile(member, io.BytesIO(payload))
PY
expect_rejection "a backslash path alias" "non-canonical member path"

index=1
while [ "$index" -le 26 ]; do
    printf 'member %s\n' "$index" > "$PAYLOAD_DIR/extra-$index"
    index=$((index + 1))
done
repack
expect_rejection "an archive with more than 32 members" "maximum is 32"
rm -f "$PAYLOAD_DIR"/extra-*

rm -f "$PACKAGE"
tar -C "$TMPDIR" -cf "$PACKAGE" "$PREFIX" "$PREFIX"
expect_rejection "duplicate exact members" "duplicate archive member"

rm -f "$PACKAGE"
tar -C "$TMPDIR" -cf "$PACKAGE" "./$PREFIX"
expect_rejection "a leading-dot path alias" "non-canonical member path"

mkdir "$TMPDIR/sysupgrade-am2-s19j"
printf 'foreign\n' > "$TMPDIR/sysupgrade-am2-s19j/kernel"
rm -f "$PACKAGE"
tar -C "$TMPDIR" -cf "$PACKAGE" "$PREFIX" sysupgrade-am2-s19j
expect_rejection "a second sysupgrade target prefix" "outside expected $PREFIX/ prefix"
rm -rf "$TMPDIR/sysupgrade-am2-s19j"

write_manifest true experimental "$BOARD" "$BOARD"
repack_with_unsafe_root symlink
expect_rejection "a symlink payload member" "unsafe type l"

write_manifest true experimental "$BOARD" "$BOARD"
repack_with_unsafe_root hardlink
expect_rejection "a hardlink payload member" "unsafe type h"
expect_busybox_tar_rejection "a GNU-encoded hardlink payload member" "unsafe type h"

write_manifest true experimental "$BOARD" "$BOARD"
repack_with_unsafe_root fifo
expect_rejection "a FIFO payload member" "unsafe type p"

write_manifest true experimental "$BOARD" "$BOARD"
rm -f "$PACKAGE"
tar -C "$TMPDIR" -cf "$PACKAGE" \
    "$PREFIX/kernel" "$PREFIX/root" "$PREFIX/METADATA" \
    "$PREFIX/MANIFEST.json" "$PREFIX/MANIFEST.sig" \
    "$PREFIX/release_ed25519.pub"
expect_rejection "an archive without its canonical directory member" "expected exactly one canonical $PREFIX/ directory member"

echo "Signed sysupgrade verifier contract checks passed."
