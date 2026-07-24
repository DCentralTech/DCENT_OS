#!/usr/bin/env bash
# Offline proof for the source-bound, deterministic sysupgrade envelope.
# Payload bytes are fixtures; this does not assert rootfs/kernel reproducibility.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
. "$SCRIPT_DIR/lib/release_envelope.sh"
. "$SCRIPT_DIR/lib/sysupgrade_package_common.sh"

for tool in openssl sha256sum tar git; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "ERROR: release envelope test requires $tool" >&2
        exit 1
    }
done
if command -v python3 >/dev/null 2>&1 &&
    python3 -c 'import sys; raise SystemExit(sys.version_info < (3, 10))' \
        >/dev/null 2>&1; then
    PYTHON=(python3)
elif command -v python >/dev/null 2>&1 &&
    python -c 'import sys; raise SystemExit(sys.version_info < (3, 10))' \
        >/dev/null 2>&1; then
    PYTHON=(python)
elif command -v py >/dev/null 2>&1 &&
    py -3 -c 'import sys; raise SystemExit(sys.version_info < (3, 10))' \
        >/dev/null 2>&1; then
    PYTHON=(py -3)
else
    echo "ERROR: release envelope test requires Python 3.10 or newer" >&2
    exit 1
fi

TMPDIR_TEST="$(mktemp -d)"
trap 'rm -rf "$TMPDIR_TEST"' EXIT
KEY="$TMPDIR_TEST/release_ed25519.key"
PUBKEY="$TMPDIR_TEST/release_ed25519.pub"
openssl genpkey -algorithm ED25519 -out "$KEY" >/dev/null 2>&1
openssl pkey -in "$KEY" -pubout -out "$PUBKEY" >/dev/null 2>&1
if [ "$("${PYTHON[@]}" -c 'import os; print(os.name)')" = nt ]; then
    "${PYTHON[@]}" - "$SCRIPT_DIR" "$KEY" <<'PY'
from pathlib import Path
import sys

sys.path.insert(0, sys.argv[1])
import release_set_publication as release_io

private_key = Path(sys.argv[2])
release_io.set_windows_file_acl(
    private_key, release_io.WINDOWS_PRIVATE_FILE_SDDL
)
release_io.require_private_windows_acl(private_key, "test private key")
PY
fi

export SOURCE_DATE_EPOCH=1700000000
export DCENT_SOURCE_COMMIT_EPOCH=1700000000
export DCENT_SOURCE_COMMIT=0123456789abcdef0123456789abcdef01234567
export DCENT_SOURCE_TREE_STATE=clean
export DCENT_BUILD_TARGET=am1-s9
export DCENT_BUILD_ARCH=armv7-unknown-linux-musleabihf
export DCENT_TOOLCHAIN_ID=linaro-7.2.1:test-fixture
export DCENT_REQUIRE_RELEASE_PROVENANCE=1
export DCENT_PACKAGE_STATUS=release
export DCENT_RELEASE_IMAGE=1
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
        sysupgrade-am1-s9 \
        "$SCRIPT_DIR/release_envelope_archive.py"
}

dcent_release_provenance_init
# A caller cannot turn environment strings into an authenticated exact
# Git-object snapshot claim. Only the verified capsule lane may use that state.
DCENT_SOURCE_TREE_STATE=exact_git_object_snapshot
if dcent_release_provenance_init >/dev/null 2>&1; then
    echo "ERROR: standalone provenance accepted a fabricated exact snapshot claim" >&2
    exit 1
fi
DCENT_RELEASE_CAPSULE_MODE=1
DCENT_CAPSULE_PROVENANCE_VERIFIED=1
if dcent_release_provenance_init >/dev/null 2>&1; then
    echo "ERROR: capsule booleans replaced exact snapshot evidence" >&2
    exit 1
fi
unset DCENT_RELEASE_CAPSULE_MODE DCENT_CAPSULE_PROVENANCE_VERIFIED
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

"${PYTHON[@]}" - "$TMPDIR_TEST/one/envelope.tar" "$SOURCE_DATE_EPOCH" <<'PY'
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

GIT_FIXTURE_COMMIT="$(git -C "$GIT_FIXTURE" rev-parse HEAD)"
SNAPSHOT_PARENT="$TMPDIR_TEST/source-snapshots"
mkdir -p "$SNAPSHOT_PARENT"
SNAPSHOT_RESULT="$(
    "${PYTHON[@]}" "$SCRIPT_DIR/source_snapshot.py" create \
        --repo-root "$GIT_FIXTURE" \
        --commit "$GIT_FIXTURE_COMMIT" \
        --stage-parent "$SNAPSHOT_PARENT"
)"
snapshot_result_field() {
    printf '%s\n' "$SNAPSHOT_RESULT" |
        "${PYTHON[@]}" "$SCRIPT_DIR/source_snapshot.py" query-result --field "$1"
}
EXACT_SNAPSHOT="$(snapshot_result_field snapshot)"
EXACT_SNAPSHOT_DESTROY_TOKEN="$(snapshot_result_field destroy_token)"
DCENT_SOURCE_COMMIT="$GIT_FIXTURE_COMMIT"
SOURCE_DATE_EPOCH=1700000000
DCENT_SOURCE_COMMIT_EPOCH=1700000000
DCENT_SOURCE_TREE_STATE=exact_git_object_snapshot
DCENT_RELEASE_CAPSULE_MODE=1
DCENT_CAPSULE_PROVENANCE_VERIFIED=1
DCENT_PROVENANCE_SOURCE_SNAPSHOT="$EXACT_SNAPSHOT"
DCENT_PROVENANCE_GIT_OBJECT_REPO="$GIT_FIXTURE"
DCENT_PROVENANCE_HELPER="$SCRIPT_DIR/source_snapshot.py"
dcent_release_provenance_init
"${PYTHON[@]}" "$SCRIPT_DIR/source_snapshot.py" destroy \
    --token "$EXACT_SNAPSHOT_DESTROY_TOKEN" "$EXACT_SNAPSHOT" >/dev/null
unset DCENT_RELEASE_CAPSULE_MODE DCENT_CAPSULE_PROVENANCE_VERIFIED
unset DCENT_PROVENANCE_SOURCE_SNAPSHOT DCENT_PROVENANCE_GIT_OBJECT_REPO
unset DCENT_PROVENANCE_HELPER
DCENT_SOURCE_TREE_STATE=clean

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

git -C "$GIT_FIXTURE" checkout -q -- tracked.txt
SUBMODULE_SOURCE="$TMPDIR_TEST/submodule-source"
git init -q "$SUBMODULE_SOURCE"
git -C "$SUBMODULE_SOURCE" config user.name envelope-test
git -C "$SUBMODULE_SOURCE" config user.email envelope-test.invalid
printf 'submodule tracked\n' > "$SUBMODULE_SOURCE/tracked.txt"
git -C "$SUBMODULE_SOURCE" add tracked.txt
git -C "$SUBMODULE_SOURCE" commit -q -m fixture
git -C "$GIT_FIXTURE" -c protocol.file.allow=always submodule add -q \
    "$SUBMODULE_SOURCE" nested-source
git -C "$GIT_FIXTURE" add .gitmodules nested-source
git -C "$GIT_FIXTURE" commit -q -m submodule
(
    unset SOURCE_DATE_EPOCH DCENT_SOURCE_COMMIT_EPOCH DCENT_SOURCE_COMMIT DCENT_SOURCE_TREE_STATE
    dcent_prepare_git_release_provenance \
        "$GIT_FIXTURE" release am1-s9 armv7-unknown-linux-musleabihf linaro-7.2.1:test-fixture
)
printf 'dirty submodule\n' >> "$GIT_FIXTURE/nested-source/tracked.txt"
dirty_git_submodule() {
    unset SOURCE_DATE_EPOCH DCENT_SOURCE_COMMIT_EPOCH DCENT_SOURCE_COMMIT DCENT_SOURCE_TREE_STATE
    dcent_prepare_git_release_provenance \
        "$GIT_FIXTURE" release am1-s9 armv7-unknown-linux-musleabihf linaro-7.2.1:test-fixture
}
expect_provenance_failure dirty-git-submodule dirty_git_submodule

unsafe_archive_member() {
    unsafe_kind="$1"
    unsafe_root="$TMPDIR_TEST/unsafe-$unsafe_kind"
    mkdir -p "$unsafe_root/stage/sysupgrade-am1-s9"
    printf 'fixture\n' > "$unsafe_root/stage/sysupgrade-am1-s9/kernel"
    case "$unsafe_kind" in
        symlink)
            ln -s kernel "$unsafe_root/stage/sysupgrade-am1-s9/kernel-link"
            [ -L "$unsafe_root/stage/sysupgrade-am1-s9/kernel-link" ] || return 1
            ;;
        hardlink) ln "$unsafe_root/stage/sysupgrade-am1-s9/kernel" "$unsafe_root/stage/sysupgrade-am1-s9/kernel-link" ;;
        fifo)
            case "$(uname -s)" in MINGW*|MSYS*) return 1 ;; esac
            mkfifo "$unsafe_root/stage/sysupgrade-am1-s9/control.fifo"
            [ -p "$unsafe_root/stage/sysupgrade-am1-s9/control.fifo" ] || return 1
            ;;
    esac
    dcent_create_deterministic_tar \
        "$unsafe_root/envelope.tar" "$unsafe_root/stage" sysupgrade-am1-s9 \
        "$SCRIPT_DIR/release_envelope_archive.py"
}
expect_provenance_failure symlink-member unsafe_archive_member symlink
expect_provenance_failure hardlink-member unsafe_archive_member hardlink
expect_provenance_failure fifo-member unsafe_archive_member fifo

# Canonical publication admission is read-only. A colliding release set remains
# byte-for-byte intact for an operator; no pathname-only cleanup authority
# exists in the envelope library.
PUBLICATION_DIR="$TMPDIR_TEST/publication-collision"
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
    echo "ERROR: a prior canonical publication was accepted for replacement" >&2
    exit 1
fi
for path in \
    "$PUBLICATION_DIR/$PUBLICATION_NAME.tar" \
    "$PUBLICATION_DIR/$PUBLICATION_NAME.tar.sig" \
    "$PUBLICATION_DIR/$PUBLICATION_NAME-LAB-UNSIGNED-NOT-FOR-RELEASE.tar" \
    "$PUBLICATION_DIR/$PUBLICATION_NAME-LAB-UNSIGNED-NOT-FOR-RELEASE.tar.sig" \
    "$PUBLICATION_DIR/$PUBLICATION_NAME.release.txt"; do
    [ "$(cat "$path")" = "stale release evidence" ] || {
        echo "ERROR: publication admission mutated a colliding path: $path" >&2
        exit 1
    }
done
[ -f "$PUBLICATION_DIR/unrelated.txt" ]
if command -v dcent_release_remove_publication >/dev/null 2>&1; then
    echo "ERROR: pathname-only publication deletion authority still exists" >&2
    exit 1
fi

# The archive creator refuses final-path collisions and symlinks without
# truncating either the existing leaf or its target.
COLLISION_ARCHIVE="$TMPDIR_TEST/archive-collision.tar"
printf 'owned collision bytes\n' > "$COLLISION_ARCHIVE"
if dcent_create_deterministic_tar \
    "$COLLISION_ARCHIVE" "$TMPDIR_TEST/one/stage" sysupgrade-am1-s9 \
    "$SCRIPT_DIR/release_envelope_archive.py" >/dev/null 2>&1; then
    echo "ERROR: deterministic archive replaced an existing output" >&2
    exit 1
fi
[ "$(cat "$COLLISION_ARCHIVE")" = "owned collision bytes" ]

ARCHIVE_VICTIM="$TMPDIR_TEST/archive-victim"
ARCHIVE_SYMLINK="$TMPDIR_TEST/archive-symlink.tar"
printf 'victim bytes\n' > "$ARCHIVE_VICTIM"
ln -s "$ARCHIVE_VICTIM" "$ARCHIVE_SYMLINK"
if [ -L "$ARCHIVE_SYMLINK" ]; then
    if dcent_create_deterministic_tar \
        "$ARCHIVE_SYMLINK" "$TMPDIR_TEST/one/stage" sysupgrade-am1-s9 \
        "$SCRIPT_DIR/release_envelope_archive.py" >/dev/null 2>&1; then
        echo "ERROR: deterministic archive followed a final-path symlink" >&2
        exit 1
    fi
    [ -L "$ARCHIVE_SYMLINK" ]
    [ "$(cat "$ARCHIVE_VICTIM")" = "victim bytes" ]
else
    rm -f -- "$ARCHIVE_SYMLINK"
fi
if find "$TMPDIR_TEST" -name '*.archive-pending.*' -print -quit | grep -q .; then
    echo "ERROR: failed archive publication left a pass-shaped private stage" >&2
    exit 1
fi
if [ "$(uname -s)" = "Linux" ]; then
    REAL_GNU_TAR="$(command -v tar)"
    FAKE_TAR_DIR="$TMPDIR_TEST/fake-tar"
    mkdir -p "$FAKE_TAR_DIR"
    cat > "$FAKE_TAR_DIR/tar" <<'EOF'
#!/bin/sh
if [ "${1:-}" = "--version" ]; then
    printf 'tar (GNU tar) release-envelope-test\n'
    exit 0
fi
printf 'mutated during archive\n' >> "$ARCHIVE_MUTATE_PATH"
exec "$REAL_GNU_TAR" "$@"
EOF
    chmod 0755 "$FAKE_TAR_DIR/tar"
    MUTATION_OUTPUT="$TMPDIR_TEST/archive-source-mutation.tar"
    if ARCHIVE_MUTATE_PATH="$TMPDIR_TEST/one/stage/sysupgrade-am1-s9/kernel" \
        REAL_GNU_TAR="$REAL_GNU_TAR" \
        PATH="$FAKE_TAR_DIR:$PATH" \
        dcent_create_deterministic_tar \
            "$MUTATION_OUTPUT" "$TMPDIR_TEST/one/stage" sysupgrade-am1-s9 \
            "$SCRIPT_DIR/release_envelope_archive.py" >/dev/null 2>&1; then
        echo "ERROR: archive publication accepted source mutation during tar" >&2
        exit 1
    fi
    [ ! -e "$MUTATION_OUTPUT" ]

    SIGNAL_TAR="$TMPDIR_TEST/signal-tar"
    SIGNAL_READY="$TMPDIR_TEST/archive-signal-ready"
    cat > "$SIGNAL_TAR" <<'EOF'
#!/bin/sh
if [ "${1:-}" = "--version" ]; then
    printf 'tar (GNU tar) release-envelope-test\n'
    exit 0
fi
archive=""
while [ "$#" -gt 0 ]; do
    if [ "$1" = "-cf" ] && [ "$#" -ge 2 ]; then
        archive=$2
        break
    fi
    shift
done
[ -n "$archive" ] || exit 2
printf 'partial archive bytes\n' > "$archive"
: > "$ARCHIVE_SIGNAL_READY"
sleep 30
EOF
    chmod 0755 "$SIGNAL_TAR"
    SIGNAL_OUTPUT="$TMPDIR_TEST/archive-signal.tar"
    ARCHIVE_SIGNAL_READY="$SIGNAL_READY" \
        "${PYTHON[@]}" "$SCRIPT_DIR/release_envelope_archive.py" \
            --output "$SIGNAL_OUTPUT" \
            --base "$TMPDIR_TEST/one/stage" \
            --top sysupgrade-am1-s9 \
            --source-date-epoch "$SOURCE_DATE_EPOCH" \
            --tar "$SIGNAL_TAR" >/dev/null 2>&1 &
    ARCHIVE_PID=$!
    signal_wait=0
    while [ ! -e "$SIGNAL_READY" ] && [ "$signal_wait" -lt 100 ]; do
        sleep 0.05
        signal_wait=$((signal_wait + 1))
    done
    [ -e "$SIGNAL_READY" ] || {
        kill "$ARCHIVE_PID" >/dev/null 2>&1 || true
        wait "$ARCHIVE_PID" >/dev/null 2>&1 || true
        echo "ERROR: archive signal fixture did not reach private staging" >&2
        exit 1
    }
    kill -TERM "$ARCHIVE_PID"
    if wait "$ARCHIVE_PID"; then
        echo "ERROR: precommit archive signal returned success" >&2
        exit 1
    fi
    [ ! -e "$SIGNAL_OUTPUT" ]
    if find "$TMPDIR_TEST" -name '*.archive-pending.*' -print -quit | grep -q .; then
        echo "ERROR: signalled archive left a pass-shaped private stage" >&2
        exit 1
    fi
fi

signed_ci_profile() {
    DCENT_PACKAGE_STATUS=ci_signed
    DCENT_RELEASE_IMAGE=1
    DCENT_REQUIRE_RELEASE_PROVENANCE=1
    dcent_release_require_signed_authority_profile "$KEY"
}
signed_release_without_hardening() {
    DCENT_PACKAGE_STATUS=release
    DCENT_RELEASE_IMAGE=0
    DCENT_REQUIRE_RELEASE_PROVENANCE=1
    dcent_release_require_signed_authority_profile "$KEY"
}
signed_release_without_provenance() {
    DCENT_PACKAGE_STATUS=release
    DCENT_RELEASE_IMAGE=1
    DCENT_REQUIRE_RELEASE_PROVENANCE=0
    dcent_release_require_signed_authority_profile "$KEY"
}
expect_provenance_failure signed-ci-profile signed_ci_profile
expect_provenance_failure signed-release-without-hardening signed_release_without_hardening
expect_provenance_failure signed-release-without-provenance signed_release_without_provenance
DCENT_PACKAGE_STATUS=release
DCENT_RELEASE_IMAGE=1
DCENT_REQUIRE_RELEASE_PROVENANCE=1
dcent_release_require_signed_authority_profile "$KEY"

# The capsule-only production driver admits absent output slots and leaves all
# release-set deletion to the outer capability lifecycle.
grep -Fq 'dcent_release_require_publication_absent' "$SCRIPT_DIR/build_in_docker.sh"
grep -Fq 'dcent_require_output_absent' "$SCRIPT_DIR/build_in_docker.sh"
grep -Fq 'release_publication.py" copy' "$SCRIPT_DIR/build_in_docker.sh"
grep -Fq 'release_publication.py" stdin' \
    "$SCRIPT_DIR/build_in_docker.sh"
"${PYTHON[@]}" - "$SCRIPT_DIR/build_in_docker.sh" <<'PY'
import pathlib
import sys

driver = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")
cleanup = driver.split("dcent_cleanup_failed_release_evidence() {", 1)[1].split(
    "\n}\ntrap dcent_cleanup_failed_release_evidence EXIT", 1
)[0]
assert "dcent_release_remove_publication" not in driver
assert "dcent_release_build_lock_acquire" not in driver
assert "dcent_release_build_lock_release" not in driver
assert "rm -f --" not in cleanup
assert "$OUTPUT_DIR/" not in cleanup
assert driver.count("dcent_release_require_publication_absent") == 1
assert "this inner process never deletes publication pathnames" in driver
assert "direct Buildroot packaging is disabled" in driver
assert '${CAPSULE_SOURCE_SNAPSHOT_MOUNT}:/dcent-capsule-source:ro' in driver
assert '${GIT_OBJECT_REPO_MOUNT}:/dcent-git-object-repo:ro' in driver
assert driver.count('DCENT_PROVENANCE_HELPER="/build/dcentos/scripts/source_snapshot.py"') == 2
assert 'RELEASE_SIGNATURE_PATH="${RELEASE_COPY}.sig"' in driver
assert 'verify_sd_image.sh" "$RELEASE_COPY"' in driver
assert 'if dcent_release_provenance_required; then\n        echo "ERROR: release build has no canonical publication name' in driver
PY

echo "release envelope reproducibility: PASS"
