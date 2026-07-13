#!/bin/sh
#
# Static /data growth-bound audit.
#
# This test is host-only. It does not contact miners, mount images, write NAND,
# or inspect live devices. It pins the disk/RAM hygiene controls that keep a
# long-running beta unit from slowly filling writable storage:
#   - persistent /data/audit.log is byte-capped and trimmed atomically,
#   - generic API state publication has a finite payload ceiling,
#   - API audit-log reads are independently byte/page capped,
#   - the live audit ring is fixed-capacity,
#   - auth.json session persistence is capped before issuing a new session,
#   - diagnostic report pairs have bounded atomic publication and reads,
#   - overlay log rotation remains copytruncate + size bounded.

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

api_lib='dcentrald/dcentrald-api/src/lib.rs'
atomic_io='dcentrald/dcentrald-api/src/atomic_io.rs'
auth_rs='dcentrald/dcentrald-api/src/auth.rs'
audit_route='dcentrald/dcentrald-api/src/routes/audit_log.rs'
audit_types='dcentrald/dcentrald-api-types/src/audit_log.rs'
diagnostic_reports='dcentrald/dcentrald-diagnostics/src/report.rs'
logrotate='br2_external_dcentos/board/zynq/rootfs-overlay/etc/init.d/S43logrotate'

for f in "$api_lib" "$atomic_io" "$auth_rs" "$audit_route" "$audit_types" "$diagnostic_reports" "$logrotate"; do
    require_file "$f"
done

# Persistent audit file: bounded on disk and crash-tolerant during trim.
require_pattern "$api_lib" 'pub const DEFAULT_AUDIT_LOG_PATH: &str = "/data/audit.log";' \
    'audit log lives on persistent /data'
require_pattern "$api_lib" 'pub const DEFAULT_AUDIT_LOG_MAX_BYTES: u64 = 1_048_576;' \
    'audit log default cap is pinned to 1 MiB'
require_pattern "$api_lib" 'trim_audit_log_to_max_bytes(path, audit_log_max_bytes())' \
    'audit append path trims after every write'
require_pattern "$api_lib" 'crate::atomic_io::atomic_write_bytes(path, &bytes[start..])' \
    'audit trim writes atomically'
require_pattern "$atomic_io" 'pub const MAX_ATOMIC_WRITE_BYTES: usize = 8 * 1024 * 1024;' \
    'generic API state publication has an explicit 8 MiB ceiling'
require_pattern "$atomic_io" 'AtomicWriteOptions::state_file(MAX_ATOMIC_WRITE_BYTES)' \
    'generic API state publication uses the bounded common primitive'
require_pattern "$atomic_io" 'error.target_published()' \
    'post-rename durability failure remains visible to cache invalidation'

# API reader: a hand-edited env override cannot force unbounded read/response.
require_pattern "$audit_route" 'pub const MAX_AUDIT_LOG_READ_BYTES: u64 = 4 * 1_048_576;' \
    'persistent audit API read cap is pinned'
require_pattern "$audit_route" 'pub const MAX_AUDIT_LOG_PAGE: usize = 1000;' \
    'persistent audit API page cap is pinned'
require_pattern "$audit_route" '.clamp(1, MAX_AUDIT_LOG_PAGE)' \
    'persistent audit API clamps requested page size'
require_pattern "$audit_route" 'file.take(read_len).read_to_end(&mut bytes)?;' \
    'persistent audit API reads a bounded tail'

# Runtime ring: bounded memory, oldest entries evicted at capacity.
require_pattern "$audit_types" 'pub const DEFAULT_AUDIT_RING_CAPACITY: usize = 256;' \
    'audit ring default capacity is pinned'
require_pattern "$audit_types" 'VecDeque::with_capacity(capacity)' \
    'audit ring allocation uses fixed capacity'
require_pattern "$audit_types" 'if self.entries.len() == self.capacity {' \
    'audit ring detects full capacity before push'
require_pattern "$audit_types" 'self.entries.pop_front();' \
    'audit ring evicts oldest entry at capacity'

# Auth persistence: session churn cannot grow auth.json indefinitely.
require_pattern "$auth_rs" 'const MAX_AUTH_SESSIONS: usize = 32;' \
    'auth session store cap is pinned'
require_pattern "$auth_rs" 'fn compact_sessions_before_issue(auth: &mut AuthData)' \
    'auth sessions compact before issue helper exists'
require_pattern "$auth_rs" 'while auth.sessions.len() >= MAX_AUTH_SESSIONS' \
    'auth session compactor evicts at cap'
require_pattern "$auth_rs" 'compact_sessions_before_issue(auth);' \
    'auth issue path runs session compaction'
require_pattern "$auth_rs" 'issue_session_caps_active_records_and_keeps_new_session' \
    'auth session cap has a Rust regression test'

# Diagnostic artifacts: bounded pair, JSON commit marker, and durable cleanup.
require_pattern "$diagnostic_reports" 'pub const MAX_REPORT_JSON_BYTES: usize = 8 * 1024 * 1024;' \
    'diagnostic JSON artifacts have an explicit 8 MiB ceiling'
require_pattern "$diagnostic_reports" 'pub const MAX_REPORT_HTML_BYTES: usize = 8 * 1024 * 1024;' \
    'diagnostic HTML artifacts have an explicit 8 MiB ceiling'
require_pattern "$diagnostic_reports" 'atomic_file::atomic_write(path, bytes, AtomicWriteOptions::state_file(max_bytes))' \
    'diagnostic report publication uses the common atomic primitive'
require_pattern "$diagnostic_reports" 'serde_json::to_writer_pretty(&mut writer, value)' \
    'diagnostic JSON pretty serialization is bounded while bytes are produced'
require_pattern "$diagnostic_reports" 'report IDs are immutable' \
    'diagnostic report pairs cannot be crash-inconsistently overwritten in place'
require_pattern "$diagnostic_reports" 'self.cleanup_orphan_html_unlocked()?;' \
    'successful diagnostic publication reaps interrupted UUID-scoped HTML orphans'
require_pattern "$diagnostic_reports" 'options.custom_flags(libc::O_NOFOLLOW);' \
    'diagnostic report reads refuse final-component symlinks'
require_pattern "$diagnostic_reports" "// JSON is the pair's commit marker and is always published last." \
    'diagnostic JSON publication is the report-pair commit marker'
require_pattern "$diagnostic_reports" 'atomic_file::remove_file(path)' \
    'diagnostic retention and deletion publish directory-durable removal'

# Overlay logs: bounded copytruncate rotator remains present and conservative.
require_pattern "$logrotate" 'WATCH_LOGS="/tmp/dcentrald.log /tmp/dashboard.log /tmp/mcp.log"' \
    'overlay rotator watches daemon/dashboard/MCP logs'
require_pattern "$logrotate" 'MAX_BYTES=8388608' \
    'overlay rotator per-log cap is pinned'
require_pattern "$logrotate" 'cp "$_log" "${_log}.1"' \
    'overlay rotator keeps one bounded forensic generation'
require_pattern "$logrotate" ': > "$_log"' \
    'overlay rotator uses copytruncate on the live inode'
require_pattern "$logrotate" '/tmp/*|/data/*) : ;;' \
    'overlay rotator refuses paths outside writable storage'

if [ "$failures" -ne 0 ]; then
    printf '\n/data growth-bound audit failed: %s failure(s)\n' "$failures" >&2
    exit 1
fi

printf '\n/data growth-bound audit passed.\n'
