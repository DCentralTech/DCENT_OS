#!/usr/bin/env bash
# Build and atomically publish one S9 release from an exact Git-object snapshot.
#
# The first process is a bootstrap only: it selects one full commit OID and
# materializes it. All Cargo, Buildroot, evidence, signing, and publication
# drivers then execute from that authenticated snapshot under one invocation.

set -euo pipefail
umask 077
# Several authenticated helpers import sibling modules. NTFS does not enforce
# the snapshot's POSIX 0500 directory modes, so default Python bytecode caching
# would add __pycache__ entries and invalidate the exact source tree mid-run.
export PYTHONDONTWRITEBYTECODE=1

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
LIVE_REPO_ROOT="$(cd "$PROJECT_DIR/../.." && pwd)"
OUTPUT_ROOT_OVERRIDE=""
RELEASE_CHANNEL="${DCENT_RELEASE_CHANNEL:-beta}"

while [ "$#" -gt 0 ]; do
    case "$1" in
        --output-root)
            [ "$#" -ge 2 ] || { echo "ERROR: --output-root requires a path" >&2; exit 2; }
            OUTPUT_ROOT_OVERRIDE=$2
            shift 2
            ;;
        --channel)
            [ "$#" -ge 2 ] || { echo "ERROR: --channel requires a value" >&2; exit 2; }
            RELEASE_CHANNEL=$2
            shift 2
            ;;
        -h|--help)
            echo "Usage: $0 [--output-root DIR] [--channel beta|rc|stable]"
            exit 0
            ;;
        *)
            echo "ERROR: unsupported argument: $1" >&2
            exit 2
            ;;
    esac
done

case "$RELEASE_CHANNEL" in
    beta|rc|stable) ;;
    *) echo "ERROR: S9 release capsule channel must be beta, rc, or stable" >&2; exit 2 ;;
esac

normalize_shell_path() {
    if command -v cygpath >/dev/null 2>&1; then
        cygpath -u "$1"
    else
        printf '%s\n' "$1"
    fi
}

if [ -n "$OUTPUT_ROOT_OVERRIDE" ]; then
    case "$OUTPUT_ROOT_OVERRIDE" in
        /*|[A-Za-z]:*) PUBLIC_OUTPUT_ROOT="$OUTPUT_ROOT_OVERRIDE" ;;
        *) PUBLIC_OUTPUT_ROOT="$PROJECT_DIR/$OUTPUT_ROOT_OVERRIDE" ;;
    esac
else
    PUBLIC_OUTPUT_ROOT="$PROJECT_DIR/output"
fi
mkdir -p "$PUBLIC_OUTPUT_ROOT"
PUBLIC_OUTPUT_ROOT="$(cd "$PUBLIC_OUTPUT_ROOT" && pwd)"

require_release_environment() {
    for name in \
        DCENT_RELEASE_SIGNING_KEY \
        DCENT_RELEASE_PUBKEY_FILE \
        DCENT_MANIFEST_PUBLIC_KEY_HEX \
        DCENT_RUST_BUILDER_BASE; do
        if [ -z "${!name:-}" ]; then
            echo "ERROR: S9 release capsule requires $name" >&2
            return 1
        fi
    done
    if ! printf '%s\n' "$DCENT_RUST_BUILDER_BASE" \
        | grep -Eq '^.+@sha256:[0-9a-f]{64}$'; then
        echo "ERROR: DCENT_RUST_BUILDER_BASE must be one exact lowercase sha256 digest reference" >&2
        return 1
    fi
    case "${DCENT_TOOLCHAIN_SHA256_VERIFIED:-}" in
        1|true|TRUE|yes|YES|y|Y) ;;
        *)
            echo "ERROR: S9 release capsule requires DCENT_TOOLCHAIN_SHA256_VERIFIED=1" >&2
            return 1
            ;;
    esac
}
require_release_environment

SOURCE_SNAPSHOT="${DCENT_CAPSULE_BOOTSTRAP_SOURCE_SNAPSHOT:-}"
SOURCE_DESTROY_TOKEN="${DCENT_CAPSULE_BOOTSTRAP_SOURCE_DESTROY_TOKEN:-}"
SOURCE_COMMIT="${DCENT_CAPSULE_BOOTSTRAP_SOURCE_COMMIT:-}"
SOURCE_TREE="${DCENT_CAPSULE_BOOTSTRAP_SOURCE_TREE:-}"
SOURCE_OWNED=0

bootstrap_cleanup() {
    status=${1:-$?}
    trap - EXIT INT TERM
    set +e
    if [ "$SOURCE_OWNED" = "1" ] && [ -n "$SOURCE_SNAPSHOT" ]; then
        python3 "$SCRIPT_DIR/source_snapshot.py" destroy \
            --token "$SOURCE_DESTROY_TOKEN" "$SOURCE_SNAPSHOT" >/dev/null 2>&1 || true
    fi
    exit "$status"
}

if [ "${DCENT_CAPSULE_BOOTSTRAPPED:-0}" != "1" ]; then
    SOURCE_COMMIT="$(git -C "$LIVE_REPO_ROOT" rev-parse --verify HEAD^{commit})"
    case "$SOURCE_COMMIT" in
        [0-9a-f][0-9a-f]*) ;;
        *) echo "ERROR: Git did not return a full lowercase commit OID" >&2; exit 1 ;;
    esac
    WORK_PARENT="$PUBLIC_OUTPUT_ROOT/.dcent-release-capsules"
    SOURCE_PARENT="$WORK_PARENT/sources"
    mkdir -p "$SOURCE_PARENT"
    SOURCE_CREATE_RESULT="$(python3 "$SCRIPT_DIR/source_snapshot.py" create \
        --repo-root "$LIVE_REPO_ROOT" \
        --commit "$SOURCE_COMMIT" \
        --stage-parent "$SOURCE_PARENT")"
    source_result_field() {
        printf '%s\n' "$SOURCE_CREATE_RESULT" \
            | python3 "$SCRIPT_DIR/source_snapshot.py" query-result --field "$1"
    }
    SOURCE_SNAPSHOT="$(source_result_field snapshot)"
    SOURCE_DESTROY_TOKEN="$(source_result_field destroy_token)"
    SOURCE_TREE="$(normalize_shell_path "$(source_result_field tree)")"
    SOURCE_OWNED=1
    trap bootstrap_cleanup EXIT
    trap 'bootstrap_cleanup 130' INT
    trap 'bootstrap_cleanup 143' TERM
    SNAPSHOT_DRIVER="$SOURCE_TREE/DCENT_OS_Antminer/scripts/build_s9_release_capsule.sh"
    [ -f "$SNAPSHOT_DRIVER" ] || {
        echo "ERROR: selected commit does not contain the S9 release capsule driver" >&2
        echo "       commit the capsule implementation before attempting a release" >&2
        exit 1
    }
    export DCENT_CAPSULE_BOOTSTRAPPED=1
    export DCENT_CAPSULE_BOOTSTRAP_SOURCE_SNAPSHOT="$SOURCE_SNAPSHOT"
    export DCENT_CAPSULE_BOOTSTRAP_SOURCE_DESTROY_TOKEN="$SOURCE_DESTROY_TOKEN"
    export DCENT_CAPSULE_BOOTSTRAP_SOURCE_COMMIT="$SOURCE_COMMIT"
    export DCENT_CAPSULE_BOOTSTRAP_SOURCE_TREE="$SOURCE_TREE"
    export DCENT_CAPSULE_BOOTSTRAP_GIT_OBJECT_REPO="$LIVE_REPO_ROOT"
    export DCENT_CAPSULE_BOOTSTRAP_EXTERNAL_INPUT_ROOT="$LIVE_REPO_ROOT"
    export DCENT_CAPSULE_BOOTSTRAP_PUBLIC_OUTPUT_ROOT="$PUBLIC_OUTPUT_ROOT"
    exec bash "$SNAPSHOT_DRIVER" \
        --output-root "$PUBLIC_OUTPUT_ROOT" --channel "$RELEASE_CHANNEL"
fi

# Everything below executes from the authenticated committed snapshot.
SOURCE_OWNED=1
# The exec resets caught traps. Re-establish the minimal source authority before
# any resumed verification can fail; capsule_cleanup replaces it once the other
# private authorities have initialized.
trap bootstrap_cleanup EXIT
trap 'bootstrap_cleanup 130' INT
trap 'bootstrap_cleanup 143' TERM
GIT_OBJECT_REPO="${DCENT_CAPSULE_BOOTSTRAP_GIT_OBJECT_REPO:?missing bootstrap Git authority}"
EXTERNAL_INPUT_ROOT="${DCENT_CAPSULE_BOOTSTRAP_EXTERNAL_INPUT_ROOT:?missing external input root}"
PUBLIC_OUTPUT_ROOT="${DCENT_CAPSULE_BOOTSTRAP_PUBLIC_OUTPUT_ROOT:?missing output root}"
VERIFIED_SOURCE="$(python3 "$SCRIPT_DIR/source_snapshot.py" verify-against-git \
    --repo-root "$GIT_OBJECT_REPO" --commit "$SOURCE_COMMIT" "$SOURCE_SNAPSHOT")"
VERIFIED_TREE="$(printf '%s\n' "$VERIFIED_SOURCE" \
    | python3 "$SCRIPT_DIR/source_snapshot.py" query-verified --field tree)"
VERIFIED_TREE="$(normalize_shell_path "$VERIFIED_TREE")"
[ "$PROJECT_DIR" = "$VERIFIED_TREE/DCENT_OS_Antminer" ] || {
    echo "ERROR: resumed release driver is not inside the authenticated snapshot" >&2
    exit 1
}
RELEASE_TARGET=s9
python3 "$SCRIPT_DIR/release_capsule_target_policy.py" \
    "$RELEASE_TARGET" target --require-publication >/dev/null
CARGO_VARIANT="$(python3 "$SCRIPT_DIR/release_capsule_target_policy.py" \
    "$RELEASE_TARGET" cargo_variant --require-publication)"
PRIMARY_ARTIFACT="$(python3 "$SCRIPT_DIR/release_capsule_target_policy.py" \
    "$RELEASE_TARGET" primary_artifact --require-publication)"
RELEASE_STEM="$(python3 "$SCRIPT_DIR/release_capsule_target_policy.py" \
    "$RELEASE_TARGET" release_stem --require-publication)"
SOURCE_DATE_EPOCH="$(git -C "$GIT_OBJECT_REPO" show -s --format=%ct "$SOURCE_COMMIT")"
RELEASE_DATE="$(python3 - "$SOURCE_DATE_EPOCH" <<'PY'
import datetime
import sys
print(datetime.datetime.fromtimestamp(int(sys.argv[1]), datetime.timezone.utc).strftime("%Y%m%d"))
PY
)"
RELEASE_NAME="${RELEASE_STEM}_${RELEASE_CHANNEL}${RELEASE_DATE}"
WORK_PARENT="$PUBLIC_OUTPUT_ROOT/.dcent-release-capsules"
INVOCATION_PARENT="$WORK_PARENT/invocations"
RESULT_PARENT="$WORK_PARENT/results"
INPUT_PARENT="$WORK_PARENT/inputs"
SIGNING_PARENT="$WORK_PARENT/signing-authorities"
RELEASE_STAGE_PARENT="$PUBLIC_OUTPUT_ROOT/.release-set-stages"
RELEASES_PARENT="$PUBLIC_OUTPUT_ROOT/releases"
mkdir -p "$INVOCATION_PARENT" "$RESULT_PARENT" "$INPUT_PARENT" "$SIGNING_PARENT" \
    "$RELEASE_STAGE_PARENT" "$RELEASES_PARENT"

INVOCATION_STAGE=""
INVOCATION_CAPABILITY=""
RESULT_STAGE=""
RESULT_ROOT=""
RESULT_CAPABILITY=""
RESULT_CREATE_RESULT_FILE=""
CARGO_INPUT_SNAPSHOT=""
CARGO_INPUT_DESTROY_TOKEN=""
SIGNING_AUTHORITY_STAGE=""
SIGNING_AUTHORITY_CAPABILITY=""
SIGNING_AUTHORITY_PRIVATE_KEY=""
SIGNING_AUTHORITY_PUBLIC_KEY=""
SIGNING_AUTHORITY_RESULT_FILE=""
RELEASE_SET_STAGE=""
RELEASE_SET_CAPABILITY_FILE=""
RELEASE_SET_MANIFEST=""
RELEASE_SET_PUBLISHED=0
INVOCATION_ID=""

capsule_cleanup() {
    status=${1:-$?}
    recovery_authority_required=0
    result_recovery_required=0
    trap - EXIT INT TERM
    set +e
    if [ "$RELEASE_SET_PUBLISHED" != "1" ] \
        && [ -n "$RELEASE_SET_CAPABILITY_FILE" ] \
        && [ -f "$RELEASE_SET_CAPABILITY_FILE" ]; then
        release_set_destroy_error="$(
            python3 "$SCRIPT_DIR/release_set_publication.py" destroy-stage \
                --capability-file "$RELEASE_SET_CAPABILITY_FILE" \
                2>&1 >/dev/null
        )"
        if [ "$?" -eq 0 ]; then
            rm -f -- "$RELEASE_SET_CAPABILITY_FILE" || status=1
            RELEASE_SET_CAPABILITY_FILE=""
        else
            echo "ERROR: release-set cleanup failed; capability retained for recovery: $RELEASE_SET_CAPABILITY_FILE" >&2
            if [ -n "$release_set_destroy_error" ]; then
                printf '%s\n' "$release_set_destroy_error" >&2
            fi
            status=1
            recovery_authority_required=1
        fi
    fi
    if [ -n "$RELEASE_SET_MANIFEST" ]; then
        rm -f -- "$RELEASE_SET_MANIFEST" || status=1
    fi
    if [ "$RELEASE_SET_PUBLISHED" = "1" ] \
        && [ -n "$RELEASE_SET_CAPABILITY_FILE" ]; then
        rm -f -- "$RELEASE_SET_CAPABILITY_FILE" || status=1
    fi
    if [ -n "$RESULT_CREATE_RESULT_FILE" ] \
        && [ -f "$RESULT_CREATE_RESULT_FILE" ]; then
        recovered_result_stage="$(python3 "$SCRIPT_DIR/release_result_stage.py" \
            query-result --field stage < "$RESULT_CREATE_RESULT_FILE" 2>/dev/null)"
        recovered_result_capability="$(python3 "$SCRIPT_DIR/release_result_stage.py" \
            query-result --field capability < "$RESULT_CREATE_RESULT_FILE" 2>/dev/null)"
        if [ -n "$recovered_result_stage" ] \
            && [ -n "$recovered_result_capability" ]; then
            RESULT_STAGE="$(normalize_shell_path "$recovered_result_stage")"
            RESULT_CAPABILITY="$(normalize_shell_path "$recovered_result_capability")"
        else
            echo "ERROR: result-stage recovery result is unreadable; retained: $RESULT_CREATE_RESULT_FILE" >&2
            status=1
            recovery_authority_required=1
            result_recovery_required=1
        fi
    fi
    if [ "$result_recovery_required" = "0" ] \
        && [ -n "$RESULT_CREATE_RESULT_FILE" ] \
        && [ -n "$RESULT_STAGE" ] && [ -e "$RESULT_STAGE" ] \
        && { [ ! -f "$RESULT_CAPABILITY" ] \
            || [ ! -f "$RESULT_STAGE/result-stage.json" ] \
            || [ ! -d "$RESULT_STAGE/results" ]; }; then
        result_create_recovery_error="$(python3 "$SCRIPT_DIR/release_result_stage.py" create \
            --stage-parent "$RESULT_PARENT" \
            --invocation-stage "$INVOCATION_STAGE" \
            --result-output "$RESULT_CREATE_RESULT_FILE" 2>&1 >/dev/null)"
        if [ "$?" -ne 0 ]; then
            echo "ERROR: result-stage creation recovery failed; locator retained: $RESULT_CREATE_RESULT_FILE" >&2
            [ -n "$result_create_recovery_error" ] \
                && printf '%s\n' "$result_create_recovery_error" >&2
            status=1
            recovery_authority_required=1
            result_recovery_required=1
        fi
    fi
    if [ "$result_recovery_required" = "0" ] \
        && [ -n "$RESULT_STAGE" ] && [ -n "$RESULT_CAPABILITY" ] \
        && { [ -e "$RESULT_STAGE" ] || [ -e "$RESULT_CAPABILITY" ]; }; then
        result_destroy_error="$(python3 "$SCRIPT_DIR/release_result_stage.py" destroy \
            --capability "$RESULT_CAPABILITY" \
            --invocation-stage "$INVOCATION_STAGE" \
            "$RESULT_STAGE" 2>&1 >/dev/null)"
        if [ "$?" -ne 0 ]; then
            echo "ERROR: result-stage cleanup failed; recovery result retained: $RESULT_CREATE_RESULT_FILE" >&2
            [ -n "$result_destroy_error" ] && printf '%s\n' "$result_destroy_error" >&2
            status=1
            recovery_authority_required=1
            result_recovery_required=1
        fi
    fi
    if [ "$result_recovery_required" = "0" ] \
        && [ -n "$RESULT_CREATE_RESULT_FILE" ] \
        && [ -f "$RESULT_CREATE_RESULT_FILE" ]; then
        rm -f -- "$RESULT_CREATE_RESULT_FILE" || status=1
    fi
    if [ -n "$CARGO_INPUT_SNAPSHOT" ] && [ -e "$CARGO_INPUT_SNAPSHOT" ]; then
        python3 "$SCRIPT_DIR/build_input_snapshot.py" destroy \
            --token "$CARGO_INPUT_DESTROY_TOKEN" "$CARGO_INPUT_SNAPSHOT" \
            >/dev/null 2>&1 || {
                status=1
                recovery_authority_required=1
            }
    fi
    if [ -n "$SIGNING_AUTHORITY_RESULT_FILE" ]; then
        if [ -f "$SIGNING_AUTHORITY_RESULT_FILE" ]; then
            recovered_signing_stage="$(python3 "$SCRIPT_DIR/release_signing_authority.py" \
                query-result --field stage < "$SIGNING_AUTHORITY_RESULT_FILE" 2>/dev/null)"
            recovered_signing_capability="$(python3 "$SCRIPT_DIR/release_signing_authority.py" \
                query-result --field capability < "$SIGNING_AUTHORITY_RESULT_FILE" 2>/dev/null)"
            if [ -n "$recovered_signing_stage" ] \
                && [ -n "$recovered_signing_capability" ]; then
                SIGNING_AUTHORITY_STAGE="$(normalize_shell_path "$recovered_signing_stage")"
                SIGNING_AUTHORITY_CAPABILITY="$(normalize_shell_path "$recovered_signing_capability")"
            else
                echo "ERROR: signing-authority recovery result is unreadable; retained: $SIGNING_AUTHORITY_RESULT_FILE" >&2
                status=1
                recovery_authority_required=1
            fi
        else
            echo "ERROR: registered signing-authority recovery result is missing; upstream authority retained: $SIGNING_AUTHORITY_RESULT_FILE" >&2
            status=1
            recovery_authority_required=1
        fi
    fi
    if [ -n "$SIGNING_AUTHORITY_STAGE" ] \
        && [ -n "$SIGNING_AUTHORITY_CAPABILITY" ] \
        && { [ -e "$SIGNING_AUTHORITY_STAGE" ] \
            || [ -e "$SIGNING_AUTHORITY_CAPABILITY" ]; }; then
        signing_destroy_error="$(python3 "$SCRIPT_DIR/release_signing_authority.py" destroy \
            --capability "$SIGNING_AUTHORITY_CAPABILITY" \
            "$SIGNING_AUTHORITY_STAGE" 2>&1 >/dev/null)"
        if [ "$?" -ne 0 ]; then
            echo "ERROR: signing-authority cleanup failed; recovery result retained: $SIGNING_AUTHORITY_RESULT_FILE" >&2
            [ -n "$signing_destroy_error" ] && printf '%s\n' "$signing_destroy_error" >&2
            status=1
            recovery_authority_required=1
        fi
    fi
    if [ "$recovery_authority_required" = "0" ] \
        && [ -n "$SIGNING_AUTHORITY_RESULT_FILE" ] \
        && [ -f "$SIGNING_AUTHORITY_RESULT_FILE" ]; then
        rm -f -- "$SIGNING_AUTHORITY_RESULT_FILE" || status=1
    fi
    if [ "$recovery_authority_required" = "0" ] \
        && [ -n "$INVOCATION_STAGE" ] && [ -e "$INVOCATION_STAGE" ]; then
        # Both build drivers must have removed their exact Docker resources
        # before the invocation control stage becomes eligible for destruction.
        external_resources_absent=1
        for resource_field in cargo_volume buildroot_volume results_volume; do
            resource_name="$(python3 "$SCRIPT_DIR/release_invocation.py" query \
                --field "$resource_field" "$INVOCATION_STAGE" 2>/dev/null)"
            if [ -n "$resource_name" ] \
                && docker volume inspect -- "$resource_name" >/dev/null 2>&1; then
                external_resources_absent=0
            fi
        done
        builder_tag="$(python3 "$SCRIPT_DIR/release_docker_resources.py" \
            query-builder-tag "$INVOCATION_STAGE" 2>/dev/null)"
        if [ -n "$builder_tag" ] \
            && docker image inspect "$builder_tag" >/dev/null 2>&1; then
            external_resources_absent=0
        fi
        if [ -n "$INVOCATION_ID" ] \
            && docker container inspect "dcentos-cargo-run-${INVOCATION_ID}" \
                >/dev/null 2>&1; then
            external_resources_absent=0
        fi
        if [ "$external_resources_absent" = "1" ]; then
            if ! {
                python3 "$SCRIPT_DIR/release_invocation.py" mark-gc-eligible \
                    --capability "$INVOCATION_CAPABILITY" \
                    --reason external-resources-disposed-and-output-state-finalized \
                    "$INVOCATION_STAGE" >/dev/null 2>&1 \
                    && python3 "$SCRIPT_DIR/release_invocation.py" destroy \
                        --capability "$INVOCATION_CAPABILITY" "$INVOCATION_STAGE" \
                        >/dev/null 2>&1
            }; then
                status=1
                recovery_authority_required=1
            fi
        else
            echo "ERROR: capsule Docker resources remain; invocation retained for recovery" >&2
            status=1
            recovery_authority_required=1
        fi
    elif [ "$recovery_authority_required" != "0" ] \
        && [ -n "$INVOCATION_STAGE" ] && [ -e "$INVOCATION_STAGE" ]; then
        echo "ERROR: dependent cleanup failed; invocation retained for recovery: $INVOCATION_STAGE" >&2
    fi
    # Destroy the authenticated source last because every cleanup authority
    # helper above is itself executed from that snapshot.
    if [ "$recovery_authority_required" = "0" ] \
        && [ "$SOURCE_OWNED" = "1" ] && [ -n "$SOURCE_SNAPSHOT" ]; then
        python3 "$SCRIPT_DIR/source_snapshot.py" destroy \
            --token "$SOURCE_DESTROY_TOKEN" "$SOURCE_SNAPSHOT" >/dev/null 2>&1 || status=1
        SOURCE_OWNED=0
    elif [ "$recovery_authority_required" != "0" ] \
        && [ "$SOURCE_OWNED" = "1" ] && [ -n "$SOURCE_SNAPSHOT" ]; then
        echo "ERROR: cleanup helper source retained for recovery: $SOURCE_SNAPSHOT" >&2
    fi
    for empty_directory in \
        "$INVOCATION_PARENT/.dcentos-release-invocation-capabilities" \
        "$SIGNING_PARENT/.dcentos-release-signing-authority-capabilities" \
        "$RESULT_PARENT/.dcentos-release-result-capabilities" \
        "$INVOCATION_PARENT" "$SIGNING_PARENT" "$RESULT_PARENT" "$RELEASE_STAGE_PARENT" \
        "$INPUT_PARENT" "$WORK_PARENT/sources"; do
        [ -n "$empty_directory" ] && rmdir -- "$empty_directory" >/dev/null 2>&1 || true
    done
    exit "$status"
}
trap capsule_cleanup EXIT
trap 'capsule_cleanup 130' INT
trap 'capsule_cleanup 143' TERM

INVOCATION_RESULT="$(python3 "$SCRIPT_DIR/release_invocation.py" create \
    --stage-parent "$INVOCATION_PARENT" --name "$RELEASE_TARGET")"
invocation_result_field() {
    printf '%s\n' "$INVOCATION_RESULT" \
        | python3 "$SCRIPT_DIR/release_invocation.py" query-result --field "$1"
}
INVOCATION_STAGE="$(normalize_shell_path "$(invocation_result_field stage)")"
INVOCATION_CAPABILITY="$(normalize_shell_path "$(invocation_result_field capability)")"
INVOCATION_ID="$(python3 "$SCRIPT_DIR/release_invocation.py" query \
    --field invocation_id "$INVOCATION_STAGE")"

# Convert the two mutable operator pathnames into one invocation-owned private
# authority before proving or using the keypair. Every later signer/verifier,
# including Docker mounts, receives only these stable copies. The private stage
# is never a release-set member and is destroyed before the invocation record.
SIGNING_AUTHORITY_RESULT_FILE="$WORK_PARENT/signing-authority-${INVOCATION_ID}.result.json"
python3 "$SCRIPT_DIR/release_signing_authority.py" create \
    --stage-parent "$SIGNING_PARENT" \
    --invocation-stage "$INVOCATION_STAGE" \
    --private-key "$DCENT_RELEASE_SIGNING_KEY" \
    --public-key "$DCENT_RELEASE_PUBKEY_FILE" \
    --result-output "$SIGNING_AUTHORITY_RESULT_FILE" >/dev/null
SIGNING_AUTHORITY_RESULT="$(cat -- "$SIGNING_AUTHORITY_RESULT_FILE")"
signing_authority_result_field() {
    printf '%s\n' "$SIGNING_AUTHORITY_RESULT" \
        | python3 "$SCRIPT_DIR/release_signing_authority.py" query-result --field "$1"
}
SIGNING_AUTHORITY_STAGE="$(normalize_shell_path "$(signing_authority_result_field stage)")"
SIGNING_AUTHORITY_CAPABILITY="$(normalize_shell_path "$(signing_authority_result_field capability)")"
SIGNING_AUTHORITY_PRIVATE_KEY="$(normalize_shell_path "$(signing_authority_result_field private_key)")"
SIGNING_AUTHORITY_PUBLIC_KEY="$(normalize_shell_path "$(signing_authority_result_field public_key)")"
DCENT_RELEASE_SIGNING_KEY="$SIGNING_AUTHORITY_PRIVATE_KEY"
DCENT_RELEASE_PUBKEY_FILE="$SIGNING_AUTHORITY_PUBLIC_KEY"
export DCENT_RELEASE_SIGNING_KEY DCENT_RELEASE_PUBKEY_FILE
python3 "$SCRIPT_DIR/release_signing_authority.py" verify \
    --invocation-stage "$INVOCATION_STAGE" "$SIGNING_AUTHORITY_STAGE" >/dev/null
# The cryptographic/key-format admission remains a separate explicit proof;
# the snapshot helper intentionally makes no OpenSSL or trust-policy claim.
bash "$SCRIPT_DIR/verify_release_keypair.sh" \
    "$DCENT_RELEASE_SIGNING_KEY" \
    "$DCENT_RELEASE_PUBKEY_FILE" \
    "$DCENT_MANIFEST_PUBLIC_KEY_HEX" >/dev/null

CARGO_INPUT_CREATE_RESULT="$(python3 "$SCRIPT_DIR/build_input_snapshot.py" create \
    --repo-root "$EXTERNAL_INPUT_ROOT" \
    --selection-root "$VERIFIED_TREE" \
    --build-input-manifest "$SCRIPT_DIR/build_inputs.manifest" \
    --target cargo-workspace \
    --stage-parent "$INPUT_PARENT")"
cargo_input_result_field() {
    printf '%s\n' "$CARGO_INPUT_CREATE_RESULT" \
        | python3 "$SCRIPT_DIR/build_input_snapshot.py" query-result --field "$1"
}
CARGO_INPUT_SNAPSHOT="$(normalize_shell_path "$(cargo_input_result_field snapshot)")"
CARGO_INPUT_DESTROY_TOKEN="$(cargo_input_result_field destroy_token)"

RESULT_CREATE_RESULT_FILE="$WORK_PARENT/result-stage-${INVOCATION_ID}.result.json"
python3 "$SCRIPT_DIR/release_result_stage.py" create \
    --stage-parent "$RESULT_PARENT" --invocation-stage "$INVOCATION_STAGE" \
    --result-output "$RESULT_CREATE_RESULT_FILE" >/dev/null
RESULT_CREATE_RESULT="$(cat -- "$RESULT_CREATE_RESULT_FILE")"
result_create_field() {
    printf '%s\n' "$RESULT_CREATE_RESULT" \
        | python3 "$SCRIPT_DIR/release_result_stage.py" query-result --field "$1"
}
RESULT_STAGE="$(normalize_shell_path "$(result_create_field stage)")"
RESULT_ROOT="$(normalize_shell_path "$(result_create_field result_root)")"
RESULT_CAPABILITY="$(normalize_shell_path "$(result_create_field capability)")"

RELEASE_SET_CAPABILITY_FILE="$WORK_PARENT/release-set-${INVOCATION_ID}.capability.json"
python3 "$SCRIPT_DIR/release_set_publication.py" create-stage \
    --parent "$RELEASE_STAGE_PARENT" \
    --capability-output "$RELEASE_SET_CAPABILITY_FILE" >/dev/null
RELEASE_SET_STAGE="$(python3 "$SCRIPT_DIR/release_set_publication.py" query \
    --field stage-path < "$RELEASE_SET_CAPABILITY_FILE")"
RELEASE_SET_STAGE="$(normalize_shell_path "$RELEASE_SET_STAGE")"

export DCENT_PACKAGE_STATUS=release
export DCENT_RELEASE_IMAGE=1
export DCENT_REQUIRE_RELEASE_PROVENANCE=1
export DCENT_ALLOW_UNSIGNED_SYSUPGRADE=0
export DCENT_RELEASE_CHANNEL="$RELEASE_CHANNEL"
export DCENT_RELEASE_CAPSULE_MODE=1
export DCENT_CAPSULE_GIT_OBJECT_REPO="$GIT_OBJECT_REPO"
export DCENT_CAPSULE_SOURCE_SNAPSHOT="$SOURCE_SNAPSHOT"
export DCENT_CAPSULE_SOURCE_COMMIT="$SOURCE_COMMIT"
export DCENT_CAPSULE_INVOCATION_STAGE="$INVOCATION_STAGE"
export DCENT_CAPSULE_RELEASE_INVOCATION="$INVOCATION_STAGE"
export DCENT_CAPSULE_RESULT_STAGE="$RESULT_STAGE"
export DCENT_CAPSULE_RESULT_ROOT="$RESULT_ROOT"
export DCENT_CAPSULE_RESULT_CAPABILITY="$RESULT_CAPABILITY"
export DCENT_CAPSULE_CARGO_BUILD_INPUT_SNAPSHOT="$CARGO_INPUT_SNAPSHOT"
export DCENT_CAPSULE_EXTERNAL_INPUT_ROOT="$EXTERNAL_INPUT_ROOT"
export DCENT_CAPSULE_EXTERNAL_INPUT_REPO_ROOT="$EXTERNAL_INPUT_ROOT"
export DCENT_CAPSULE_RELEASE_SET_CAPABILITY_FILE="$RELEASE_SET_CAPABILITY_FILE"

bash "$SCRIPT_DIR/build-dcentrald.sh" "$CARGO_VARIANT"
python3 "$SCRIPT_DIR/release_result_stage.py" verify \
    --invocation-stage "$INVOCATION_STAGE" "$RESULT_STAGE" >/dev/null

bash "$SCRIPT_DIR/build_in_docker.sh" \
    --target "$RELEASE_TARGET" --output-dir "$RELEASE_SET_STAGE"

# Project the still-live authorities into a path-free signed audit index. The
# index excludes only itself, its detached signature, and the final set
# descriptor by a fixed schema convention; the later seal requires all three.
SOURCE_CLOSURE_CANDIDATES=("$RELEASE_SET_STAGE"/*.source-closure.json)
[ "${#SOURCE_CLOSURE_CANDIDATES[@]}" = 1 ] \
    && [ -f "${SOURCE_CLOSURE_CANDIDATES[0]}" ] || {
    echo "ERROR: private S9 release set must contain exactly one source closure" >&2
    exit 1
}
SOURCE_CLOSURE_PATH="${SOURCE_CLOSURE_CANDIDATES[0]}"
SOURCE_CLOSURE_SIGNATURE_PATH="${SOURCE_CLOSURE_PATH}.sig"
python3 "$SCRIPT_DIR/portable_release_evidence.py" create-live \
    --repo-root "$GIT_OBJECT_REPO" \
    --target "$RELEASE_TARGET" \
    --output-name "$RELEASE_NAME" \
    --source-commit "$SOURCE_COMMIT" \
    --source-snapshot "$SOURCE_SNAPSHOT" \
    --release-invocation "$INVOCATION_STAGE" \
    --cargo-input-snapshot "$CARGO_INPUT_SNAPSHOT" \
    --result-stage "$RESULT_STAGE" \
    --artifact-dir "$RELEASE_SET_STAGE" \
    --closure "$SOURCE_CLOSURE_PATH" \
    --closure-signature "$SOURCE_CLOSURE_SIGNATURE_PATH" \
    --public-key "$DCENT_RELEASE_PUBKEY_FILE" >/dev/null
PORTABLE_EVIDENCE_PATH="$RELEASE_SET_STAGE/portable-release-evidence.json"
PORTABLE_EVIDENCE_SIGNATURE_PATH="${PORTABLE_EVIDENCE_PATH}.sig"
python3 "$SCRIPT_DIR/sign_release_artifact.py" "$PORTABLE_EVIDENCE_PATH" \
    --key "$DCENT_RELEASE_SIGNING_KEY" \
    --pubkey "$DCENT_RELEASE_PUBKEY_FILE" \
    --output-sig "$PORTABLE_EVIDENCE_SIGNATURE_PATH" >/dev/null

RELEASE_SET_MANIFEST="$WORK_PARENT/release-set-${INVOCATION_ID}.files.json"
python3 "$SCRIPT_DIR/release_set_publication.py" manifest-stage \
    --capability-file "$RELEASE_SET_CAPABILITY_FILE" \
    --output "$RELEASE_SET_MANIFEST" >/dev/null
python3 "$SCRIPT_DIR/release_set_publication.py" seal-stage \
    --capability-file "$RELEASE_SET_CAPABILITY_FILE" \
    --manifest "$RELEASE_SET_MANIFEST" \
    --output-name "$RELEASE_NAME" >/dev/null
python3 "$SCRIPT_DIR/portable_release_evidence.py" verify-stage \
    --repo-root "$GIT_OBJECT_REPO" \
    --public-key "$DCENT_RELEASE_PUBKEY_FILE" \
    "$RELEASE_SET_STAGE" >/dev/null
PUBLISH_RESULT="$(python3 "$SCRIPT_DIR/release_set_publication.py" publish \
    --capability-file "$RELEASE_SET_CAPABILITY_FILE" \
    --output-parent "$RELEASES_PARENT")"
RELEASE_SET_PUBLISHED=1
PUBLISHED_PATH="$(printf '%s\n' "$PUBLISH_RESULT" \
    | python3 "$SCRIPT_DIR/release_set_publication.py" query --field published-path)"
python3 "$SCRIPT_DIR/portable_release_evidence.py" verify \
    --repo-root "$GIT_OBJECT_REPO" \
    --public-key "$DCENT_RELEASE_PUBKEY_FILE" \
    "$PUBLISHED_PATH" >/dev/null
[ -f "$PUBLISHED_PATH/$PRIMARY_ARTIFACT" ] || {
    echo "ERROR: published release lacks policy primary artifact: $PRIMARY_ARTIFACT" >&2
    exit 1
}
echo "Authoritative S9 release set: $PUBLISHED_PATH"
echo "Claim: signed exact published bytes plus Git-authenticated retained audit projections"
echo "Non-claims: build execution attestation, compiler trust, reproducibility, installed-payload equivalence, boot, and mining"
