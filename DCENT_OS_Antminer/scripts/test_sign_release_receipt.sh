#!/bin/sh
# Adversarial host-side tests for exact release-receipt signing.
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
SIGNER="$SCRIPT_DIR/sign_release_receipt.sh"
TEST_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/dcentos-receipt-signing.XXXXXX")
cleanup() {
    chmod -R u+w "$TEST_ROOT" 2>/dev/null || true
    rm -rf "$TEST_ROOT"
}
trap cleanup EXIT HUP INT TERM

fail_test() {
    echo "release receipt signing test failed: $*" >&2
    exit 1
}

command -v openssl >/dev/null 2>&1 || fail_test "openssl is unavailable"
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
printf '{"schema":"dcentos.test-receipt.v1"}\n' > "$TEST_ROOT/receipt.json"
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
fi

"$SIGNER" "$TEST_ROOT/receipt.json" "$TEST_ROOT/private.pem" \
    "$TEST_ROOT/public.pem" "$TEST_ROOT/receipt.json.sig" >/dev/null \
    || fail_test "valid receipt signing failed"
[ "$(wc -c < "$TEST_ROOT/receipt.json.sig" | tr -d '[:space:]')" = 64 ] \
    || fail_test "signature length is not 64 bytes"
openssl pkeyutl -verify -rawin -pubin -inkey "$TEST_ROOT/public.pem" \
    -sigfile "$TEST_ROOT/receipt.json.sig" -in "$TEST_ROOT/receipt.json" \
    >/dev/null || fail_test "published signature does not verify"
"$SIGNER" "$TEST_ROOT/receipt.json" "$TEST_ROOT/private.pem" \
    "$TEST_ROOT/public.pem" "$TEST_ROOT/receipt.json.sig" >/dev/null \
    || fail_test "exact existing signature was not idempotently reconciled"

"$SIGNER" "$TEST_ROOT/receipt.json" "$TEST_ROOT/private.pem" \
    "$TEST_ROOT/public.pem" "$TEST_ROOT/receipt.second.sig" >/dev/null \
    || fail_test "deterministic second signing failed"
cmp "$TEST_ROOT/receipt.json.sig" "$TEST_ROOT/receipt.second.sig" \
    || fail_test "Ed25519 signing was not deterministic"

if ln -s "receipt.json" "$TEST_ROOT/receipt-link.json" 2>/dev/null &&
    "$PYTHON" - "$TEST_ROOT/receipt-link.json" <<'PY'
from pathlib import Path
import stat
import sys

path = Path(sys.argv[1])
metadata = path.lstat()
is_reparse = bool(
    getattr(metadata, "st_file_attributes", 0)
    & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
)
raise SystemExit(0 if path.is_symlink() or is_reparse else 1)
PY
then
    if "$SIGNER" "$TEST_ROOT/receipt-link.json" "$TEST_ROOT/private.pem" \
        "$TEST_ROOT/public.pem" "$TEST_ROOT/symlink.sig" >/dev/null 2>&1; then
        fail_test "symlinked receipt was accepted"
    fi
    [ ! -e "$TEST_ROOT/symlink.sig" ] \
        || fail_test "symlink rejection left output"
fi

cp "$TEST_ROOT/receipt.json" "$TEST_ROOT/hardlinked-receipt.json"
if ln "$TEST_ROOT/hardlinked-receipt.json" \
    "$TEST_ROOT/receipt-alias.json" 2>/dev/null &&
    "$PYTHON" - "$TEST_ROOT/hardlinked-receipt.json" <<'PY'
from pathlib import Path
import sys

raise SystemExit(0 if Path(sys.argv[1]).stat().st_nlink > 1 else 1)
PY
then
    if "$SIGNER" "$TEST_ROOT/hardlinked-receipt.json" "$TEST_ROOT/private.pem" \
        "$TEST_ROOT/public.pem" "$TEST_ROOT/hardlink.sig" >/dev/null 2>&1; then
        fail_test "multiply-linked receipt was accepted"
    fi
    [ ! -e "$TEST_ROOT/hardlink.sig" ] \
        || fail_test "hardlink rejection left output"
fi

if "$SIGNER" "$TEST_ROOT/receipt.json" "$TEST_ROOT/private.pem" \
    "$TEST_ROOT/wrong-public.pem" "$TEST_ROOT/wrong-key.sig" >/dev/null 2>&1; then
    fail_test "mismatched trusted public key was accepted"
fi
[ ! -e "$TEST_ROOT/wrong-key.sig" ] || fail_test "wrong-key failure left output"

printf 'preserve-existing-signature\n' > "$TEST_ROOT/existing.sig"
if "$SIGNER" "$TEST_ROOT/receipt.json" "$TEST_ROOT/private.pem" \
    "$TEST_ROOT/public.pem" "$TEST_ROOT/existing.sig" >/dev/null 2>&1; then
    fail_test "existing signature output was overwritten"
fi
[ "$(cat "$TEST_ROOT/existing.sig")" = preserve-existing-signature ] \
    || fail_test "existing signature output changed"

if [ "$("$PYTHON" -c 'import os; print(os.name)')" = posix ]; then
    chmod 644 "$TEST_ROOT/private.pem"
    if "$SIGNER" "$TEST_ROOT/receipt.json" "$TEST_ROOT/private.pem" \
        "$TEST_ROOT/public.pem" "$TEST_ROOT/unsafe-key.sig" >/dev/null 2>&1; then
        fail_test "group/world-readable private key was accepted"
    fi
    [ ! -e "$TEST_ROOT/unsafe-key.sig" ] || fail_test "unsafe key left output"
    chmod 600 "$TEST_ROOT/private.pem"
fi

"$PYTHON" - "$SCRIPT_DIR/sign_release_receipt.py" "$TEST_ROOT" <<'PY' \
    || fail_test "pinned-input race regression failed"
import importlib.util
import errno
import os
from argparse import Namespace
from pathlib import Path
import signal
import sys

module_path = Path(sys.argv[1])
root = Path(sys.argv[2])
spec = importlib.util.spec_from_file_location("dcent_receipt_signing_test", module_path)
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)

callback_error_output = root / "callback-error.sig"

def raise_noncollision_file_exists():
    raise FileExistsError(errno.EEXIST, "injected callback failure")

try:
    module.release_io.publish_regular_file_noreplace(
        callback_error_output,
        b"x" * 64,
        mode=0o644,
        before_commit=raise_noncollision_file_exists,
    )
except module.release_io.PublicationCollision as error:
    raise SystemExit(
        "callback FileExistsError was misclassified as publication collision"
    ) from error
except module.release_io.ReleaseSetError:
    pass
else:
    raise SystemExit("callback FileExistsError was accepted")
if callback_error_output.exists():
    raise SystemExit("callback failure published an output")

preexisting_output = root / "preexisting-exact.sig"
module.release_io.publish_regular_file_noreplace(
    preexisting_output,
    (root / "receipt.json.sig").read_bytes(),
    mode=0o644,
)
original_publish = module.release_io.publish_regular_file_noreplace
original_fsync_directory = module.release_io.fsync_directory
directory_syncs = 0

def reject_retry_staging(*_args, **_kwargs):
    raise AssertionError("exact-output retry attempted destination staging")

def record_directory_sync(path):
    global directory_syncs
    directory_syncs += 1
    return original_fsync_directory(path)

module.release_io.publish_regular_file_noreplace = reject_retry_staging
module.release_io.fsync_directory = record_directory_sync
try:
    module.sign_receipt(
        Namespace(
            receipt=str(root / "receipt.json"),
            private_key=str(root / "private.pem"),
            public_key=str(root / "public.pem"),
            signature=str(preexisting_output),
        )
    )
finally:
    module.release_io.publish_regular_file_noreplace = original_publish
    module.release_io.fsync_directory = original_fsync_directory
if directory_syncs == 0:
    raise SystemExit("exact-output reconciliation did not establish durability")

raced_output = root / "raced-output.sig"
original_run = module.run_openssl
calls = 0

def run_then_claim_output(*args, **kwargs):
    global calls
    original_run(*args, **kwargs)
    calls += 1
    if calls == 1:
        raced_output.write_bytes(b"competing publisher\n")

module.run_openssl = run_then_claim_output
try:
    try:
        module.sign_receipt(
            Namespace(
                receipt=str(root / "receipt.json"),
                private_key=str(root / "private.pem"),
                public_key=str(root / "public.pem"),
                signature=str(raced_output),
            )
        )
    except module.SigningError:
        pass
    else:
        raise SystemExit("destination creation race was accepted")
finally:
    module.run_openssl = original_run
if raced_output.read_bytes() != b"competing publisher\n":
    raise SystemExit("destination creation race replaced competing output")
if calls == 0:
    raise SystemExit("destination creation race injection did not run")

concurrent_output = root / "concurrent-exact.sig"
original_publish = module.release_io.publish_regular_file_noreplace
publish_calls = 0

def publish_competitor_then_report_collision(output, content, **_kwargs):
    global publish_calls
    original_publish(output, content, mode=0o644)
    publish_calls += 1
    raise module.release_io.PublicationCollision(
        "injected simultaneous exact publication"
    )

module.release_io.publish_regular_file_noreplace = (
    publish_competitor_then_report_collision
)
try:
    module.sign_receipt(
        Namespace(
            receipt=str(root / "receipt.json"),
            private_key=str(root / "private.pem"),
            public_key=str(root / "public.pem"),
            signature=str(concurrent_output),
        )
    )
finally:
    module.release_io.publish_regular_file_noreplace = original_publish
if publish_calls != 1 or not concurrent_output.is_file():
    raise SystemExit("simultaneous exact publication did not converge")

terminated_output = root / "terminated-before-commit.sig"
original_run = module.run_openssl
calls = 0

def run_then_terminate(*args, **kwargs):
    global calls
    original_run(*args, **kwargs)
    calls += 1
    if calls == 1:
        signal.raise_signal(signal.SIGTERM)

module.run_openssl = run_then_terminate
try:
    try:
        module.sign_receipt(
            Namespace(
                receipt=str(root / "receipt.json"),
                private_key=str(root / "private.pem"),
                public_key=str(root / "public.pem"),
                signature=str(terminated_output),
            )
        )
    except module.SigningSignal:
        pass
    else:
        raise SystemExit("pre-commit termination request was ignored")
finally:
    module.run_openssl = original_run
if terminated_output.exists():
    raise SystemExit("pre-commit termination published a signature")
if calls == 0:
    raise SystemExit("pre-commit termination injection did not run")

boundary_signal_output = root / "boundary-signal.sig"
original_publish = module.release_io.publish_regular_file_noreplace
publish_calls = 0

def signal_before_publication(*args, **kwargs):
    global publish_calls
    publish_calls += 1
    signal.raise_signal(signal.SIGTERM)
    return original_publish(*args, **kwargs)

module.release_io.publish_regular_file_noreplace = signal_before_publication
try:
    try:
        module.sign_receipt(
            Namespace(
                receipt=str(root / "receipt.json"),
                private_key=str(root / "private.pem"),
                public_key=str(root / "public.pem"),
                signature=str(boundary_signal_output),
            )
        )
    except module.SigningSignal:
        pass
    else:
        raise SystemExit("signal immediately before publication was ignored")
finally:
    module.release_io.publish_regular_file_noreplace = original_publish
if publish_calls != 1 or boundary_signal_output.exists():
    raise SystemExit("pre-publication signal crossed the commit boundary")

verification_fault_output = root / "verification-fault.sig"
original_reconcile = module.reconcile_published_signature
reconcile_calls = 0

def fail_first_published_verification(*args, **kwargs):
    global reconcile_calls
    reconcile_calls += 1
    if reconcile_calls == 1:
        raise module.SigningError("injected post-commit verification fault")
    return original_reconcile(*args, **kwargs)

module.reconcile_published_signature = fail_first_published_verification
try:
    try:
        module.sign_receipt(
            Namespace(
                receipt=str(root / "receipt.json"),
                private_key=str(root / "private.pem"),
                public_key=str(root / "public.pem"),
                signature=str(verification_fault_output),
            )
        )
    except module.SigningError:
        pass
    else:
        raise SystemExit("injected post-commit verification fault did not fail")
finally:
    module.reconcile_published_signature = original_reconcile
if not verification_fault_output.is_file():
    raise SystemExit("post-commit verification fault lost the durable output")
module.sign_receipt(
    Namespace(
        receipt=str(root / "receipt.json"),
        private_key=str(root / "private.pem"),
        public_key=str(root / "public.pem"),
        signature=str(verification_fault_output),
    )
)

publication_fault_output = root / "publication-fault.sig"
original_publish = module.release_io.publish_regular_file_noreplace
publish_calls = 0

def publish_exact_then_report_failure(*args, **kwargs):
    global publish_calls
    original_publish(*args, **kwargs)
    publish_calls += 1
    raise module.release_io.ReleaseSetError(
        "injected post-publication durability/quarantine failure"
    )

module.release_io.publish_regular_file_noreplace = publish_exact_then_report_failure
try:
    try:
        module.sign_receipt(
            Namespace(
                receipt=str(root / "receipt.json"),
                private_key=str(root / "private.pem"),
                public_key=str(root / "public.pem"),
                signature=str(publication_fault_output),
            )
        )
    except module.release_io.ReleaseSetError:
        pass
    else:
        raise SystemExit("publication helper failure was converted to success")
finally:
    module.release_io.publish_regular_file_noreplace = original_publish
if publish_calls != 1 or not publication_fault_output.is_file():
    raise SystemExit("publication failure injection did not cross the output boundary")
module.sign_receipt(
    Namespace(
        receipt=str(root / "receipt.json"),
        private_key=str(root / "private.pem"),
        public_key=str(root / "public.pem"),
        signature=str(publication_fault_output),
    )
)

cleanup_fault_output = root / "cleanup-fault.sig"
original_retire = module.retire_signing_control
retained_controls = []

def fail_control_retirement(control, pending):
    if pending.exists() or pending.is_symlink():
        pending.unlink()
    retained_controls.append(control)
    raise OSError("injected control-directory retirement fault")

module.retire_signing_control = fail_control_retirement
try:
    module.sign_receipt(
        Namespace(
            receipt=str(root / "receipt.json"),
            private_key=str(root / "private.pem"),
            public_key=str(root / "public.pem"),
            signature=str(cleanup_fault_output),
        )
    )
finally:
    module.retire_signing_control = original_retire
    for retained in retained_controls:
        retained.rmdir()
if not cleanup_fault_output.is_file():
    raise SystemExit("post-commit cleanup fault lost the durable output")
module.sign_receipt(
    Namespace(
        receipt=str(root / "receipt.json"),
        private_key=str(root / "private.pem"),
        public_key=str(root / "public.pem"),
        signature=str(cleanup_fault_output),
    )
)

postcommit_signal_output = root / "postcommit-signal.sig"
original_publish = module.release_io.publish_regular_file_noreplace
publish_calls = 0

def publish_then_terminate(*args, **kwargs):
    global publish_calls
    original_publish(*args, **kwargs)
    publish_calls += 1
    signal.raise_signal(signal.SIGTERM)

module.release_io.publish_regular_file_noreplace = publish_then_terminate
try:
    module.sign_receipt(
        Namespace(
            receipt=str(root / "receipt.json"),
            private_key=str(root / "private.pem"),
            public_key=str(root / "public.pem"),
            signature=str(postcommit_signal_output),
        )
    )
finally:
    module.release_io.publish_regular_file_noreplace = original_publish
if publish_calls != 1 or not postcommit_signal_output.is_file():
    raise SystemExit("post-commit signal did not preserve committed success")
module.sign_receipt(
    Namespace(
        receipt=str(root / "receipt.json"),
        private_key=str(root / "private.pem"),
        public_key=str(root / "public.pem"),
        signature=str(postcommit_signal_output),
    )
)

if os.name == "posix":
    for mode in (0o600, 0o666):
        noncanonical = root / f"noncanonical-{mode:04o}.sig"
        noncanonical.write_bytes((root / "receipt.json.sig").read_bytes())
        noncanonical.chmod(mode)
        try:
            module.sign_receipt(
                Namespace(
                    receipt=str(root / "receipt.json"),
                    private_key=str(root / "private.pem"),
                    public_key=str(root / "public.pem"),
                    signature=str(noncanonical),
                )
            )
        except module.SigningError:
            pass
        else:
            raise SystemExit(f"noncanonical signature mode {mode:04o} was accepted")
        if noncanonical.stat().st_mode & 0o777 != mode:
            raise SystemExit("noncanonical signature mode was mutated")

    raced_receipt = root / "raced-receipt.json"
    raced_receipt.write_bytes((root / "receipt.json").read_bytes())
    output = root / "raced.sig"
    original_run = module.run_openssl
    calls = 0

    def run_then_replace(*args, **kwargs):
        global calls
        original_run(*args, **kwargs)
        calls += 1
        if calls == 1:
            replacement = root / "raced-receipt.replacement"
            replacement.write_bytes(b'{"changed":true}\n')
            replacement.replace(raced_receipt)

    module.run_openssl = run_then_replace
    try:
        try:
            module.sign_receipt(
                Namespace(
                    receipt=str(raced_receipt),
                    private_key=str(root / "private.pem"),
                    public_key=str(root / "public.pem"),
                    signature=str(output),
                )
            )
        except module.SigningError:
            pass
        else:
            raise SystemExit("replaced receipt path was accepted")
    finally:
        module.run_openssl = original_run
    if output.exists():
        raise SystemExit("receipt replacement race left a signature")
    if calls == 0:
        raise SystemExit("receipt replacement injection did not run")
else:
    private_signature = root / "windows-private-signature.sig"
    private_signature.write_bytes((root / "receipt.json.sig").read_bytes())
    module.release_io.set_windows_file_acl(
        private_signature, module.release_io.WINDOWS_PRIVATE_FILE_SDDL
    )
    try:
        module.sign_receipt(
            Namespace(
                receipt=str(root / "receipt.json"),
                private_key=str(root / "private.pem"),
                public_key=str(root / "public.pem"),
                signature=str(private_signature),
            )
        )
    except module.SigningError:
        pass
    else:
        raise SystemExit("private Windows signature ACL was accepted as public")

    overbroad_signature = root / "windows-overbroad-signature.sig"
    overbroad_signature.write_bytes((root / "receipt.json.sig").read_bytes())
    module.release_io.set_windows_file_acl(
        overbroad_signature,
        "D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;OW)(A;;FA;;;WD)",
    )
    try:
        module.sign_receipt(
            Namespace(
                receipt=str(root / "receipt.json"),
                private_key=str(root / "private.pem"),
                public_key=str(root / "public.pem"),
                signature=str(overbroad_signature),
            )
        )
    except module.SigningError:
        pass
    else:
        raise SystemExit("overbroad Windows signature ACL was accepted")

    unsafe_key = root / "windows-public-private.pem"
    unsafe_key.write_bytes((root / "private.pem").read_bytes())
    module.release_io.set_windows_file_acl(
        unsafe_key, module.release_io.WINDOWS_PUBLIC_FILE_SDDL
    )
    try:
        module.sign_receipt(
            Namespace(
                receipt=str(root / "receipt.json"),
                private_key=str(unsafe_key),
                public_key=str(root / "public.pem"),
                signature=str(root / "windows-public-key.sig"),
            )
        )
    except module.SigningError:
        pass
    else:
        raise SystemExit("Windows private key with public-read ACL was accepted")
    if (root / "windows-public-key.sig").exists():
        raise SystemExit("Windows unsafe-key rejection left a signature")

    raced_key = root / "windows-raced-private.pem"
    raced_key.write_bytes((root / "private.pem").read_bytes())
    module.release_io.set_windows_file_acl(
        raced_key, module.release_io.WINDOWS_PRIVATE_FILE_SDDL
    )
    raced_key_output = root / "windows-raced-key.sig"
    original_run = module.run_openssl
    calls = 0
    acl_change_denied = False

    def run_then_expose_key(*args, **kwargs):
        global acl_change_denied, calls
        original_run(*args, **kwargs)
        calls += 1
        if calls == 1:
            try:
                module.release_io.set_windows_file_acl(
                    raced_key, module.release_io.WINDOWS_PUBLIC_FILE_SDDL
                )
            except PermissionError:
                acl_change_denied = True

    module.run_openssl = run_then_expose_key
    signing_failed = False
    try:
        try:
            module.sign_receipt(
                Namespace(
                    receipt=str(root / "receipt.json"),
                    private_key=str(raced_key),
                    public_key=str(root / "public.pem"),
                    signature=str(raced_key_output),
                )
            )
        except module.SigningError:
            signing_failed = True
    finally:
        module.run_openssl = original_run
    if calls == 0:
        raise SystemExit("Windows private-key ACL race injection did not run")
    if acl_change_denied:
        if signing_failed or not raced_key_output.is_file():
            raise SystemExit("Windows ACL pin denial prevented valid publication")
    elif not signing_failed or raced_key_output.exists():
        raise SystemExit("Windows private-key ACL change was accepted")

    pinned_receipt = root / "windows-pinned-receipt.json"
    pinned_receipt.write_bytes((root / "receipt.json").read_bytes())
    output = root / "windows-pinned.sig"
    original_run = module.run_openssl
    write_was_denied = False
    calls = 0

    def run_then_attempt_write(*args, **kwargs):
        global calls, write_was_denied
        original_run(*args, **kwargs)
        calls += 1
        if calls == 1:
            try:
                pinned_receipt.write_bytes(b'{"changed":true}\n')
            except PermissionError:
                write_was_denied = True

    module.run_openssl = run_then_attempt_write
    try:
        module.sign_receipt(
            Namespace(
                receipt=str(pinned_receipt),
                private_key=str(root / "private.pem"),
                public_key=str(root / "public.pem"),
                signature=str(output),
            )
        )
    finally:
        module.run_openssl = original_run
    if not write_was_denied:
        raise SystemExit("Windows input pin did not deny a competing writer")
    if not output.is_file():
        raise SystemExit("Windows exclusive-pin signing did not publish")
PY

mkdir -p "$TEST_ROOT/crash-scratch"
set +e
TMPDIR="$TEST_ROOT/crash-scratch" \
    TMP="$TEST_ROOT/crash-scratch" \
    TEMP="$TEST_ROOT/crash-scratch" \
    "$PYTHON" - "$SCRIPT_DIR/sign_release_receipt.py" "$TEST_ROOT" <<'PY'
import importlib.util
import os
from argparse import Namespace
from pathlib import Path
import sys

module_path = Path(sys.argv[1])
root = Path(sys.argv[2])
spec = importlib.util.spec_from_file_location("dcent_receipt_crash_test", module_path)
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)
original_run = module.run_openssl

def run_then_crash(*args, **kwargs):
    original_run(*args, **kwargs)
    os._exit(91)

module.run_openssl = run_then_crash
module.sign_receipt(
    Namespace(
        receipt=str(root / "receipt.json"),
        private_key=str(root / "private.pem"),
        public_key=str(root / "public.pem"),
        signature=str(root / "crash.sig"),
    )
)
PY
crash_rc=$?
set -e
[ "$crash_rc" -eq 91 ] || fail_test "hard-crash injection did not execute"
[ ! -e "$TEST_ROOT/crash.sig" ] \
    || fail_test "pre-publication hard crash left an official signature"
[ -z "$(find "$TEST_ROOT" -maxdepth 1 -name '.*.signing-*' -print -quit)" ] \
    || fail_test "hard crash stranded signing control in the artifact parent"
[ -n "$(find "$TEST_ROOT/crash-scratch" -mindepth 1 -maxdepth 1 \
    -type d -name 'dcentos-receipt-signing-*' -print -quit)" ] \
    || fail_test "hard-crash scratch was not isolated in the external temp root"

set -- "$TEST_ROOT"/.*.signing-* "$TEST_ROOT"/.*.publication-pending.*
for leaked in "$@"; do
    [ ! -e "$leaked" ] || fail_test "private signing control leaked: $leaked"
done

echo "release receipt signing tests passed"
