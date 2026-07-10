#!/bin/sh
#
# Offline resource-soak harness.
#
# Host-only: this script launches or monitors only local test processes. It
# never contacts miners, opens SSH, flashes media, polls device IPs, or touches
# live hardware. The default path runs a tiny local worker and compresses a
# logical 4-hour soak into a few host samples, then fails if RSS or fd count
# grows beyond the configured bounds.

set -eu

SCRIPT_DIR=$(CDPATH= cd "$(dirname "$0")" && pwd)

MODE=run
SERIES_FILE=
OUTPUT_FILE=
SAMPLES=4
SAMPLE_INTERVAL=1
LOGICAL_HOURS=4
MAX_RSS_GROWTH_KB=8192
MAX_FD_GROWTH=4
COMMAND=
PID=
TMP_DIR=
KEEP_OUTPUT=0

usage() {
    cat <<'USAGE'
Usage:
  offline_soak_harness.sh [options] [-- command]
  offline_soak_harness.sh --series path.csv [options]
  offline_soak_harness.sh --self-test

Options:
  --samples N              Number of samples for process mode (default: 4)
  --sample-interval N      Seconds between samples (default: 1)
  --logical-hours N        Logical soak duration represented by samples (default: 4)
  --max-rss-growth-kb N    Allowed max(RSS)-min(RSS), KiB (default: 8192)
  --max-fd-growth N        Allowed max(fd)-min(fd) (default: 4)
  --output path.csv        Write sampled CSV to path

CSV format:
  sample,logical_seconds,rss_kb,fd_count
USAGE
}

die() {
    printf 'ERROR: %s\n' "$*" >&2
    exit 2
}

is_uint() {
    case "$1" in
        ''|*[!0-9]*) return 1 ;;
        *) return 0 ;;
    esac
}

require_uint() {
    _name=$1
    _value=$2
    if ! is_uint "$_value"; then
        die "$_name must be a non-negative integer, got '$_value'"
    fi
}

cleanup() {
    if [ -n "$PID" ] && kill -0 "$PID" >/dev/null 2>&1; then
        kill "$PID" >/dev/null 2>&1 || true
        wait "$PID" >/dev/null 2>&1 || true
    fi
    if [ -n "$TMP_DIR" ] && [ -d "$TMP_DIR" ]; then
        rm -rf "$TMP_DIR"
    fi
}

trap cleanup EXIT INT TERM

parse_args() {
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --self-test)
                MODE=self_test
                ;;
            --series)
                shift
                [ "$#" -gt 0 ] || die "--series requires a path"
                MODE=series
                SERIES_FILE=$1
                ;;
            --samples)
                shift
                [ "$#" -gt 0 ] || die "--samples requires a value"
                SAMPLES=$1
                ;;
            --sample-interval)
                shift
                [ "$#" -gt 0 ] || die "--sample-interval requires a value"
                SAMPLE_INTERVAL=$1
                ;;
            --logical-hours)
                shift
                [ "$#" -gt 0 ] || die "--logical-hours requires a value"
                LOGICAL_HOURS=$1
                ;;
            --max-rss-growth-kb)
                shift
                [ "$#" -gt 0 ] || die "--max-rss-growth-kb requires a value"
                MAX_RSS_GROWTH_KB=$1
                ;;
            --max-fd-growth)
                shift
                [ "$#" -gt 0 ] || die "--max-fd-growth requires a value"
                MAX_FD_GROWTH=$1
                ;;
            --output)
                shift
                [ "$#" -gt 0 ] || die "--output requires a path"
                OUTPUT_FILE=$1
                KEEP_OUTPUT=1
                ;;
            -h|--help)
                usage
                exit 0
                ;;
            --)
                shift
                [ "$#" -gt 0 ] || die "-- requires a command"
                COMMAND=$*
                break
                ;;
            *)
                die "unknown argument: $1"
                ;;
        esac
        shift
    done
}

validate_options() {
    require_uint "samples" "$SAMPLES"
    require_uint "sample interval" "$SAMPLE_INTERVAL"
    require_uint "logical hours" "$LOGICAL_HOURS"
    require_uint "max RSS growth" "$MAX_RSS_GROWTH_KB"
    require_uint "max fd growth" "$MAX_FD_GROWTH"
    [ "$SAMPLES" -ge 2 ] || die "--samples must be at least 2"
    [ "$SAMPLE_INTERVAL" -ge 1 ] || die "--sample-interval must be at least 1"
}

read_rss_kb() {
    _pid=$1
    _status="/proc/$_pid/status"
    [ -r "$_status" ] || return 1
    _rss=$(sed -n 's/^VmRSS:[	 ][	 ]*\([0-9][0-9]*\).*/\1/p' "$_status")
    [ -n "$_rss" ] || _rss=0
    printf '%s\n' "$_rss"
}

count_fds() {
    _pid=$1
    _fd_dir="/proc/$_pid/fd"
    [ -d "$_fd_dir" ] || return 1
    ls -1 "$_fd_dir" 2>/dev/null | wc -l | tr -d ' '
}

validate_series() {
    _file=$1
    [ -f "$_file" ] || die "series file missing: $_file"

    _samples=0
    _min_rss=
    _max_rss=
    _min_fd=
    _max_fd=

    while IFS=, read -r _sample _logical _rss _fd _rest; do
        case "$_sample" in
            ''|'#'*) continue ;;
            sample) continue ;;
        esac
        if ! is_uint "$_rss" || ! is_uint "$_fd"; then
            printf 'FAIL: malformed resource sample in %s: %s,%s,%s,%s\n' \
                "$_file" "$_sample" "$_logical" "$_rss" "$_fd" >&2
            return 1
        fi
        if [ "$_samples" -eq 0 ]; then
            _min_rss=$_rss
            _max_rss=$_rss
            _min_fd=$_fd
            _max_fd=$_fd
        else
            [ "$_rss" -lt "$_min_rss" ] && _min_rss=$_rss
            [ "$_rss" -gt "$_max_rss" ] && _max_rss=$_rss
            [ "$_fd" -lt "$_min_fd" ] && _min_fd=$_fd
            [ "$_fd" -gt "$_max_fd" ] && _max_fd=$_fd
        fi
        _samples=$((_samples + 1))
    done < "$_file"

    if [ "$_samples" -lt 2 ]; then
        printf 'FAIL: offline soak needs at least 2 samples (%s had %s)\n' \
            "$_file" "$_samples" >&2
        return 1
    fi

    _rss_growth=$((_max_rss - _min_rss))
    _fd_growth=$((_max_fd - _min_fd))

    if [ "$_rss_growth" -gt "$MAX_RSS_GROWTH_KB" ]; then
        printf 'FAIL: RSS growth %s KiB exceeds limit %s KiB (%s)\n' \
            "$_rss_growth" "$MAX_RSS_GROWTH_KB" "$_file" >&2
        return 1
    fi
    if [ "$_fd_growth" -gt "$MAX_FD_GROWTH" ]; then
        printf 'FAIL: fd growth %s exceeds limit %s (%s)\n' \
            "$_fd_growth" "$MAX_FD_GROWTH" "$_file" >&2
        return 1
    fi

    printf 'PASS: offline soak bounds rss_growth_kb=%s fd_growth=%s samples=%s\n' \
        "$_rss_growth" "$_fd_growth" "$_samples"
    return 0
}

start_worker() {
    TMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/dcent-offline-soak.XXXXXX")
    _flag="$TMP_DIR/keep-running"
    : > "$_flag"
    if [ -n "$COMMAND" ]; then
        sh -c "$COMMAND" &
    else
        sh -c 'while [ -f "$1" ]; do sleep 1; done' sh "$_flag" &
    fi
    PID=$!
}

sample_process() {
    [ -d /proc ] || die "/proc is required for RSS/fd sampling"

    if [ -z "$OUTPUT_FILE" ]; then
        if [ -z "$TMP_DIR" ]; then
            TMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/dcent-offline-soak.XXXXXX")
        fi
        OUTPUT_FILE="$TMP_DIR/resource-samples.csv"
    fi

    printf 'sample,logical_seconds,rss_kb,fd_count\n' > "$OUTPUT_FILE"
    _logical_total=$((LOGICAL_HOURS * 3600))
    _last_index=$((SAMPLES - 1))
    _i=0
    while [ "$_i" -lt "$SAMPLES" ]; do
        if ! kill -0 "$PID" >/dev/null 2>&1; then
            printf 'FAIL: monitored process exited before sample %s\n' "$_i" >&2
            return 1
        fi
        _rss=$(read_rss_kb "$PID") || {
            printf 'FAIL: cannot read RSS for pid %s\n' "$PID" >&2
            return 1
        }
        _fds=$(count_fds "$PID") || {
            printf 'FAIL: cannot count fds for pid %s\n' "$PID" >&2
            return 1
        }
        _logical=$((_i * _logical_total / _last_index))
        printf '%s,%s,%s,%s\n' "$_i" "$_logical" "$_rss" "$_fds" >> "$OUTPUT_FILE"
        _i=$((_i + 1))
        if [ "$_i" -lt "$SAMPLES" ]; then
            sleep "$SAMPLE_INTERVAL"
        fi
    done

    validate_series "$OUTPUT_FILE"
}

write_series() {
    _path=$1
    shift
    {
        printf 'sample,logical_seconds,rss_kb,fd_count\n'
        while [ "$#" -gt 0 ]; do
            printf '%s\n' "$1"
            shift
        done
    } > "$_path"
}

self_test() {
    _tmp=$(mktemp -d "${TMPDIR:-/tmp}/dcent-offline-soak-self.XXXXXX")
    TMP_DIR=$_tmp

    _stable="$_tmp/stable.csv"
    _rss_leak="$_tmp/rss-leak.csv"
    _fd_leak="$_tmp/fd-leak.csv"
    _live="$_tmp/live.csv"

    write_series "$_stable" \
        '0,0,10000,5' \
        '1,7200,10064,5' \
        '2,14400,10032,6'
    write_series "$_rss_leak" \
        '0,0,10000,5' \
        '1,7200,16000,5' \
        '2,14400,22000,5'
    write_series "$_fd_leak" \
        '0,0,10000,5' \
        '1,7200,10000,8' \
        '2,14400,10000,14'

    sh "$SCRIPT_DIR/offline_soak_harness.sh" --series "$_stable" \
        --max-rss-growth-kb 128 --max-fd-growth 2 >/dev/null

    if sh "$SCRIPT_DIR/offline_soak_harness.sh" --series "$_rss_leak" \
        --max-rss-growth-kb 128 --max-fd-growth 2 >/dev/null 2>&1; then
        printf 'FAIL: RSS-growth self-test did not fail\n' >&2
        return 1
    fi

    if sh "$SCRIPT_DIR/offline_soak_harness.sh" --series "$_fd_leak" \
        --max-rss-growth-kb 128 --max-fd-growth 2 >/dev/null 2>&1; then
        printf 'FAIL: fd-growth self-test did not fail\n' >&2
        return 1
    fi

    sh "$SCRIPT_DIR/offline_soak_harness.sh" --samples 3 --sample-interval 1 \
        --logical-hours 4 --max-rss-growth-kb 8192 --max-fd-growth 4 \
        --output "$_live" >/dev/null

    printf 'PASS: offline soak harness self-test covers stable, RSS-growth, fd-growth, and live local-process sampling\n'
}

parse_args "$@"
validate_options

case "$MODE" in
    self_test)
        self_test
        ;;
    series)
        [ -n "$SERIES_FILE" ] || die "--series requires a path"
        validate_series "$SERIES_FILE"
        ;;
    run)
        start_worker
        sample_process
        if [ "$KEEP_OUTPUT" -eq 1 ]; then
            printf 'PASS: offline soak samples written to %s\n' "$OUTPUT_FILE"
        fi
        ;;
    *)
        die "internal error: unknown mode $MODE"
        ;;
esac
