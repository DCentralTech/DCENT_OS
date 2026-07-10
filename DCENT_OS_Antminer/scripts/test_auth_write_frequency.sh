#!/bin/sh
#
# Static auth.json write-frequency audit.
#
# The bearer-token request path may read auth.json, but it must not write it on
# every authenticated request. Idle tracking is intentionally process-local so
# session activity does not churn NAND by fsyncing auth.json in the hot path.

set -eu

SCRIPT_DIR=$(CDPATH= cd "$(dirname "$0")" && pwd)
PROJECT_DIR=$(CDPATH= cd "$SCRIPT_DIR/.." && pwd)
cd "$PROJECT_DIR"

failures=0

pass() {
    printf 'PASS: %s\n' "$*"
}

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    failures=$((failures + 1))
}

require_file() {
    if [ -f "$1" ]; then
        pass "required file exists: $1"
    else
        fail "required file missing: $1"
    fi
}

require_pattern() {
    file=$1
    pattern=$2
    label=$3

    if [ ! -f "$file" ]; then
        fail "$label: missing file $file"
        return
    fi

    if grep -F -- "$pattern" "$file" >/dev/null 2>&1; then
        pass "$label"
    else
        fail "$label: missing pattern '$pattern' in $file"
    fi
}

function_body() {
    file=$1
    fn=$2

    awk -v fn="$fn" '
        BEGIN {
            in_fn = 0
            depth = 0
            seen_open = 0
        }
        {
            if (!in_fn && $0 ~ "^[[:space:]]*(pub(\\([^)]*\\))?[[:space:]]+)?(async[[:space:]]+)?fn[[:space:]]+" fn "[[:space:]]*\\(") {
                in_fn = 1
            }
            if (in_fn) {
                print
                open_line = $0
                close_line = $0
                opens = gsub(/\{/, "", open_line)
                closes = gsub(/\}/, "", close_line)
                depth += opens - closes
                if (opens > 0) {
                    seen_open = 1
                }
                if (seen_open && depth <= 0) {
                    exit
                }
            }
        }
    ' "$file"
}

reject_function_persistence() {
    file=$1
    fn=$2

    body=$(function_body "$file" "$fn")
    if [ -z "$body" ]; then
        fail "$fn: function body not found in $file"
        return
    fi

    if printf '%s\n' "$body" | grep -E '(save_auth|save_auth_at|atomic_write|std::fs::write|write_all|OpenOptions::new|File::create)' >/dev/null 2>&1; then
        fail "$fn: hot auth/session path contains auth.json persistence"
    else
        pass "$fn: no auth.json persistence in function body"
    fi
}

auth_rs='dcentrald/dcentrald-api/src/auth.rs'
rest_rs='dcentrald/dcentrald-api/src/rest.rs'
rest_late_rs='dcentrald/dcentrald-api/src/rest/late.rs'

require_file "$auth_rs"
require_file "$rest_rs"
require_file "$rest_late_rs"

require_pattern "$auth_rs" 'static SESSION_LAST_SEEN: LazyLock<Mutex<HashMap<String, Instant>>>' \
    'auth idle tracker is process-local'
require_pattern "$auth_rs" 'persisted so the hot per-request auth path never fsyncs `auth.json`' \
    'auth idle-tracker comment documents the NAND-wear contract'
require_pattern "$auth_rs" 'map.insert(token_hash.to_string(), now);' \
    'auth idle touch records activity in memory'
require_pattern "$auth_rs" 'pub(crate) fn save_auth_at(path: &std::path::Path, auth: &AuthData)' \
    'auth persistence remains centralized behind save_auth_at'
require_pattern "$auth_rs" 'atomic_write(path, json.as_bytes())?;' \
    'auth persistence uses atomic_io only at the save boundary'

for fn in \
    session_idle_ok_and_touch_at \
    session_matches_token \
    session_for_token \
    role_for_token \
    check_auth \
    current_session_id \
    issue_session_with_role \
    revoke_session
do
    reject_function_persistence "$auth_rs" "$fn"
done

save_count=$(grep -F 'crate::auth::save_auth(&auth_data)' "$rest_late_rs" | wc -l | tr -d ' ')
if [ "$save_count" = "3" ]; then
    pass 'REST auth persistence is limited to setup, login/session issue, and revoke'
else
    fail "expected exactly 3 REST auth save sites; found $save_count"
fi

require_pattern "$rest_late_rs" 'async fn post_auth_setup(' \
    'auth setup route exists as an explicit persistence transition'
require_pattern "$rest_late_rs" 'async fn post_auth_session(' \
    'auth session-issue route exists as an explicit persistence transition'
require_pattern "$rest_late_rs" 'async fn delete_auth_session_current(headers: HeaderMap)' \
    'auth session-revoke route exists as an explicit persistence transition'
require_pattern "$auth_rs" 'let mut dirty = false;' \
    'legacy auth migration uses an explicit dirty flag'
require_pattern "$auth_rs" 'if auth.version < 2 {' \
    'legacy auth version migration is an explicit load-time write trigger'
require_pattern "$auth_rs" 'if let Some(legacy_token) = auth.api_token.take() {' \
    'legacy auth token migration is an explicit load-time write trigger'
require_pattern "$auth_rs" 'if dirty {' \
    'legacy auth migration only saves after a dirty schema/token upgrade'
require_pattern "$auth_rs" 'let _ = save_auth_at(path, &auth);' \
    'legacy auth migration save remains explicit'

if [ "$failures" -ne 0 ]; then
    printf '\nauth.json write-frequency audit failed: %s failure(s)\n' "$failures" >&2
    exit 1
fi

printf '\nauth.json write-frequency audit passed.\n'
