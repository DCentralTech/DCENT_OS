#!/usr/bin/env bash
# Offline proof for the source-bound, deterministic sysupgrade envelope.
# Payload bytes are fixtures; this does not assert rootfs/kernel reproducibility.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
. "$SCRIPT_DIR/lib/release_envelope.sh"
. "$SCRIPT_DIR/lib/sysupgrade_package_common.sh"

for tool in openssl sha256sum tar python3 git; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "ERROR: release envelope test requires $tool" >&2
        exit 1
    }
done

TMPDIR_TEST="$(mktemp -d)"
trap 'rm -rf "$TMPDIR_TEST"' EXIT
KEY="$TMPDIR_TEST/release_ed25519.key"
PUBKEY="$TMPDIR_TEST/release_ed25519.pub"
openssl genpkey -algorithm ED25519 -out "$KEY" >/dev/null 2>&1
openssl pkey -in "$KEY" -pubout -out "$PUBKEY" >/dev/null 2>&1

export SOURCE_DATE_EPOCH=1700000000
export DCENT_SOURCE_COMMIT_EPOCH=1700000000
export DCENT_SOURCE_COMMIT=0123456789abcdef0123456789abcdef01234567
export DCENT_SOURCE_TREE_STATE=clean
export DCENT_BUILD_TARGET=am1-s9
export DCENT_BUILD_ARCH=armv7-unknown-linux-musleabihf
export DCENT_TOOLCHAIN_ID=linaro-7.2.1:test-fixture
export DCENT_REQUIRE_RELEASE_PROVENANCE=1
export DCENT_PACKAGE_STATUS=ci_signed
export DCENT_RELEASE_IMAGE=0
export DCENT_RELEASE_SIGNING_KEY="$KEY"
export DCENT_RELEASE_PUBKEY_FILE="$PUBKEY"
export DCENT_REQUIRE_RELEASE_KEY=1
export DCENT_ALLOW_UNSIGNED_SYSUPGRADE=0

build_fixture_envelope() {
    fixture_root="$1"
    fixture_reverse="$2"
    mkdir -p "$fixture_root/stage/sysupgrade-am1-s9"
    SUP_DIR="$fixture_root/stage/sysupgrade-am1-s9"
    BOARD_NAME=am1-s9
    BOARD_FAMILY=zynq-am1
    PACKAGE_VERSION=0.0.0-envelope-test

    if [ "$fixture_reverse" = "1" ]; then
        printf 'rootfs fixture\n' > "$SUP_DIR/root"
        printf 'kernel fixture\n' > "$SUP_DIR/kernel"
    else
        printf 'kernel fixture\n' > "$SUP_DIR/kernel"
        printf 'rootfs fixture\n' > "$SUP_DIR/root"
    fi
    printf 'DCENT_OS\nBuild: %s\nBoard: am1-s9\n' "$DCENT_CREATED_AT_UTC" > "$SUP_DIR/METADATA"

    KERNEL_SIZE=$(wc -c < "$SUP_DIR/kernel" | tr -d '[:space:]')
    ROOTFS_SIZE=$(wc -c < "$SUP_DIR/root" | tr -d '[:space:]')
    METADATA_SIZE=$(wc -c < "$SUP_DIR/METADATA" | tr -d '[:space:]')
    KERNEL_SHA256=$(sha256sum "$SUP_DIR/kernel" | awk '{print $1}')
    ROOTFS_SHA256=$(sha256sum "$SUP_DIR/root" | awk '{print $1}')
    METADATA_SHA256=$(sha256sum "$SUP_DIR/METADATA" | awk '{print $1}')
    {
        printf '%s  kernel\n' "$KERNEL_SHA256"
        printf '%s  root\n' "$ROOTFS_SHA256"
        printf '%s  METADATA\n' "$METADATA_SHA256"
    } > "$SUP_DIR/SHA256SUMS"

    # Deliberately vary source mtimes and permissions. The envelope must erase
    # both host filesystem effects before archiving.
    if [ "$fixture_reverse" = "1" ]; then
        touch -t 202501020304 "$SUP_DIR/kernel" "$SUP_DIR/root"
        chmod 0600 "$SUP_DIR/kernel"
    else
        touch -t 202201020304 "$SUP_DIR/kernel" "$SUP_DIR/root"
        chmod 0664 "$SUP_DIR/kernel"
    fi

    dcent_stage_release_key
    dcent_write_sysupgrade_manifest
    dcent_sign_sysupgrade_manifest
    dcent_create_deterministic_tar \
        "$fixture_root/envelope.tar" \
        "$fixture_root/stage" \
        sysupgrade-am1-s9
}

dcent_release_provenance_init
# A Git-object materialization is a truthful release source state, distinct
# from (and at least as strong as) a clean mutable worktree observation.
DCENT_SOURCE_TREE_STATE=exact_git_object_snapshot
dcent_release_provenance_init
DCENT_SOURCE_TREE_STATE=clean
build_fixture_envelope "$TMPDIR_TEST/one" 0
build_fixture_envelope "$TMPDIR_TEST/two" 1

for leaf in MANIFEST.json MANIFEST.sig SHA256SUMS release_ed25519.pub; do
    cmp "$TMPDIR_TEST/one/stage/sysupgrade-am1-s9/$leaf" \
        "$TMPDIR_TEST/two/stage/sysupgrade-am1-s9/$leaf"
done
cmp "$TMPDIR_TEST/one/envelope.tar" "$TMPDIR_TEST/two/envelope.tar"
openssl pkeyutl -verify -rawin -pubin \
    -inkey "$PUBKEY" \
    -sigfile "$TMPDIR_TEST/one/stage/sysupgrade-am1-s9/MANIFEST.sig" \
    -in "$TMPDIR_TEST/one/stage/sysupgrade-am1-s9/MANIFEST.json" >/dev/null

python3 - "$TMPDIR_TEST/one/envelope.tar" "$SOURCE_DATE_EPOCH" <<'PY'
import json
import pathlib
import sys
import tarfile

archive = pathlib.Path(sys.argv[1])
epoch = int(sys.argv[2])
with tarfile.open(archive, "r:") as tf:
    members = tf.getmembers()
    names = [member.name for member in members]
    assert names == sorted(names), names
    for member in members:
        assert member.uid == 0 and member.gid == 0, member.name
        assert member.mtime == epoch, member.name
        assert not member.pax_headers, member.name
        assert member.mode == (0o755 if member.isdir() else 0o644), member.name
    manifest = json.load(tf.extractfile("sysupgrade-am1-s9/MANIFEST.json"))
assert manifest["created_at_utc"] == "2023-11-14T22:13:20Z"
assert manifest["provenance"]["source_date_epoch"] == epoch
assert manifest["provenance"]["source_commit"] == "0123456789abcdef0123456789abcdef01234567"
PY

expect_provenance_failure() {
    failure_name="$1"
    shift
    if ("$@" >/dev/null 2>&1); then
        echo "ERROR: expected provenance rejection: $failure_name" >&2
        exit 1
    fi
}

missing_epoch() {
    unset SOURCE_DATE_EPOCH
    dcent_release_provenance_init
}
invalid_epoch() {
    SOURCE_DATE_EPOCH=not-an-epoch
    dcent_release_provenance_init
}
mismatched_epoch() {
    SOURCE_DATE_EPOCH=1700000001
    dcent_release_provenance_init
}
missing_commit() {
    unset DCENT_SOURCE_COMMIT
    dcent_release_provenance_init
}
dirty_claim() {
    DCENT_SOURCE_TREE_STATE=dirty
    dcent_release_provenance_init
}
invalid_snapshot_claim() {
    DCENT_SOURCE_TREE_STATE=snapshot
    dcent_release_provenance_init
}
missing_toolchain() {
    unset DCENT_TOOLCHAIN_ID
    dcent_release_provenance_init
}

expect_provenance_failure missing-epoch missing_epoch
expect_provenance_failure invalid-epoch invalid_epoch
expect_provenance_failure epoch-commit-mismatch mismatched_epoch
expect_provenance_failure missing-commit missing_commit
expect_provenance_failure dirty-source dirty_claim
expect_provenance_failure invalid-snapshot-source invalid_snapshot_claim
expect_provenance_failure missing-toolchain missing_toolchain

GIT_FIXTURE="$TMPDIR_TEST/git-fixture"
git init -q "$GIT_FIXTURE"
git -C "$GIT_FIXTURE" config user.name envelope-test
git -C "$GIT_FIXTURE" config user.email envelope-test.invalid
printf 'tracked\n' > "$GIT_FIXTURE/tracked.txt"
git -C "$GIT_FIXTURE" add tracked.txt
GIT_AUTHOR_DATE='2023-11-14T22:13:20Z' \
GIT_COMMITTER_DATE='2023-11-14T22:13:20Z' \
    git -C "$GIT_FIXTURE" commit -q -m fixture

(
    unset SOURCE_DATE_EPOCH DCENT_SOURCE_COMMIT_EPOCH DCENT_SOURCE_COMMIT DCENT_SOURCE_TREE_STATE
    dcent_prepare_git_release_provenance \
        "$GIT_FIXTURE" ci_signed am1-s9 armv7-unknown-linux-musleabihf linaro-7.2.1:test-fixture
)
printf 'dirty\n' >> "$GIT_FIXTURE/tracked.txt"
dirty_git_tree() {
    unset SOURCE_DATE_EPOCH DCENT_SOURCE_COMMIT_EPOCH DCENT_SOURCE_COMMIT DCENT_SOURCE_TREE_STATE
    dcent_prepare_git_release_provenance \
        "$GIT_FIXTURE" ci_signed am1-s9 armv7-unknown-linux-musleabihf linaro-7.2.1:test-fixture
}
expect_provenance_failure dirty-git-tree dirty_git_tree

unsafe_archive_member() {
    unsafe_kind="$1"
    unsafe_root="$TMPDIR_TEST/unsafe-$unsafe_kind"
    mkdir -p "$unsafe_root/stage/sysupgrade-am1-s9"
    printf 'fixture\n' > "$unsafe_root/stage/sysupgrade-am1-s9/kernel"
    case "$unsafe_kind" in
        symlink) ln -s kernel "$unsafe_root/stage/sysupgrade-am1-s9/kernel-link" ;;
        hardlink) ln "$unsafe_root/stage/sysupgrade-am1-s9/kernel" "$unsafe_root/stage/sysupgrade-am1-s9/kernel-link" ;;
        fifo) mkfifo "$unsafe_root/stage/sysupgrade-am1-s9/control.fifo" ;;
    esac
    dcent_create_deterministic_tar \
        "$unsafe_root/envelope.tar" "$unsafe_root/stage" sysupgrade-am1-s9
}
expect_provenance_failure symlink-member unsafe_archive_member symlink
expect_provenance_failure hardlink-member unsafe_archive_member hardlink
expect_provenance_failure fifo-member unsafe_archive_member fifo

# Simulate a failure after canonical publication (for example, a late SD signer
# or private-stage destroy refusal). The shared EXIT-path helper must retract
# both trust variants, metadata, and in-progress temporaries without touching
# unrelated output.
PUBLICATION_DIR="$TMPDIR_TEST/publication-failure"
PUBLICATION_NAME="DCENTOS_XIL1_S9_beta20260712"
mkdir -p "$PUBLICATION_DIR"
for path in \
    "$PUBLICATION_DIR/$PUBLICATION_NAME.tar" \
    "$PUBLICATION_DIR/$PUBLICATION_NAME.tar.sig" \
    "$PUBLICATION_DIR/$PUBLICATION_NAME-LAB-UNSIGNED-NOT-FOR-RELEASE.tar" \
    "$PUBLICATION_DIR/$PUBLICATION_NAME-LAB-UNSIGNED-NOT-FOR-RELEASE.tar.sig" \
    "$PUBLICATION_DIR/$PUBLICATION_NAME.release.txt"; do
    printf 'stale release evidence\n' > "$path"
done
printf 'unrelated\n' > "$PUBLICATION_DIR/unrelated.txt"
if dcent_release_require_publication_absent \
    "$PUBLICATION_DIR" "$PUBLICATION_NAME" tar; then
    echo "ERROR: a prior canonical publication was accepted for destructive replacement" >&2
    exit 1
fi
dcent_release_remove_publication \
    "$PUBLICATION_DIR" "$PUBLICATION_NAME" tar
for path in \
    "$PUBLICATION_DIR/$PUBLICATION_NAME.tar" \
    "$PUBLICATION_DIR/$PUBLICATION_NAME.tar.sig" \
    "$PUBLICATION_DIR/$PUBLICATION_NAME-LAB-UNSIGNED-NOT-FOR-RELEASE.tar" \
    "$PUBLICATION_DIR/$PUBLICATION_NAME-LAB-UNSIGNED-NOT-FOR-RELEASE.tar.sig" \
    "$PUBLICATION_DIR/$PUBLICATION_NAME.release.txt"; do
    [ ! -e "$path" ] || {
        echo "ERROR: failed publication evidence survived cleanup: $path" >&2
        exit 1
    }
done
[ -f "$PUBLICATION_DIR/unrelated.txt" ]
dcent_release_require_publication_absent \
    "$PUBLICATION_DIR" "$PUBLICATION_NAME" tar
if dcent_release_remove_publication "$PUBLICATION_DIR" ../escape tar; then
    echo "ERROR: publication cleanup accepted a traversal name" >&2
    exit 1
fi

BUILD_LOCK="$TMPDIR_TEST/shared-build.lock"
dcent_release_build_lock_acquire "$BUILD_LOCK"
if dcent_release_build_lock_acquire "$BUILD_LOCK"; then
    echo "ERROR: concurrent build acquired the shared build/output lock" >&2
    exit 1
fi
dcent_release_build_lock_release "$BUILD_LOCK"
dcent_release_build_lock_acquire "$BUILD_LOCK"
dcent_release_build_lock_release "$BUILD_LOCK"

# The production driver refuses prior canonical names before work, retracts its
# own outputs in both failed cleanup passes, and uses the no-replace publisher.
[ "$(grep -cF 'dcent_release_remove_publication' "$SCRIPT_DIR/build_in_docker.sh")" -eq 2 ]
grep -Fq 'dcent_release_require_publication_absent' "$SCRIPT_DIR/build_in_docker.sh"
grep -Fq 'release_publication.py" copy' "$SCRIPT_DIR/build_in_docker.sh"
grep -Fq 'release_publication.py" stdin' \
    "$SCRIPT_DIR/build_in_docker.sh"
python3 - "$SCRIPT_DIR/build_in_docker.sh" <<'PY'
import pathlib
import sys

driver = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")
cleanup = driver.split("dcent_cleanup_failed_release_evidence() {", 1)[1].split(
    "\n}\ntrap dcent_cleanup_failed_release_evidence EXIT", 1
)[0]
assert cleanup.index("set +e") < cleanup.index("rm -f --")
first_failure = cleanup.split('if [ "$status" -ne 0 ]; then', 1)[1]
assert first_failure.index("dcent_release_remove_publication") < first_failure.index(
    "rm -f --"
)
assert cleanup.count("dcent_release_remove_publication") == 2
assert driver.count("dcent_release_require_publication_absent") == 1
assert 'BUILD_LOCK_DIR="$PROJECT_DIR/output/.dcentos-build.lock"' in driver
assert driver.count("dcent_release_build_lock_acquire") == 1
assert driver.count("dcent_release_build_lock_release") >= 2
assert cleanup.rindex("dcent_release_build_lock_release") > cleanup.rindex(
    "dcent_release_remove_publication"
)
assert 'RELEASE_SIGNATURE_PATH="${RELEASE_COPY}.sig"' in driver
assert 'verify_sd_image.sh" "$RELEASE_COPY"' in driver
assert 'if dcent_release_provenance_required; then\n        echo "ERROR: release build has no canonical publication name' in driver
PY

echo "release envelope reproducibility: PASS"
