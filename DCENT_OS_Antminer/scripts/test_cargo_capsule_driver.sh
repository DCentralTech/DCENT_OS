#!/bin/bash
# Offline fake-Docker proof for build-dcentrald.sh release-capsule mode.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
TMPDIR_TEST="$(mktemp -d "${TMPDIR:-/tmp}/dcentos cargo capsule.XXXXXX")"
trap 'rm -rf "$TMPDIR_TEST"' EXIT

SOURCE_REPO="$TMPDIR_TEST/source-repo"
EXTERNAL_REPO="$TMPDIR_TEST/external-inputs"
CONTROL_PARENT="$TMPDIR_TEST/control"
FAKE_BIN="$TMPDIR_TEST/fake-bin"
FAKE_STATE="$TMPDIR_TEST/docker-state"
DOCKER_LOG="$TMPDIR_TEST/docker.log"
mkdir -p \
    "$SOURCE_REPO/DCENT_OS_Antminer/scripts" \
    "$SOURCE_REPO/DCENT_OS_Antminer/scripts/hw-acceptance" \
    "$SOURCE_REPO/DCENT_OS_Antminer/docs/architecture" \
    "$SOURCE_REPO/DCENT_OS_Antminer/dcentrald" \
    "$SOURCE_REPO/projects/dcent-schema" \
    "$SOURCE_REPO/knowledge-base/firmware-archive" \
    "$EXTERNAL_REPO/DCENT_OS_Antminer/scripts" \
    "$EXTERNAL_REPO/knowledge-base/extractions/s9" \
    "$CONTROL_PARENT" "$FAKE_BIN" "$FAKE_STATE/images" \
    "$FAKE_STATE/volumes" "$FAKE_STATE/containers"

for helper in \
    build-dcentrald.sh build_input_snapshot.py binary_build_receipt.py \
    release_capsule_lineage.py release_invocation.py release_result_stage.py \
    release_docker_resources.py source_closure.py source_snapshot.py; do
    cp "$SCRIPT_DIR/$helper" "$SOURCE_REPO/DCENT_OS_Antminer/scripts/$helper"
done
chmod +x "$SOURCE_REPO/DCENT_OS_Antminer/scripts/build-dcentrald.sh"
printf '%s\n' '[workspace]' > "$SOURCE_REPO/DCENT_OS_Antminer/dcentrald/Cargo.toml"
printf '%s\n' 'snapshot-only dcentrald source' > "$SOURCE_REPO/DCENT_OS_Antminer/dcentrald/source.txt"
printf '%s\n' 'test-sku' > "$SOURCE_REPO/DCENT_OS_Antminer/scripts/hw-acceptance/skus.conf"
printf '%s\n' 'test-matrix' > "$SOURCE_REPO/DCENT_OS_Antminer/docs/architecture/install_matrix.tsv"
printf '%s\n' '[test]' > "$SOURCE_REPO/DCENT_OS_Antminer/dcentrald/dcentrald_s21xp.toml"
printf '%s\n' 'snapshot-only schema source' > "$SOURCE_REPO/projects/dcent-schema/schema.txt"
printf '%s\n' '{}' > "$SOURCE_REPO/knowledge-base/firmware-archive/stock-bitmain-manifest.json"
: > "$SOURCE_REPO/knowledge-base/firmware-archive/stock-bitmain-manifest.json.sig"
printf '%s\n' 'fake authenticated S9 kernel' \
    > "$EXTERNAL_REPO/knowledge-base/extractions/s9/kernel.bin"
S9_KERNEL_SHA="$(sha256sum "$EXTERNAL_REPO/knowledge-base/extractions/s9/kernel.bin" | awk '{print $1}')"
printf '%s  %s\n' "$S9_KERNEL_SHA" \
    'knowledge-base/extractions/s9/kernel.bin' \
    > "$SOURCE_REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest"
cp "$SOURCE_REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest" \
    "$EXTERNAL_REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest"
# The external checkout's mutable manifest is deliberately hostile.  Capsule
# selection authority must come only from the authenticated snapshot root;
# the external root supplies payload bytes and nothing else.
printf '%s\n' 'attacker-controlled manifest must not be opened' \
    > "$EXTERNAL_REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest"

git -C "$SOURCE_REPO" init -q
git -C "$SOURCE_REPO" config user.email test@example.invalid
git -C "$SOURCE_REPO" config user.name 'Capsule Test'
git -C "$SOURCE_REPO" add .
git -C "$SOURCE_REPO" commit -qm snapshot
SOURCE_COMMIT="$(git -C "$SOURCE_REPO" rev-parse HEAD)"
SNAPSHOT_RESULT="$(python3 "$SCRIPT_DIR/source_snapshot.py" create \
    --repo-root "$SOURCE_REPO" --commit "$SOURCE_COMMIT" \
    --stage-parent "$CONTROL_PARENT")"
SNAPSHOT_DESCRIPTOR="$(printf '%s\n' "$SNAPSHOT_RESULT" \
    | python3 "$SCRIPT_DIR/source_snapshot.py" query-result --field snapshot)"
SNAPSHOT_TREE="$(printf '%s\n' "$SNAPSHOT_RESULT" \
    | python3 "$SCRIPT_DIR/source_snapshot.py" query-result --field tree)"
CARGO_INPUT_RESULT="$(python3 "$SCRIPT_DIR/build_input_snapshot.py" create \
    --repo-root "$EXTERNAL_REPO" \
    --selection-root "$SNAPSHOT_TREE" \
    --build-input-manifest "$SNAPSHOT_TREE/DCENT_OS_Antminer/scripts/build_inputs.manifest" \
    --target cargo-workspace --stage-parent "$CONTROL_PARENT")"
CARGO_BUILD_INPUT_SNAPSHOT="$(printf '%s\n' "$CARGO_INPUT_RESULT" \
    | python3 "$SCRIPT_DIR/build_input_snapshot.py" query-result --field snapshot)"

cat > "$FAKE_BIN/docker" <<'FAKE_DOCKER'
#!/bin/bash
set -euo pipefail
state=${FAKE_DOCKER_STATE:?}
log=${FAKE_DOCKER_LOG:?}
printf '%q ' "$@" >> "$log"
printf '\n' >> "$log"
kind=${1:-}
shift || true
digest="sha256:$(printf '1%.0s' {1..64})"
fake_shell_path() {
    local value=$1
    if [[ "$value" == '\\?\'* ]]; then value="${value:4}"; fi
    case "$value" in
        [A-Za-z]:*|*\\*) cygpath -u "$value" ;;
        *) printf '%s\n' "$value" ;;
    esac
}

if [ "$kind" = image ]; then
    action=$1; shift
    if [ "$action" = inspect ]; then
        format=""; if [ "${1:-}" = --format ]; then format=$2; shift 2; fi
        tag=$1; [ -f "$state/images/$tag" ] || exit 1
        invocation=$(cut -d'|' -f2 "$state/images/$tag")
        if [ "$format" = '{{.Id}}' ]; then
            printf '%s\n' "$digest"
        elif [ -n "$format" ]; then
            printf '%s|%s\n' "$digest" "$invocation"
        fi
    elif [ "$action" = tag ]; then
        source=$1 target=$2
        invocation=""
        if [ -f "$state/images/$source" ]; then invocation=$(cut -d'|' -f2 "$state/images/$source"); fi
        printf '%s|%s\n' "$digest" "$invocation" > "$state/images/$target"
    elif [ "$action" = rm ]; then rm -f "$state/images/$1"; fi
    exit 0
fi
if [ "$kind" = volume ]; then
    action=$1; shift
    if [ "$action" = inspect ]; then
        format=""; if [ "${1:-}" = --format ]; then format=$2; shift 2; fi
        [ "${1:-}" = -- ] && shift
        name=$1; [ -f "$state/volumes/$name" ] || exit 1
        descriptor=$(cut -d'|' -f1 "$state/volumes/$name")
        invocation=$(cut -d'|' -f2 "$state/volumes/$name")
        role=$(cut -d'|' -f3 "$state/volumes/$name")
        schema=$(cut -d'|' -f4 "$state/volumes/$name")
        if [ -n "$format" ]; then
            printf '%s|%s|%s\n' "$name" "$invocation" "$role"
        else
            printf '[{"CreatedAt":"2026-07-12T12:34:56Z","Driver":"local","Labels":{"org.dcentral.dcentos.release-resource.invocation-descriptor-sha256":"%s","org.dcentral.dcentos.release-resource.invocation-id":"%s","org.dcentral.dcentos.release-resource.role":"%s","org.dcentral.dcentos.release-resource.schema":"%s"},"Mountpoint":"/var/lib/docker/volumes/%s/_data","Name":"%s","Options":null,"Scope":"local"}]\n' \
                "$descriptor" "$invocation" "$role" "$schema" "$name" "$name"
        fi
    elif [ "$action" = create ]; then
        descriptor="" invocation="" role="" schema="" name=""
        while [ $# -gt 0 ]; do
            case "$1" in
                --driver) shift 2 ;;
                --label)
                    case "$2" in
                        org.dcentral.dcentos.release-resource.invocation-descriptor-sha256=*) descriptor=${2#*=};;
                        org.dcentral.dcentos.release-resource.invocation-id=*) invocation=${2#*=};;
                        org.dcentral.dcentos.release-resource.role=*) role=${2#*=};;
                        org.dcentral.dcentos.release-resource.schema=*) schema=${2#*=};;
                    esac
                    shift 2 ;;
                --) shift; name=$1; shift ;;
                *) name=$1; shift ;;
            esac
        done
        printf '%s|%s|%s|%s\n' "$descriptor" "$invocation" "$role" "$schema" > "$state/volumes/$name"
        printf '%s\n' "$name"
    elif [ "$action" = rm ]; then [ "${1:-}" = -- ] && shift; rm -f "$state/volumes/$1"; fi
    exit 0
fi
if [ "$kind" = container ]; then
    action=$1; shift
    if [ "$action" = inspect ]; then
        format=""; if [ "${1:-}" = --format ]; then format=$2; shift 2; fi
        name=$1; [ -f "$state/containers/$name" ] || exit 1
        invocation=$(cut -d'|' -f2 "$state/containers/$name")
        if [ -n "$format" ]; then printf '/%s|%s|cargo-build\n' "$name" "$invocation"; fi
    elif [ "$action" = rm ]; then
        [ "${1:-}" = -f ] && shift
        rm -f "$state/containers/$1"
    fi
    exit 0
fi
if [ "$kind" = build ]; then
    tag=""; invocation=""; context=""
    while [ $# -gt 0 ]; do
        case "$1" in
            -t) tag=$2; shift 2 ;;
            --label)
                case "$2" in org.dcentral.dcentos.release-invocation-id=*) invocation=${2#*=};; esac
                shift 2 ;;
            *) context=$1; shift ;;
        esac
    done
    cat >/dev/null
    context="$(fake_shell_path "$context")"
    printf '%s\n' "$context" > "$state/observed-build-context"
    printf '%s|%s\n' "$digest" "$invocation" > "$state/images/$tag"
    exit 0
fi
if [ "$kind" = run ]; then
    name=""; invocation=""; results=""; source_mount=""; schema_mount=""; cargo_mount=""; image=""; capsule_mode=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --rm) shift ;;
            --name) name=$2; shift 2 ;;
            --label)
                case "$2" in org.dcentral.dcentos.release-invocation-id=*) invocation=${2#*=};; esac
                shift 2 ;;
            -v)
                case "$2" in
                    *:/results) results=${2%:/results};;
                    *:/src:ro) source_mount=${2%:/src:ro};;
                    *:/src) source_mount=${2%:/src};;
                    *:/dcent-schema:ro) schema_mount=${2%:/dcent-schema:ro};;
                    *:/cargo-target) cargo_mount=${2%:/cargo-target};;
                esac
                shift 2 ;;
            -e)
                case "$2" in
                    DCENT_CAPSULE_MODE=*) capsule_mode=${2#*=};;
                esac
                shift 2 ;;
            sha256:*) image=$1; shift; break ;;
            *) shift ;;
        esac
    done
    source_mount="$(fake_shell_path "$source_mount")"
    schema_mount="$(fake_shell_path "$schema_mount")"
    results="$(fake_shell_path "$results")"
    [ "$image" = "$digest" ] || { echo 'mutable image execution' >&2; exit 91; }
    [ -n "$source_mount" ] || { echo 'missing source mount' >&2; exit 92; }
    if [ "$capsule_mode" = 0 ]; then
        results=$source_mount
    else
        [ -n "$results" ] || { echo 'missing result mount' >&2; exit 92; }
    fi
    printf '%s\n' "$source_mount" > "$state/observed-source-mount"
    printf '%s\n' "$schema_mount" > "$state/observed-schema-mount"
    printf '%s\n' "$cargo_mount" > "$state/observed-cargo-mount"
    printf '%s\n' "$results" > "$state/observed-result-mount"
    if [ -n "$name" ]; then printf '%s|%s\n' "$name" "$invocation" > "$state/containers/$name"; fi
    if [ "${FAKE_DOCKER_SIGNAL_RUN:-0}" = 1 ]; then
        printf '%s\n' "$PPID" > "$state/signal-ready"
        sleep 2
        exit 143
    fi
    if [ "${FAKE_DOCKER_FAIL_RUN:-0}" = 1 ]; then exit 93; fi
    triple=armv7-unknown-linux-musleabihf
    release="$results/target/$triple/release"
    inventory="$results/target/release-inventory"
    mkdir -p "$release" "$inventory"
    for binary in dcentrald dcentos-init dcentos-discovery; do printf 'ELF-%s\n' "$binary" > "$release/$binary"; chmod 755 "$release/$binary"; done
    printf '{}\n' > "$inventory/$triple.metadata.json"
    printf 'rustc 1.90.0\ncargo 1.90.0\nbuilder_base_reference=test\nbuilder_image_id=%s\nbuilder_package_resolution=test\n' "$digest" > "$inventory/$triple.toolchain.txt"
    {
        printf 'CARGO_BUILD_PROFILE=release\n'
        printf 'DCENT_BUILDER_BASE_REFERENCE=%s\n' "${DCENT_RUST_BUILDER_BASE:?}"
        printf 'DCENT_BUILDER_IMAGE_ID=%s\n' "$digest"
        printf 'DCENT_BUILDER_KIND=docker-cross\n'
        printf 'DCENT_BUILDER_PACKAGE_RESOLUTION=apt-bookworm-live-not-reconstructibly-pinned\n'
    } > "$inventory/$triple.compile-env.txt"
    if [ -n "$name" ]; then rm -f "$state/containers/$name"; fi
    exit 0
fi
echo "unsupported fake docker invocation: $kind $*" >&2
exit 99
FAKE_DOCKER
chmod +x "$FAKE_BIN/docker"

# Pin the Windows/Git-Bash transport branch as well as exercising every real
# capsule path above with spaces.  On a host that provides cygpath, this exact
# spelling must survive conversion without token splitting.
grep -Fq 'cygpath -u "$path_value"' "$SCRIPT_DIR/build-dcentrald.sh"
if command -v cygpath >/dev/null 2>&1; then
    test "$(cygpath -u 'C:\Capsule Root\result stage')" = '/c/Capsule Root/result stage'
fi
FAKE_CYGPATH_BIN="$TMPDIR_TEST/fake-cygpath"
mkdir -p "$FAKE_CYGPATH_BIN"
cat > "$FAKE_CYGPATH_BIN/cygpath" <<'FAKE_CYGPATH'
#!/bin/bash
set -euo pipefail
[ "$1" = -u ]
case "$2" in
    'C:\Capsule Root\result stage') printf '%s\n' '/c/Capsule Root/result stage' ;;
    *) printf '%s\n' "$2" ;;
esac
FAKE_CYGPATH
chmod +x "$FAKE_CYGPATH_BIN/cygpath"
eval "$(sed -n '/^normalize_shell_path()/,/^}/p' "$SCRIPT_DIR/build-dcentrald.sh")"
test "$(PATH="$FAKE_CYGPATH_BIN:$PATH" normalize_shell_path 'C:\Capsule Root\result stage')" \
    = '/c/Capsule Root/result stage'
unset -f normalize_shell_path

new_capsule() {
    local suffix=$1 invocation_result
    invocation_result="$(python3 "$SCRIPT_DIR/release_invocation.py" create \
        --stage-parent "$CONTROL_PARENT" --name "cargo-$suffix")"
    INVOCATION_STAGE="$(printf '%s\n' "$invocation_result" | python3 "$SCRIPT_DIR/release_invocation.py" query-result --field stage)"
    INVOCATION_ID="$(printf '%s\n' "$invocation_result" | python3 "$SCRIPT_DIR/release_invocation.py" query-result --field invocation_id)"
    CARGO_VOLUME="$(printf '%s\n' "$invocation_result" | python3 "$SCRIPT_DIR/release_invocation.py" query-result --field cargo_volume)"
    local result
    result="$(python3 "$SCRIPT_DIR/release_result_stage.py" create \
        --stage-parent "$CONTROL_PARENT" --invocation-stage "$INVOCATION_STAGE")"
    RESULT_STAGE="$(printf '%s\n' "$result" | python3 "$SCRIPT_DIR/release_result_stage.py" query-result --field stage)"
    RESULT_ROOT="$(printf '%s\n' "$result" | python3 "$SCRIPT_DIR/release_result_stage.py" query-result --field result_root)"
    RESULT_CAPABILITY="$(printf '%s\n' "$result" | python3 "$SCRIPT_DIR/release_result_stage.py" query-result --field capability)"
}

run_driver() {
    env \
        PATH="$FAKE_BIN:$PATH" \
        FAKE_DOCKER_STATE="$FAKE_STATE" FAKE_DOCKER_LOG="$DOCKER_LOG" \
        DCENT_CAPSULE_GIT_OBJECT_REPO="$SOURCE_REPO" \
        DCENT_CAPSULE_SOURCE_SNAPSHOT="$SNAPSHOT_DESCRIPTOR" \
        DCENT_CAPSULE_SOURCE_COMMIT="$SOURCE_COMMIT" \
        DCENT_CAPSULE_INVOCATION_STAGE="$INVOCATION_STAGE" \
        DCENT_CAPSULE_RESULT_STAGE="$RESULT_STAGE" \
        DCENT_CAPSULE_RESULT_ROOT="$RESULT_ROOT" \
        DCENT_CAPSULE_RESULT_CAPABILITY="$RESULT_CAPABILITY" \
        DCENT_CAPSULE_EXTERNAL_INPUT_REPO_ROOT="$EXTERNAL_REPO" \
        DCENT_CAPSULE_CARGO_BUILD_INPUT_SNAPSHOT="${CARGO_BUILD_INPUT_SNAPSHOT_OVERRIDE:-$CARGO_BUILD_INPUT_SNAPSHOT}" \
        DCENT_MANIFEST_PUBLIC_KEY_HEX="$(printf 'a%.0s' {1..64})" \
        DCENT_RUST_BUILDER_BASE="rust-test@sha256:$(printf 'b%.0s' {1..64})" \
        "$SOURCE_REPO/DCENT_OS_Antminer/scripts/build-dcentrald.sh" zynq
}

LIVE_SENTINEL="$SOURCE_REPO/DCENT_OS_Antminer/dcentrald/target/live-sentinel"
mkdir -p "$(dirname "$LIVE_SENTINEL")"
printf 'do-not-touch\n' > "$LIVE_SENTINEL"

new_capsule success
: > "$DOCKER_LOG"
run_driver >/dev/null
TRIPLE=armv7-unknown-linux-musleabihf
for binary in dcentrald dcentos-init dcentos-discovery; do
    test -f "$RESULT_ROOT/target/$TRIPLE/release/$binary"
    test -f "$RESULT_ROOT/target/$TRIPLE/release/$binary.build-receipt.json"
    grep -q '"schema_version":4' "$RESULT_ROOT/target/$TRIPLE/release/$binary.build-receipt.json"
    grep -q '"build_inputs":' "$RESULT_ROOT/target/$TRIPLE/release/$binary.build-receipt.json"
    grep -q '"selection_authority":"manifest-from-same-git-authenticated-release-capsule-source-snapshot"' \
        "$RESULT_ROOT/target/$TRIPLE/release/$binary.build-receipt.json"
done
test "$(cat "$LIVE_SENTINEL")" = do-not-touch
test "$(python3 "$SCRIPT_DIR/release_result_stage.py" query --invocation-stage "$INVOCATION_STAGE" --field state "$RESULT_STAGE")" = sealed
test "$(cat "$FAKE_STATE/observed-build-context")" = "$SNAPSHOT_TREE/DCENT_OS_Antminer/dcentrald"
test "$(cat "$FAKE_STATE/observed-source-mount")" = "$SNAPSHOT_TREE/DCENT_OS_Antminer/dcentrald"
test "$(cat "$FAKE_STATE/observed-schema-mount")" = "$SNAPSHOT_TREE/projects/dcent-schema"
test "$(cat "$FAKE_STATE/observed-cargo-mount")" = "$CARGO_VOLUME"
test "$(cat "$FAKE_STATE/observed-result-mount")" = "$RESULT_ROOT"
grep -Fq "sha256:$(printf '1%.0s' {1..64})" "$DOCKER_LOG"
test ! -e "$FAKE_STATE/volumes/$CARGO_VOLUME"
test ! -e "$FAKE_STATE/images/dcentos-release-builder:$INVOCATION_ID"
test -e "$FAKE_STATE/images/dcentos-cargo-cache:$(printf '1%.0s' {1..64})"
test -f "$CARGO_BUILD_INPUT_SNAPSHOT"

# The outer-owned descriptor is mandatory and must be split-authority v2.
# Both failures occur before any Docker resource allocation.
new_capsule missing-input
docker_lines_before="$(wc -l < "$DOCKER_LOG")"
if CARGO_BUILD_INPUT_SNAPSHOT_OVERRIDE="$CONTROL_PARENT/missing-input-snapshot.json" \
    run_driver >"$TMPDIR_TEST/missing-input.out" 2>&1; then
    echo 'missing outer Cargo snapshot unexpectedly succeeded' >&2
    exit 1
fi
grep -Eq 'snapshot|No such file|not found' "$TMPDIR_TEST/missing-input.out"
test "$(wc -l < "$DOCKER_LOG")" = "$docker_lines_before"

LEGACY_INPUT_ROOT="$TMPDIR_TEST/legacy-input-root"
mkdir -p "$LEGACY_INPUT_ROOT/DCENT_OS_Antminer/scripts" \
    "$LEGACY_INPUT_ROOT/knowledge-base/extractions/s9"
cp "$EXTERNAL_REPO/knowledge-base/extractions/s9/kernel.bin" \
    "$LEGACY_INPUT_ROOT/knowledge-base/extractions/s9/kernel.bin"
printf '%s  %s\n' "$S9_KERNEL_SHA" \
    'knowledge-base/extractions/s9/kernel.bin' \
    > "$LEGACY_INPUT_ROOT/DCENT_OS_Antminer/scripts/build_inputs.manifest"
LEGACY_INPUT_RESULT="$(python3 "$SCRIPT_DIR/build_input_snapshot.py" create \
    --repo-root "$LEGACY_INPUT_ROOT" \
    --build-input-manifest "$LEGACY_INPUT_ROOT/DCENT_OS_Antminer/scripts/build_inputs.manifest" \
    --target cargo-workspace --stage-parent "$CONTROL_PARENT")"
LEGACY_INPUT_SNAPSHOT="$(printf '%s\n' "$LEGACY_INPUT_RESULT" \
    | python3 "$SCRIPT_DIR/build_input_snapshot.py" query-result --field snapshot)"
new_capsule swapped-input
docker_lines_before="$(wc -l < "$DOCKER_LOG")"
if CARGO_BUILD_INPUT_SNAPSHOT_OVERRIDE="$LEGACY_INPUT_SNAPSHOT" \
    run_driver >"$TMPDIR_TEST/swapped-input.out" 2>&1; then
    echo 'legacy Cargo snapshot unexpectedly entered release capsule' >&2
    exit 1
fi
grep -q 'split-authority v2' "$TMPDIR_TEST/swapped-input.out"
test "$(wc -l < "$DOCKER_LOG")" = "$docker_lines_before"
test -f "$CARGO_BUILD_INPUT_SNAPSHOT"

# A result stage cannot be swapped under a different invocation authority.
OLD_RESULT_STAGE=$RESULT_STAGE OLD_RESULT_ROOT=$RESULT_ROOT OLD_RESULT_CAPABILITY=$RESULT_CAPABILITY
new_capsule swap
RESULT_STAGE=$OLD_RESULT_STAGE RESULT_ROOT=$OLD_RESULT_ROOT RESULT_CAPABILITY=$OLD_RESULT_CAPABILITY
if run_driver >"$TMPDIR_TEST/swap.out" 2>&1; then echo 'capsule swap unexpectedly succeeded' >&2; exit 1; fi
grep -Eq 'different|bound|invocation' "$TMPDIR_TEST/swap.out"

# A pre-existing derived volume is never adopted or deleted.
new_capsule preexisting
printf '%s|attacker\n' "$CARGO_VOLUME" > "$FAKE_STATE/volumes/$CARGO_VOLUME"
if run_driver >"$TMPDIR_TEST/preexisting.out" 2>&1; then echo 'pre-existing volume unexpectedly succeeded' >&2; exit 1; fi
grep -q 'already exists' "$TMPDIR_TEST/preexisting.out"
test -f "$FAKE_STATE/volumes/$CARGO_VOLUME"
rm -f "$FAKE_STATE/volumes/$CARGO_VOLUME"

# Build failure and TERM both remove only exact invocation-owned resources.
for mode in failure signal; do
    new_capsule "$mode"
    if [ "$mode" = failure ]; then
        if FAKE_DOCKER_FAIL_RUN=1 run_driver >"$TMPDIR_TEST/$mode.out" 2>&1; then exit 1; fi
    else
        rm -f "$FAKE_STATE/signal-ready"
        (FAKE_DOCKER_SIGNAL_RUN=1 run_driver >"$TMPDIR_TEST/$mode.out" 2>&1) &
        driver_pid=$!
        for _attempt in $(seq 1 500); do
            [ -e "$FAKE_STATE/signal-ready" ] && break
            sleep 0.02
        done
        [ -e "$FAKE_STATE/signal-ready" ] || { echo 'signal run never reached Docker' >&2; exit 1; }
        build_pid="$(cat "$FAKE_STATE/signal-ready")"
        kill -TERM "$build_pid"
        if wait "$driver_pid"; then exit 1; fi
    fi
    test ! -e "$FAKE_STATE/volumes/$CARGO_VOLUME"
    test ! -e "$FAKE_STATE/images/dcentos-release-builder:$INVOCATION_ID"
    test ! -e "$FAKE_STATE/containers/dcentos-cargo-run-$INVOCATION_ID"
    test -f "$CARGO_BUILD_INPUT_SNAPSHOT"
done

# Release intent without a complete capsule fails before Docker.  A direct
# development build remains available, writes only its historical live target,
# and explicitly emits no lineage receipt.
if env PATH="$FAKE_BIN:$PATH" DCENT_RELEASE_IMAGE=1 \
    "$SOURCE_REPO/DCENT_OS_Antminer/scripts/build-dcentrald.sh" zynq \
    >"$TMPDIR_TEST/no-capsule.out" 2>&1; then
    echo 'release build without capsule unexpectedly succeeded' >&2
    exit 1
fi
grep -q 'without an authenticated release capsule' "$TMPDIR_TEST/no-capsule.out"
if ! env PATH="$FAKE_BIN:$PATH" FAKE_DOCKER_STATE="$FAKE_STATE" FAKE_DOCKER_LOG="$DOCKER_LOG" \
    DCENT_RUST_BUILDER_BASE=rust:1.90-bookworm \
    "$SOURCE_REPO/DCENT_OS_Antminer/scripts/build-dcentrald.sh" zynq \
    >"$TMPDIR_TEST/development.out" 2>&1; then
    cat "$TMPDIR_TEST/development.out" >&2
    exit 1
fi
grep -q 'build receipt skipped' "$TMPDIR_TEST/development.out"
test "$(cat "$LIVE_SENTINEL")" = do-not-touch
for binary in dcentrald dcentos-init dcentos-discovery; do
    test -f "$SOURCE_REPO/DCENT_OS_Antminer/dcentrald/target/$TRIPLE/release/$binary"
    test ! -e "$SOURCE_REPO/DCENT_OS_Antminer/dcentrald/target/$TRIPLE/release/$binary.build-receipt.json"
done

echo "cargo capsule driver fake-Docker tests passed"
