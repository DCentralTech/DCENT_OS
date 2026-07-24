#!/usr/bin/env bash
# Hermetic host/fake-Docker proof for the S9 release-capsule orchestrator.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TMP_BASE="$(cd "${TMPDIR:-/tmp}" && pwd -P)"
TMPDIR_TEST="$(mktemp -d "$TMP_BASE/dcentos-s9-capsule.XXXXXX")"
TMPDIR_TEST="$(cd "$TMPDIR_TEST" && pwd -P)"
safe_test_cleanup() {
    case "$TMPDIR_TEST" in
        "$TMP_BASE"/dcentos-s9-capsule.*) ;;
        *) echo "refusing unsafe harness cleanup: $TMPDIR_TEST" >&2; return 1;;
    esac
    [ "$(dirname "$TMPDIR_TEST")" = "$TMP_BASE" ] && [ ! -L "$TMPDIR_TEST" ] || {
        echo "refusing changed harness cleanup root: $TMPDIR_TEST" >&2; return 1
    }
    rm -rf -- "$TMPDIR_TEST"
}
trap safe_test_cleanup EXIT

LIVE_REPO="$TMPDIR_TEST/live-repo"
OUTPUT_ROOT="$TMPDIR_TEST/public-output"
FAKE_BIN="$TMPDIR_TEST/fake-bin"
FAKE_STATE="$TMPDIR_TEST/docker-state"
DOCKER_LOG="$TMPDIR_TEST/docker.log"
AUDIT_LOG="$TMPDIR_TEST/audit.log"
mkdir -p "$LIVE_REPO/DCENT_OS_Antminer/scripts" "$FAKE_BIN" \
    "$FAKE_STATE/volumes" "$FAKE_STATE/images" "$FAKE_STATE/containers"
: > "$DOCKER_LOG"

# Pin the production driver invariants that the lightweight authenticated test
# driver below models. These assertions prevent the harness from passing after
# a regression back to the live tree, global volume, mutable tag execution, or
# direct public output.
grep -Fq 'EXPECTED_CAPSULE_PROJECT="$(cd "$CAPSULE_SOURCE_TREE/DCENT_OS_Antminer" && pwd)"' \
    "$SCRIPT_DIR/build_in_docker.sh"
grep -Fq 'VOLUME_NAME="$(python3 "$SCRIPT_DIR/release_invocation.py" query' \
    "$SCRIPT_DIR/build_in_docker.sh"
grep -Fq 'IMAGE_NAME="$(python3 "$SCRIPT_DIR/release_docker_resources.py"' \
    "$SCRIPT_DIR/build_in_docker.sh"
grep -Fq 'docker build -f "$DOCKER_BUILD_DOCKERFILE" -t "$IMAGE_NAME" "$DOCKER_BUILD_CTX"' \
    "$SCRIPT_DIR/build_in_docker.sh"
grep -Fq 'BUILD_CONTAINER_ID="$(docker image inspect --format' \
    "$SCRIPT_DIR/build_in_docker.sh"
grep -Fq '"$BUILD_CONTAINER_ID" bash -c' "$SCRIPT_DIR/build_in_docker.sh"
grep -Fq -- '-v "${POSIX_PROJECT_DIR}:/src:ro"' "$SCRIPT_DIR/build_in_docker.sh"
grep -Fq -- '--output-dir for its private release-set stage' "$SCRIPT_DIR/build_in_docker.sh"
grep -Fq 'portable_release_evidence.py" create-live' \
    "$SCRIPT_DIR/build_s9_release_capsule.sh"
grep -Fq 'sign_release_artifact.py" "$PORTABLE_EVIDENCE_PATH"' \
    "$SCRIPT_DIR/build_s9_release_capsule.sh"
grep -Fq 'RELEASE_TARGET=s9' "$SCRIPT_DIR/build_s9_release_capsule.sh"
grep -Fq 'release_capsule_target_policy.py' \
    "$SCRIPT_DIR/build_s9_release_capsule.sh"
grep -Fq 'portable_release_evidence.py" verify-stage' \
    "$SCRIPT_DIR/build_s9_release_capsule.sh"
grep -Fq 'portable_release_evidence.py" verify' \
    "$SCRIPT_DIR/build_s9_release_capsule.sh"
grep -Fq 'release_signing_authority.py" create' \
    "$SCRIPT_DIR/build_s9_release_capsule.sh"
grep -Fq -- '--result-output "$RESULT_CREATE_RESULT_FILE"' \
    "$SCRIPT_DIR/build_s9_release_capsule.sh"
grep -Fq 'result-stage cleanup failed; recovery result retained:' \
    "$SCRIPT_DIR/build_s9_release_capsule.sh"
grep -Fq 'result-stage creation recovery failed; locator retained:' \
    "$SCRIPT_DIR/build_s9_release_capsule.sh"
grep -Fq 'result-stage recovery result is unreadable; retained:' \
    "$SCRIPT_DIR/build_s9_release_capsule.sh"
grep -Fq -- '--result-output "$SIGNING_AUTHORITY_RESULT_FILE"' \
    "$SCRIPT_DIR/build_s9_release_capsule.sh"
grep -Fq 'signing-authority cleanup failed; recovery result retained:' \
    "$SCRIPT_DIR/build_s9_release_capsule.sh"
grep -Fq 'registered signing-authority recovery result is missing; upstream authority retained:' \
    "$SCRIPT_DIR/build_s9_release_capsule.sh"
grep -Fq 'dependent cleanup failed; invocation retained for recovery:' \
    "$SCRIPT_DIR/build_s9_release_capsule.sh"
grep -Fq 'cleanup helper source retained for recovery:' \
    "$SCRIPT_DIR/build_s9_release_capsule.sh"
grep -Fq 'DCENT_RELEASE_SIGNING_KEY="$SIGNING_AUTHORITY_PRIVATE_KEY"' \
    "$SCRIPT_DIR/build_s9_release_capsule.sh"
grep -Fq 'clean|exact_git_object_snapshot' "$SCRIPT_DIR/lib/release_envelope.sh"
if grep -Fq 'VOLUME_NAME="dcentos-build-work"' \
    <(sed -n '/if \[ "${DCENT_RELEASE_CAPSULE_MODE/,/fi/p' "$SCRIPT_DIR/build_in_docker.sh"); then
    echo "capsule branch names a global Buildroot volume" >&2
    exit 1
fi

for helper in \
    atomic_publish_directory.py atomic_publish_file.py durable_file_io.py \
    build_s9_release_capsule.sh source_snapshot.py release_invocation.py \
    release_result_stage.py release_set_publication.py release_publication.py \
    release_docker_resources.py release_signing_authority.py \
    release_capsule_target_policy.py sign_release_artifact.py \
    sign_release_receipt.py \
    firmware_release_name.sh verify_release_keypair.sh; do
    cp "$SCRIPT_DIR/$helper" "$LIVE_REPO/DCENT_OS_Antminer/scripts/$helper"
done
grep -Fq 'from atomic_publish_file import' \
    "$LIVE_REPO/DCENT_OS_Antminer/scripts/release_set_publication.py"
grep -Fq 'from atomic_publish_directory import' \
    "$LIVE_REPO/DCENT_OS_Antminer/scripts/release_set_publication.py"
grep -Fq 'from durable_file_io import' \
    "$LIVE_REPO/DCENT_OS_Antminer/scripts/atomic_publish_file.py"
chmod +x "$LIVE_REPO/DCENT_OS_Antminer/scripts/build_s9_release_capsule.sh"
grep -Fq 'release-set cleanup failed; capability retained for recovery:' \
    "$LIVE_REPO/DCENT_OS_Antminer/scripts/build_s9_release_capsule.sh"
grep -Fq 'release_set_destroy_error' \
    "$LIVE_REPO/DCENT_OS_Antminer/scripts/build_s9_release_capsule.sh"

# The orchestrator needs only create/query/destroy from this outer-owned input
# authority. The production helper has its own exhaustive adversarial suite.
cat > "$LIVE_REPO/DCENT_OS_Antminer/scripts/build_input_snapshot.py" <<'PY'
#!/usr/bin/env python3
import argparse, hashlib, json, pathlib, secrets, shutil, tempfile
p=argparse.ArgumentParser(); s=p.add_subparsers(dest="cmd", required=True)
c=s.add_parser("create"); c.add_argument("--repo-root"); c.add_argument("--selection-root")
c.add_argument("--build-input-manifest"); c.add_argument("--target"); c.add_argument("--stage-parent", required=True)
q=s.add_parser("query-result"); q.add_argument("--field", required=True)
d=s.add_parser("destroy"); d.add_argument("--token", required=True); d.add_argument("snapshot")
a=p.parse_args()
if a.cmd=="create":
    parent=pathlib.Path(a.stage_parent); parent.mkdir(parents=True, exist_ok=True)
    stage=pathlib.Path(tempfile.mkdtemp(prefix="input-", dir=parent)); token=secrets.token_hex(32)
    snap=stage/"snapshot.json"; snap.write_text(json.dumps({"token_sha256":hashlib.sha256(token.encode()).hexdigest()}))
    print(json.dumps({"snapshot":str(snap),"stage":str(stage),"destroy_token":token}))
elif a.cmd=="query-result": print(json.load(__import__("sys").stdin)[a.field])
else:
    snap=pathlib.Path(a.snapshot); value=json.loads(snap.read_text())
    if hashlib.sha256(a.token.encode()).hexdigest()!=value["token_sha256"]: raise SystemExit("bad token")
    shutil.rmtree(snap.parent)
PY

# The real portable-evidence helper has a dedicated Git/signature/exact-set
# suite. This interface stub pins outer-driver ordering: project while private,
# sign before sealing, then independently verify only after publication.
cat > "$LIVE_REPO/DCENT_OS_Antminer/scripts/portable_release_evidence.py" <<'PY'
#!/usr/bin/env python3
import argparse, json, os, pathlib, shutil
p=argparse.ArgumentParser(); s=p.add_subparsers(dest="cmd", required=True)
c=s.add_parser("create-live")
for name in ("repo-root","target","output-name","source-commit","source-snapshot","release-invocation","cargo-input-snapshot","result-stage","artifact-dir","closure","closure-signature","public-key"):
    c.add_argument("--"+name, required=True)
for command in ("verify-stage", "verify"):
    v=s.add_parser(command); v.add_argument("--repo-root", required=True); v.add_argument("--public-key", required=True); v.add_argument("release_dir")
a=p.parse_args()
if a.cmd=="create-live":
    if a.target != "s9": raise SystemExit("outer S9 capsule did not bind portable target")
    if not a.output_name.startswith("DCENTOS_XIL1_S9_"): raise SystemExit("outer S9 capsule did not bind release name")
    out=pathlib.Path(a.artifact_dir)
    shutil.copyfile(a.source_snapshot, out/"release-source-snapshot.json")
    shutil.copyfile(pathlib.Path(a.release_invocation)/"invocation.json", out/"release-invocation.json")
    shutil.copyfile(a.cargo_input_snapshot, out/"release-cargo-input.json")
    (out/"release-result-audit.json").write_text('{"claim":"audit-only"}\n', encoding="ascii")
    (out/"portable-release-evidence.json").write_text(json.dumps({"schema":"harness-portable-evidence.v1"}, separators=(",",":"), sort_keys=True)+"\n", encoding="ascii")
    with open(os.environ["HARNESS_AUDIT_LOG"],"a",encoding="utf-8") as log: log.write("portable-create=private\n")
elif a.cmd=="verify-stage":
    root=pathlib.Path(a.release_dir)
    if not (root/".dcent-release-set.json").is_file(): raise SystemExit("unsealed release stage")
    if any(pathlib.Path(os.environ["HARNESS_OUTPUT_ROOT"], "releases").glob("*")): raise SystemExit("stage verified after publication")
    with open(os.environ["HARNESS_AUDIT_LOG"],"a",encoding="utf-8") as log: log.write("portable-verify=stage\n")
else:
    root=pathlib.Path(a.release_dir)
    for name in ("portable-release-evidence.json","portable-release-evidence.json.sig","release-source-snapshot.json","release-invocation.json","release-cargo-input.json","release-packaging-input.json","release-result-audit.json"):
        if not (root/name).is_file(): raise SystemExit("missing portable member: "+name)
    with open(os.environ["HARNESS_AUDIT_LOG"],"a",encoding="utf-8") as log: log.write("portable-verify=published\n")
PY

cat > "$LIVE_REPO/DCENT_OS_Antminer/scripts/build-dcentrald.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
script_dir="$(cd "$(dirname "$0")" && pwd)"
case "$DCENT_RELEASE_SIGNING_KEY" in
  "$HARNESS_OUTPUT_ROOT"/.dcent-release-capsules/signing-authorities/*/private-key.pem) ;;
  *) echo 'Cargo signer did not receive the private authority snapshot' >&2; exit 1;;
esac
case "$DCENT_RELEASE_PUBKEY_FILE" in
  "$HARNESS_OUTPUT_ROOT"/.dcent-release-capsules/signing-authorities/*/public-key.pem) ;;
  *) echo 'Cargo verifier did not receive the public authority snapshot' >&2; exit 1;;
esac
python3 "$script_dir/release_signing_authority.py" verify \
    --invocation-stage "$DCENT_CAPSULE_INVOCATION_STAGE" \
    "$(dirname "$DCENT_RELEASE_SIGNING_KEY")" >/dev/null
test "$(cat "$DCENT_RELEASE_SIGNING_KEY")" = "$HARNESS_EXPECTED_PRIVATE_KEY"
test "$(cat "$DCENT_RELEASE_PUBKEY_FILE")" = "$HARNESS_EXPECTED_PUBLIC_KEY"
# Rotate both original operator pathnames after admission. All later signing
# and verification must continue through the invocation snapshot. The signal
# cleanup case opts out only because its wrapper is intentionally killed.
if [ "${HARNESS_ROTATE_KEYS:-1}" = 1 ]; then
    cp "$HARNESS_OTHER_PRIVATE_KEY" "$HARNESS_ORIGINAL_PRIVATE_KEY"
    cp "$HARNESS_OTHER_PUBLIC_KEY" "$HARNESS_ORIGINAL_PUBLIC_KEY"
    printf 'signing-private=%s\nsigning-public=%s\noriginal-keys-rotated=yes\n' \
        "$DCENT_RELEASE_SIGNING_KEY" "$DCENT_RELEASE_PUBKEY_FILE" >> "$HARNESS_AUDIT_LOG"
fi
python3 "$script_dir/release_result_stage.py" verify \
    --invocation-stage "$DCENT_CAPSULE_INVOCATION_STAGE" "$DCENT_CAPSULE_RESULT_STAGE" >/dev/null
root="$(python3 "$script_dir/release_result_stage.py" query --field result_root \
    --invocation-stage "$DCENT_CAPSULE_INVOCATION_STAGE" "$DCENT_CAPSULE_RESULT_STAGE")"
mkdir -p "$root/target/armv7-unknown-linux-musleabihf/release"
printf 'sealed cargo result from snapshot A\n' > \
    "$root/target/armv7-unknown-linux-musleabihf/release/dcentrald"
python3 "$script_dir/release_result_stage.py" seal \
    --capability "$DCENT_CAPSULE_RESULT_CAPABILITY" \
    --invocation-stage "$DCENT_CAPSULE_INVOCATION_STAGE" "$DCENT_CAPSULE_RESULT_STAGE" >/dev/null
SH
chmod +x "$LIVE_REPO/DCENT_OS_Antminer/scripts/build-dcentrald.sh"

cat > "$LIVE_REPO/DCENT_OS_Antminer/scripts/build_in_docker.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
script_dir="$(cd "$(dirname "$0")" && pwd)"; project_dir="$(dirname "$script_dir")"
output=""; while [ "$#" -gt 0 ]; do case "$1" in --output-dir) output=$2; shift 2;; *) shift;; esac; done
stage="$(python3 "$script_dir/release_set_publication.py" query --field stage-path \
    < "$DCENT_CAPSULE_RELEASE_SET_CAPABILITY_FILE")"
[ "$output" = "$stage" ]
verified="$(python3 "$script_dir/source_snapshot.py" verify-against-git \
    --repo-root "$DCENT_CAPSULE_GIT_OBJECT_REPO" --commit "$DCENT_CAPSULE_SOURCE_COMMIT" \
    "$DCENT_CAPSULE_SOURCE_SNAPSHOT")"
tree="$(printf '%s\n' "$verified" | python3 "$script_dir/source_snapshot.py" query-verified --field tree)"
[ "$project_dir" = "$tree/DCENT_OS_Antminer" ]
[ "$(cat "$project_dir/source-marker.txt")" = snapshot-A ]
[ "$(cat "$HARNESS_LIVE_ROOT/DCENT_OS_Antminer/source-marker.txt")" = mutated-live-B ]
if find "$HARNESS_OUTPUT_ROOT/releases" -mindepth 1 -print -quit | grep -q .; then
    echo 'public release existed before promotion' >&2; exit 1
fi
invocation="$(python3 "$script_dir/release_invocation.py" query --field invocation_id "$DCENT_CAPSULE_INVOCATION_STAGE")"
volume="$(python3 "$script_dir/release_invocation.py" query --field buildroot_volume "$DCENT_CAPSULE_INVOCATION_STAGE")"
tag="$(python3 "$script_dir/release_docker_resources.py" query-builder-tag "$DCENT_CAPSULE_INVOCATION_STAGE")"
[ "$volume" != dcentos-build-work ]
docker volume inspect -- "$volume" >/dev/null 2>&1 && { echo 'pre-existing volume adopted' >&2; exit 1; }
docker volume create --label "invocation=$invocation" --label role=buildroot -- "$volume" >/dev/null
docker build -t "$tag" "$project_dir" >/dev/null
image_id="$(docker image inspect --format '{{.Id}}' "$tag")"
case "$image_id" in sha256:[0-9a-f][0-9a-f]*) ;; *) exit 1;; esac
cleanup() { docker volume rm -- "$volume" >/dev/null 2>&1 || true; docker image rm "$tag" >/dev/null 2>&1 || true; }
trap cleanup EXIT
docker run --rm -v "$project_dir:/src:ro" -v "$output:/out" "$image_id" true
printf 'source=%s\nlive=%s\nvolume=%s\ntag=%s\nimage=%s\nprivate=%s\n' \
    "$(cat "$project_dir/source-marker.txt")" \
    "$(cat "$HARNESS_LIVE_ROOT/DCENT_OS_Antminer/source-marker.txt")" \
    "$volume" "$tag" "$image_id" "$output" >> "$HARNESS_AUDIT_LOG"
python3 "$script_dir/release_publication.py" stdin --output "$output/dcentos-sysupgrade-118.tar" \
    <<< 'private firmware bytes from snapshot A' >/dev/null
printf '{"schema":"harness-packaging-input.v1"}\n' > "$output/release-packaging-input.json"
printf '{"schema":"harness-source-closure.v1"}\n' > \
    "$output/dcentos-sysupgrade-118.tar.source-closure.json"
head -c 64 /dev/zero > "$output/dcentos-sysupgrade-118.tar.source-closure.json.sig"
if [ "${HARNESS_FAIL_AFTER_PRIVATE:-0}" = 1 ]; then exit 91; fi
if [ -n "${HARNESS_SIGNAL_READY:-}" ]; then
    # Docker resources are already disposed in the real driver before control
    # returns. Model that state, then let the outer signal cleanup own stages.
    cleanup; trap - EXIT; : > "$HARNESS_SIGNAL_READY"; sleep 30
fi
SH
chmod +x "$LIVE_REPO/DCENT_OS_Antminer/scripts/build_in_docker.sh"
printf 'snapshot-A\n' > "$LIVE_REPO/DCENT_OS_Antminer/source-marker.txt"
printf '# test selection authority\n' > "$LIVE_REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest"

cat > "$FAKE_BIN/docker" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
state=${FAKE_DOCKER_STATE:?}; log=${FAKE_DOCKER_LOG:?}; printf '%q ' "$@" >> "$log"; printf '\n' >> "$log"
digest="sha256:$(printf '7%.0s' {1..64})"; kind=${1:-}; shift || true
case "$kind" in
info) exit 0;;
volume)
    action=$1; shift
    [ "${1:-}" = -- ] && shift || true; name=${!#}
    case "$action" in
      inspect) [ -f "$state/volumes/$name" ];;
      create) printf '%s\n' "$*" > "$state/volumes/$name"; printf '%s\n' "$name";;
      rm) rm -f -- "$state/volumes/$name";; esac;;
image)
    action=$1; shift
    case "$action" in
      inspect) [ "${1:-}" = --format ] && shift 2; tag=$1; [ -f "$state/images/$tag" ]; printf '%s\n' "$digest";;
      rm) rm -f -- "$state/images/$1";;
      tag) cp "$state/images/$1" "$state/images/$2";; esac;;
build)
    tag=""; while [ "$#" -gt 0 ]; do case "$1" in -t) tag=$2; shift 2;; *) shift;; esac; done
    printf '%s\n' "$digest" > "$state/images/$tag";;
run)
    source_mount=""; image=""
    while [ "$#" -gt 0 ]; do case "$1" in --rm) shift;; -v) case "$2" in *:/src:ro) source_mount=${2%:/src:ro};; esac; shift 2;; sha256:*) image=$1; shift; break;; *) shift;; esac; done
    [ "$image" = "$digest" ]; [ "$(cat "$source_mount/source-marker.txt")" = snapshot-A ];;
container) exit 1;;
*) exit 99;; esac
SH
chmod +x "$FAKE_BIN/docker"

git -C "$LIVE_REPO" init -q
git -C "$LIVE_REPO" config user.email test@example.invalid
git -C "$LIVE_REPO" config user.name 'Capsule Harness'
git -C "$LIVE_REPO" add .
git -C "$LIVE_REPO" commit -qm 'snapshot A'
printf 'mutated-live-B\n' > "$LIVE_REPO/DCENT_OS_Antminer/source-marker.txt"

SIGNING_KEY="$TMPDIR_TEST/signing.pem"; PUBLIC_KEY="$TMPDIR_TEST/public.pem"
OTHER_KEY="$TMPDIR_TEST/other.pem"; OTHER_PUBLIC_KEY="$TMPDIR_TEST/other-public.pem"
openssl genpkey -algorithm ED25519 -out "$SIGNING_KEY" >/dev/null 2>&1
openssl pkey -in "$SIGNING_KEY" -pubout -out "$PUBLIC_KEY" >/dev/null 2>&1
openssl genpkey -algorithm ED25519 -out "$OTHER_KEY" >/dev/null 2>&1
openssl pkey -in "$OTHER_KEY" -pubout -out "$OTHER_PUBLIC_KEY" >/dev/null 2>&1
EXPECTED_PRIVATE_KEY="$(cat "$SIGNING_KEY")"
EXPECTED_PUBLIC_KEY="$(cat "$PUBLIC_KEY")"
SIGNING_KEY_BASELINE="$TMPDIR_TEST/signing-baseline.pem"
PUBLIC_KEY_BASELINE="$TMPDIR_TEST/public-baseline.pem"
cp "$SIGNING_KEY" "$SIGNING_KEY_BASELINE"
cp "$PUBLIC_KEY" "$PUBLIC_KEY_BASELINE"
MANIFEST_PUBLIC_KEY_HEX="$(openssl pkey -pubin -in "$PUBLIC_KEY" -outform DER \
    | python3 -c 'import sys; print(sys.stdin.buffer.read()[-32:].hex())')"
run_capsule_raw() {
    env PATH="$FAKE_BIN:$PATH" FAKE_DOCKER_STATE="$FAKE_STATE" FAKE_DOCKER_LOG="$DOCKER_LOG" \
        HARNESS_LIVE_ROOT="$LIVE_REPO" HARNESS_OUTPUT_ROOT="$OUTPUT_ROOT" HARNESS_AUDIT_LOG="$AUDIT_LOG" \
        HARNESS_ORIGINAL_PRIVATE_KEY="$SIGNING_KEY" HARNESS_ORIGINAL_PUBLIC_KEY="$PUBLIC_KEY" \
        HARNESS_OTHER_PRIVATE_KEY="$OTHER_KEY" HARNESS_OTHER_PUBLIC_KEY="$OTHER_PUBLIC_KEY" \
        HARNESS_EXPECTED_PRIVATE_KEY="$EXPECTED_PRIVATE_KEY" HARNESS_EXPECTED_PUBLIC_KEY="$EXPECTED_PUBLIC_KEY" \
        DCENT_RELEASE_SIGNING_KEY="$SIGNING_KEY" DCENT_RELEASE_PUBKEY_FILE="$PUBLIC_KEY" \
        DCENT_MANIFEST_PUBLIC_KEY_HEX="$MANIFEST_PUBLIC_KEY_HEX" \
        DCENT_RUST_BUILDER_BASE="rust-test@sha256:$(printf 'b%.0s' {1..64})" \
        DCENT_TOOLCHAIN_SHA256_VERIFIED=1 "$@" \
        "$LIVE_REPO/DCENT_OS_Antminer/scripts/build_s9_release_capsule.sh" --output-root "$OUTPUT_ROOT"
}
run_capsule() {
    set +e
    run_capsule_raw "$@"
    status=$?
    cp "$SIGNING_KEY_BASELINE" "$SIGNING_KEY"
    cp "$PUBLIC_KEY_BASELINE" "$PUBLIC_KEY"
    set -e
    return "$status"
}

# Admission rejects malformed builder identity before allocating an invocation.
# A mismatched signing pair allocates only its invocation/signing authority;
# cleanup may inspect the derived Docker names but must never create or run one.
if run_capsule DCENT_RUST_BUILDER_BASE='rust-test@sha256:abcd-junk' \
    > "$TMPDIR_TEST/bad-builder.out" 2>&1; then
    echo 'malformed builder digest unexpectedly admitted' >&2; exit 1
fi
grep -q 'exact lowercase sha256 digest' "$TMPDIR_TEST/bad-builder.out"
if run_capsule DCENT_RELEASE_PUBKEY_FILE="$OTHER_PUBLIC_KEY" \
    > "$TMPDIR_TEST/bad-key.out" 2>&1; then
    echo 'mismatched release keypair unexpectedly admitted' >&2; exit 1
fi
grep -Eq 'match|mismatch|public key' "$TMPDIR_TEST/bad-key.out"
! grep -Eq '^volume create |^build |^run ' "$DOCKER_LOG"
test -z "$(find "$FAKE_STATE/volumes" -mindepth 1 -print -quit)"

# Failure after private bytes never creates a public release and destroys all
# invocation/source/result/release-set stages and exact Docker resources.
if run_capsule HARNESS_FAIL_AFTER_PRIVATE=1 > "$TMPDIR_TEST/failure.out" 2>&1; then
    echo 'injected private-stage failure unexpectedly succeeded' >&2; exit 1
fi
test ! -d "$OUTPUT_ROOT/releases" || ! find "$OUTPUT_ROOT/releases" -mindepth 1 -print -quit | grep -q .
test ! -e "$FAKE_STATE/volumes/dcentos-build-work"
test -z "$(find "$FAKE_STATE/volumes" -mindepth 1 -print -quit)"
test ! -d "$OUTPUT_ROOT/.dcent-release-capsules/signing-authorities" \
    || test -z "$(find "$OUTPUT_ROOT/.dcent-release-capsules/signing-authorities" -name private-key.pem -print -quit)"
test ! -d "$OUTPUT_ROOT/.dcent-release-capsules" \
    || test -z "$(find "$OUTPUT_ROOT/.dcent-release-capsules" -name 'result-stage-*.result.json' -print -quit)"

# A TERM while the private stage is active is never reported as success and
# cannot create a public set.
READY="$TMPDIR_TEST/signal.ready"; rm -f "$READY"
(run_capsule_raw HARNESS_ROTATE_KEYS=0 HARNESS_SIGNAL_READY="$READY" \
    > "$TMPDIR_TEST/signal.out" 2>&1) & signal_pid=$!
for _ in $(seq 1 500); do [ -e "$READY" ] && break; sleep 0.02; done
test -e "$READY"
capsule_children="$(pgrep -P "$signal_pid" || true)"
test "$(printf '%s\n' "$capsule_children" | grep -c '[0-9]')" -eq 1
kill -TERM "$capsule_children"
if wait "$signal_pid"; then echo 'terminated capsule reported success' >&2; exit 1; fi
test ! -d "$OUTPUT_ROOT/releases" || ! find "$OUTPUT_ROOT/releases" -mindepth 1 -print -quit | grep -q .

# A successful run publishes one exact directory only after the private driver
# returned; live mutation B never becomes a source mount or artifact input.
run_capsule > "$TMPDIR_TEST/success.out"
test ! -d "$OUTPUT_ROOT/.dcent-release-capsules" \
    || test -z "$(find "$OUTPUT_ROOT/.dcent-release-capsules" -name 'result-stage-*.result.json' -print -quit)"
PUBLISHED="$(find "$OUTPUT_ROOT/releases" -mindepth 1 -maxdepth 1 -type d -print -quit)"
test -n "$PUBLISHED"
test "$(cat "$PUBLISHED/dcentos-sysupgrade-118.tar")" = 'private firmware bytes from snapshot A'
test -f "$PUBLISHED/release-source-snapshot.json"
test -f "$PUBLISHED/release-invocation.json"
test -f "$PUBLISHED/release-cargo-input.json"
test -f "$PUBLISHED/release-packaging-input.json"
test -f "$PUBLISHED/release-result-audit.json"
test -f "$PUBLISHED/portable-release-evidence.json"
test -f "$PUBLISHED/portable-release-evidence.json.sig"
# These portable descriptor copies are evidence, not the capability-owned live
# authorities accepted by the admission verifier. Pin that distinction so the
# release path cannot silently advertise post-cleanup independent verification.
if python3 "$SCRIPT_DIR/source_snapshot.py" verify "$PUBLISHED/release-source-snapshot.json" \
    > "$TMPDIR_TEST/portable-source.out" 2>&1; then
    echo 'portable source descriptor unexpectedly acted as a live snapshot authority' >&2; exit 1
fi
grep -Eq 'owned|descriptor' "$TMPDIR_TEST/portable-source.out"
grep -Fq 'explicit post-cleanup portable-audit verifier' "$SCRIPT_DIR/source_closure.py"
test ! -e "$PUBLISHED/knowledge-base/extractions/s9/kernel.bin"
grep -q '^portable-create=private$' "$AUDIT_LOG"
grep -q '^portable-verify=stage$' "$AUDIT_LOG"
grep -q '^portable-verify=published$' "$AUDIT_LOG"
grep -q '^signing-private=.*/.dcent-release-capsules/signing-authorities/.*/private-key.pem$' "$AUDIT_LOG"
grep -q '^signing-public=.*/.dcent-release-capsules/signing-authorities/.*/public-key.pem$' "$AUDIT_LOG"
grep -q '^original-keys-rotated=yes$' "$AUDIT_LOG"
grep -q '^source=snapshot-A$' "$AUDIT_LOG"
grep -q '^live=mutated-live-B$' "$AUDIT_LOG"
grep -q '^volume=dcentos-ri-s9-.*-buildroot$' "$AUDIT_LOG"
grep -q '^tag=dcentos-release-builder:' "$AUDIT_LOG"
grep -q '^image=sha256:7777777777777777777777777777777777777777777777777777777777777777$' "$AUDIT_LOG"
grep -q '/src:ro' "$DOCKER_LOG"
! grep -q 'dcentos-build-work' "$DOCKER_LOG"
openssl pkeyutl -verify -rawin -pubin -inkey "$PUBLIC_KEY" \
    -in "$PUBLISHED/portable-release-evidence.json" \
    -sigfile "$PUBLISHED/portable-release-evidence.json.sig" >/dev/null
test ! -d "$OUTPUT_ROOT/.dcent-release-capsules/signing-authorities" \
    || test -z "$(find "$OUTPUT_ROOT/.dcent-release-capsules/signing-authorities" -name private-key.pem -print -quit)"
FIRST_SHA="$(sha256sum "$PUBLISHED/dcentos-sysupgrade-118.tar" | awk '{print $1}')"

# Same-name publication is no-replace. A second complete invocation fails and
# cannot modify or delete the already authoritative release set.
if run_capsule > "$TMPDIR_TEST/collision.out" 2>&1; then
    echo 'same-name release collision unexpectedly succeeded' >&2; exit 1
fi
test "$(sha256sum "$PUBLISHED/dcentos-sysupgrade-118.tar" | awk '{print $1}')" = "$FIRST_SHA"
test "$(find "$OUTPUT_ROOT/releases" -mindepth 1 -maxdepth 1 -type d | wc -l)" -eq 1

# Invocation/result descriptor swapping is rejected by the production helper.
CONTROL="$TMPDIR_TEST/swap-control"; mkdir -p "$CONTROL"
i1="$(python3 "$SCRIPT_DIR/release_invocation.py" create --stage-parent "$CONTROL" --name one)"
i2="$(python3 "$SCRIPT_DIR/release_invocation.py" create --stage-parent "$CONTROL" --name two)"
s1="$(printf '%s\n' "$i1" | python3 "$SCRIPT_DIR/release_invocation.py" query-result --field stage)"
s2="$(printf '%s\n' "$i2" | python3 "$SCRIPT_DIR/release_invocation.py" query-result --field stage)"
r1="$(python3 "$SCRIPT_DIR/release_result_stage.py" create --stage-parent "$CONTROL" \
    --invocation-stage "$s1" --result-output "$TMPDIR_TEST/swap-result.json")"
r1s="$(printf '%s\n' "$r1" | python3 "$SCRIPT_DIR/release_result_stage.py" query-result --field stage)"
if python3 "$SCRIPT_DIR/release_result_stage.py" verify --invocation-stage "$s2" "$r1s" \
    > "$TMPDIR_TEST/swap.out" 2>&1; then
    echo 'result descriptor swap unexpectedly verified' >&2; exit 1
fi
grep -Eq 'invocation|different|bound' "$TMPDIR_TEST/swap.out"

echo 'S9 release capsule driver fake-Docker tests passed'
