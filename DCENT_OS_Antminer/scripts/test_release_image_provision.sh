#!/bin/sh
# Adversarial host-side tests for release-image marker provisioning.
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
. "$SCRIPT_DIR/lib/release_image_provision.sh"

TEST_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/dcentos-release-image-test.XXXXXX")
cleanup() {
    chmod -R u+w "$TEST_ROOT" 2>/dev/null || true
    rm -rf "$TEST_ROOT"
}
trap cleanup EXIT HUP INT TERM

fail_test() {
    echo "release image provisioning test failed: $*" >&2
    exit 1
}

make_target() {
    _test_target="$1"
    _test_root_hash="$2"
    mkdir -p "${_test_target}/etc/dcentos"
    printf 'root:%s:20000:0:99999:7:::\n' "$_test_root_hash" > "${_test_target}/etc/shadow"
    printf 'daemon:*:20000:0:99999:7:::\n' >> "${_test_target}/etc/shadow"
    # Buildroot's post-build staging tree may still be 0644; its device table
    # applies the final 0600 mode later under fakeroot.
    chmod 644 "${_test_target}/etc/shadow"
}

provision_dev() {
    DCENT_RELEASE_IMAGE=0 dcent_provision_release_image "$1" test >/dev/null 2>&1
}

provision_release() {
    DCENT_RELEASE_IMAGE=1 dcent_provision_release_image "$1" test >/dev/null 2>&1
}

TARGET="${TEST_ROOT}/dev-regular"
make_target "$TARGET" dcentral
printf 'stale\n' > "${TARGET}/etc/dcentos/release-image"
provision_dev "$TARGET" || fail_test "dev build rejected removable regular marker"
[ ! -e "${TARGET}/etc/dcentos/release-image" ] || fail_test "dev marker survived"

TARGET="${TEST_ROOT}/dev-directory"
make_target "$TARGET" dcentral
mkdir "${TARGET}/etc/dcentos/release-image"
printf 'preserve-dev-directory\n' > "${TARGET}/etc/dcentos/release-image/sentinel"
provision_dev "$TARGET" || fail_test "dev build rejected a quarantinable marker directory"
[ ! -e "${TARGET}/etc/dcentos/release-image" ] || fail_test "dev marker directory retained its runtime name"
set -- "${TARGET}/etc/dcentos"/.release-image.rejected.*
[ "$#" -eq 1 ] && [ -d "$1" ] || fail_test "dev marker directory was not quarantined"
[ "$(cat "$1/sentinel")" = preserve-dev-directory ] || fail_test "dev marker quarantine lost contents"

TARGET="${TEST_ROOT}/dev-symlink"
make_target "$TARGET" dcentral
VICTIM="${TEST_ROOT}/dev-symlink-victim"
printf 'important-dev-bytes\n' > "$VICTIM"
ln -s "$VICTIM" "${TARGET}/etc/dcentos/release-image"
provision_dev "$TARGET" || fail_test "dev build rejected removable marker symlink"
[ "$(cat "$VICTIM")" = important-dev-bytes ] || fail_test "dev marker symlink target changed"

TARGET="${TEST_ROOT}/unlocked-release"
make_target "$TARGET" dcentral
printf 'stale-release-claim\n' > "${TARGET}/etc/dcentos/release-image"
if provision_release "$TARGET"; then
    fail_test "release marker was stamped with an unlocked root account"
fi
[ ! -e "${TARGET}/etc/dcentos/release-image" ] || fail_test "unlocked release retained a stale marker"

TARGET="${TEST_ROOT}/duplicate-root"
make_target "$TARGET" '*'
printf 'root:*:20000:0:99999:7:::\n' >> "${TARGET}/etc/shadow"
if provision_release "$TARGET"; then
    fail_test "release accepted duplicate root shadow entries"
fi

TARGET="${TEST_ROOT}/shadow-symlink"
make_target "$TARGET" '*'
SHADOW_VICTIM="${TEST_ROOT}/shadow-victim"
cp "${TARGET}/etc/shadow" "$SHADOW_VICTIM"
rm "${TARGET}/etc/shadow"
ln -s "$SHADOW_VICTIM" "${TARGET}/etc/shadow"
if provision_release "$TARGET"; then
    fail_test "release accepted a symlinked shadow database"
fi
[ ! -e "${TARGET}/etc/dcentos/release-image" ] || fail_test "shadow symlink failure left a marker"

TARGET="${TEST_ROOT}/shadow-writable-mode"
make_target "$TARGET" '*'
chmod 666 "${TARGET}/etc/shadow"
if provision_release "$TARGET"; then
    fail_test "release accepted a group/world-writable shadow database"
fi
[ ! -e "${TARGET}/etc/dcentos/release-image" ] || fail_test "unsafe shadow mode failure left a marker"

TARGET="${TEST_ROOT}/locked-release"
make_target "$TARGET" '*'
printf 'grace\n' > "${TARGET}/etc/dcentos/first-boot-grace"
provision_release "$TARGET" || fail_test "locked release was rejected"
[ ! -e "${TARGET}/etc/dcentos/first-boot-grace" ] || fail_test "grace marker survived"
[ -f "${TARGET}/etc/dcentos/release-image" ] || fail_test "release marker is absent"
[ ! -L "${TARGET}/etc/dcentos/release-image" ] || fail_test "release marker is a symlink"
[ "$(stat -c '%h' "${TARGET}/etc/dcentos/release-image")" = 1 ] || fail_test "release marker is multiply linked"
[ "$(stat -c '%a' "${TARGET}/etc/dcentos/release-image")" = 644 ] || fail_test "release marker mode is not 0644"

TARGET="${TEST_ROOT}/release-symlink"
make_target "$TARGET" '!'
VICTIM="${TEST_ROOT}/release-symlink-victim"
printf 'important-release-bytes\n' > "$VICTIM"
ln -s "$VICTIM" "${TARGET}/etc/dcentos/release-image"
provision_release "$TARGET" || fail_test "release rejected a safely replaceable marker symlink"
[ "$(cat "$VICTIM")" = important-release-bytes ] || fail_test "release marker symlink target changed"
[ ! -L "${TARGET}/etc/dcentos/release-image" ] || fail_test "release marker remained a symlink"

TARGET="${TEST_ROOT}/release-hardlink"
make_target "$TARGET" '*LOCKED*'
VICTIM="${TEST_ROOT}/release-hardlink-victim"
printf 'important-hardlink-bytes\n' > "$VICTIM"
ln "$VICTIM" "${TARGET}/etc/dcentos/release-image"
provision_release "$TARGET" || fail_test "release rejected a safely replaceable marker hardlink"
[ "$(cat "$VICTIM")" = important-hardlink-bytes ] || fail_test "release marker hardlink target changed"
[ "$(stat -c '%h' "$VICTIM")" = 1 ] || fail_test "release marker hardlink was retained"

TARGET="${TEST_ROOT}/grace-directory"
make_target "$TARGET" '*'
mkdir "${TARGET}/etc/dcentos/first-boot-grace"
printf 'preserve-grace-directory\n' > "${TARGET}/etc/dcentos/first-boot-grace/sentinel"
provision_release "$TARGET" || fail_test "release rejected a quarantinable grace directory"
[ ! -e "${TARGET}/etc/dcentos/first-boot-grace" ] || fail_test "grace directory retained its runtime name"
[ -f "${TARGET}/etc/dcentos/release-image" ] || fail_test "quarantined grace prevented release marker publication"
set -- "${TARGET}/etc/dcentos"/.first-boot-grace.rejected.*
[ "$#" -eq 1 ] && [ -d "$1" ] || fail_test "grace directory was not quarantined"
[ "$(cat "$1/sentinel")" = preserve-grace-directory ] || fail_test "grace quarantine lost contents"

TARGET="${TEST_ROOT}/marker-directory"
make_target "$TARGET" '*'
mkdir "${TARGET}/etc/dcentos/release-image"
printf 'preserve-marker-directory\n' > "${TARGET}/etc/dcentos/release-image/sentinel"
provision_release "$TARGET" || fail_test "release rejected a quarantinable marker directory"
[ -f "${TARGET}/etc/dcentos/release-image" ] || fail_test "release marker did not replace the retired directory name"
[ ! -L "${TARGET}/etc/dcentos/release-image" ] || fail_test "release marker replaced a directory with a symlink"
set -- "${TARGET}/etc/dcentos"/.release-image.rejected.*
[ "$#" -eq 1 ] && [ -d "$1" ] || fail_test "prior marker directory was not quarantined"
[ "$(cat "$1/sentinel")" = preserve-marker-directory ] || fail_test "marker quarantine lost contents"

TARGET="${TEST_ROOT}/config-symlink"
make_target "$TARGET" '*'
OUTSIDE="${TEST_ROOT}/outside-config"
mkdir "$OUTSIDE"
rmdir "${TARGET}/etc/dcentos"
ln -s "$OUTSIDE" "${TARGET}/etc/dcentos"
if provision_release "$TARGET"; then
    fail_test "release accepted a symlinked /etc/dcentos"
fi
[ ! -e "${OUTSIDE}/release-image" ] || fail_test "marker escaped through config symlink"

TARGET="${TEST_ROOT}/marker-write-failure"
make_target "$TARGET" '*'
cat() {
    return 1
}
if provision_release "$TARGET"; then
    fail_test "release ignored a marker write failure"
fi
unset -f cat
[ ! -e "${TARGET}/etc/dcentos/release-image" ] || fail_test "marker write failure left a marker"

TARGET="${TEST_ROOT}/marker-verification-failure"
make_target "$TARGET" '*'
stat() {
    for _test_stat_arg do
        case "$_test_stat_arg" in
            */etc/dcentos/release-image) return 1 ;;
        esac
    done
    command stat "$@"
}
if provision_release "$TARGET"; then
    fail_test "release ignored a published marker verification failure"
fi
unset -f stat
[ ! -e "${TARGET}/etc/dcentos/release-image" ] || fail_test "marker verification failure left a runtime claim"

TARGET="${TEST_ROOT}/shadow-publication-race"
make_target "$TARGET" '*'
mv() {
    _test_mv_last=
    for _test_mv_arg do
        _test_mv_last=$_test_mv_arg
    done
    command mv "$@" || return 1
    if [ "$_test_mv_last" = "${TARGET}/etc/dcentos/release-image" ]; then
        printf 'root:dcentral:20000:0:99999:7:::\n' > "${TARGET}/etc/shadow.replacement"
        printf 'daemon:*:20000:0:99999:7:::\n' >> "${TARGET}/etc/shadow.replacement"
        command mv -f "${TARGET}/etc/shadow.replacement" "${TARGET}/etc/shadow"
    fi
}
if provision_release "$TARGET"; then
    fail_test "release ignored /etc/shadow replacement during marker publication"
fi
unset -f mv
grep -F 'root:dcentral:' "${TARGET}/etc/shadow" >/dev/null || fail_test "shadow race injection did not run"
[ ! -e "${TARGET}/etc/dcentos/release-image" ] || fail_test "shadow replacement race left a runtime claim"

TARGET="${TEST_ROOT}/shadow-hardlink-open-race"
make_target "$TARGET" '*'
dcent_release_stat_signature() {
    if [ "$1" = "${TARGET}/etc/shadow" ] && [ ! -e "${TARGET}/hardlink-race-injected" ]; then
        cp "$1" "${TARGET}/etc/shadow.alias"
        rm "$1"
        ln "${TARGET}/etc/shadow.alias" "$1"
        : > "${TARGET}/hardlink-race-injected"
    fi
    command stat -c '%d:%i:%h:%s:%f:%y:%z' "$1" 2>/dev/null
}
if provision_release "$TARGET"; then
    fail_test "release accepted a hardlink introduced at the authoritative shadow open"
fi
[ -e "${TARGET}/hardlink-race-injected" ] || fail_test "shadow hardlink race injection did not run"
[ "$(stat -c '%h' "${TARGET}/etc/shadow")" = 2 ] || fail_test "shadow hardlink race did not produce two names"
[ ! -e "${TARGET}/etc/dcentos/release-image" ] || fail_test "shadow hardlink race left a runtime claim"

echo "release image provisioning tests passed"
