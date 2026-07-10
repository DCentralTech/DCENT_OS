#!/bin/sh
#
# switch_firmware.sh — POSIX sh + awk reimplementation of switch_firmware.py
#
# Byte-identical /tmp/uboot_env_patched.bin output to
#   board/zynq/rootfs-overlay/usr/sbin/switch_firmware.py
# for the same /tmp/uboot_env.bin input and the same args:
#   sh switch_firmware.sh <1|2> --i-understand-this-is-not-fw-setenv [--with-stage]
#
# *** DEPRECATED FOR THE OTA/SYSUPGRADE WRITE PATH (W24-OTA-2). ***
# Exactly like switch_firmware.py, this script is the offline/forensic
# U-Boot env-image patcher ONLY. The canonical am2 A/B NAND writer
# (board/zynq/am2-s19jpro/rootfs-overlay/usr/sbin/sysupgrade) flips the
# LIVE A/B boot-selector with `fw_setenv` (libubootenv, redundant-copy-
# atomic) and does NOT invoke switch_firmware.{py,sh} — the raw
# dd/flash_erase/nandwrite env flip is the documented .39/.139 brick root
# cause. To stay argv-faithful to switch_firmware.py's matching gate, this
# script REFUSES to run without the explicit
# `--i-understand-this-is-not-fw-setenv` acknowledgement so it can never
# stand in for fw_setenv by accident.
#
# Why this exists: it is the dependency-free POSIX-sh+awk twin of the .py
# CRC32 env patcher for a BraiinsOS source that has NEITHER python3 NOR
# python. It is staged verbatim to /usr/sbin/ as a last-resort manual-
# recovery env patcher. It mirrors the proven writer's manifest_field()
# jsonfilter-over-python3 idiom only as a byte-READER cascade (od ->
# hexdump), never as the live env flip.
#
# On-target tool budget (BraiinsOS BusyBox): sh, awk (/usr/bin/awk),
# dd, od, printf, cut, the standard busybox set. NO python, NO perl,
# NO lua, NO gawk-only features. Binary-safe: every byte (incl. 0x00
# and 0xff and high bytes) is moved through awk as an integer 0..255.
#
# Same nonzero-exit-on-error contract as the python: a CRC-parse
# failure on copy 1 -> exit 1 (NO output written -> the proven writer's
# cmp -s readback gate fails closed -> env NOT flipped -> no brick).
#
# Spec mirrored exactly (see switch_firmware.py):
#   CRC32 = zlib/IEEE: table from poly 0xEDB88320, init 0xFFFFFFFF,
#           final ^ 0xFFFFFFFF.
#   COPY_SIZE = 131072 per redundant env copy.
#   parse_env_copy: first 4 bytes = stored CRC little-endian. Try
#     4-byte header (env=data[4:], crc over that); if mismatch try
#     5-byte header (env=data[5:], byte[4]=flags). Entries are
#     NUL-separated key=value, list ends at double-NUL.
#   --with-stage  -> firmware=N, upgrade_stage=0, delete first_boot.
#   (no flag)     -> firmware=N, delete upgrade_stage + first_boot.
#   Rebuild: for key in sorted(env_vars): "key=value\x00"; then one
#     extra trailing \x00. Pad with 0xff so len == COPY_SIZE - hdr.
#   new_crc = crc32(new_env). single = <I new_crc> + (5-byte hdr:
#     <B (flags1+1)&0xFF>) + new_env.
#   redundant (input >= COPY_SIZE*2): out = copy1 + copy2, pad 0xff to
#     original total input size; else out = single. W24-OTA-2: copy1 is
#     the single buffer verbatim; for a 5-byte (CRC+flags) env copy2
#     carries a DISTINCT flag (copy1_flag+1)&0xFF so U-Boot's redundant-
#     env "newer copy" disambiguation stays well-defined and byte-matches
#     switch_firmware.py (identical flags bricked .39/.139). A 4-byte
#     (no-flags) env has no flag field, so copy1 == copy2.
#   Write /tmp/uboot_env_patched.bin.
#

# Force a single-byte (C/POSIX) locale for the WHOLE script. This makes
# awk `printf "%c", n` emit the RAW byte n (0..255) and makes awk
# length()/substr()/sprintf("%c") strictly byte-oriented — the
# byte-faithful primitives the env rebuild + CRC + file write depend
# on. Without this, a gawk host in a UTF-8 locale would multibyte-
# encode bytes >=128 and silently corrupt the patched env (a brick).
# busybox awk is always single-byte; this only hardens gawk hosts and
# is harmless on the BraiinsOS busybox target.
LC_ALL=C
LANG=C
export LC_ALL LANG

ENV_FILE="/tmp/uboot_env.bin"
OUT_FILE="/tmp/uboot_env_patched.bin"
COPY_SIZE=131072

# --- Parse arguments (mirror switch_firmware.py argv handling) ---
WITH_STAGE=0
TARGET_FW=""
ACK_NOT_FW_SETENV=0
for arg in "$@"; do
    case "$arg" in
        --with-stage) WITH_STAGE=1 ;;
        --i-understand-this-is-not-fw-setenv) ACK_NOT_FW_SETENV=1 ;;
        1|2) TARGET_FW="$arg" ;;
        *)
            echo "Unknown argument: $arg"
            exit 1
            ;;
    esac
done

if [ -z "$TARGET_FW" ]; then
    echo "Usage: sh switch_firmware.sh <1|2> --i-understand-this-is-not-fw-setenv [--with-stage]"
    echo "  1 = firmware1 (mtd7)"
    echo "  2 = firmware2 (mtd8)"
    echo "  --with-stage = set upgrade_stage=0 for auto_recovery"
    exit 1
fi

# W24-OTA-2: refuse to run as a stand-in for fw_setenv (mirror
# switch_firmware.py). The OTA/sysupgrade write path flips the live A/B
# selector with fw_setenv (libubootenv) — this script is offline/forensic
# recovery only and must never be the live env-flip mechanism. Argv-
# faithful to the .py gate so the two scripts AGREE: both REFUSE without
# the ack flag (exit 2), both PROCEED with it.
if [ "$ACK_NOT_FW_SETENV" -ne 1 ]; then
    echo "REFUSING: switch_firmware.sh is DEPRECATED for the env flip."
    echo "  The am1-s9 sysupgrade now flips the A/B selector with fw_setenv"
    echo "  (libubootenv, redundant-copy-atomic). The raw flash_erase-both +"
    echo "  identical-flag dual-write this script feeds bricked .39/.139."
    echo "  To flip the live env safely:"
    echo "    fw_setenv firmware <1|2>; fw_setenv upgrade_stage 0; fw_setenv first_boot yes"
    echo "  If you REALLY need offline env-image patching (no libubootenv),"
    echo "  re-run with --i-understand-this-is-not-fw-setenv."
    exit 2
fi

if [ ! -f "$ENV_FILE" ]; then
    echo "ERROR: $ENV_FILE not found"
    exit 1
fi

# Stream the whole input as whitespace-separated unsigned decimal bytes
# to awk on stdin. EVERY byte (including 0x00 / 0xff) is carried as a
# 0..255 integer — the only binary-safe way to move arbitrary bytes
# through POSIX awk. awk writes OUT_FILE directly via `printf "%c", n`
# (raw single byte under LC_ALL=C) so the output is byte-exact (no
# shell re-encoding round-trip).
#
# Byte-reader prefer/fallback cascade (mirrors the proven writer's
# `manifest_field()` jsonfilter-over-python3 idiom):
#   1. `od -An -v -tu1`           — preferred. Present on a DCENT_OS
#                                    source rootfs (env re-flash path,
#                                    behaviour UNCHANGED).
#   2. `hexdump -v -e '1/1 "%u "'` — fallback. BraiinsOS BusyBox has
#                                    NO `od` but DOES ship hexdump
#                                    (/usr/bin/hexdump, confirmed on
#                                    the live .109 unit). Verified
#                                    byte-faithful: a 0x00/0xff/0x41
#                                    input emits `0 255 65 `.
# Both emit the SAME multiset of whitespace-separated 0..255 decimals
# in the SAME order with the SAME count — only the separator run
# differs (od: leading spaces + ~16/line newlines; hexdump: one
# trailing space per byte, no newlines). The awk consumer below splits
# on default FS/RS (any whitespace, leading/trailing ignored) and
# accumulates $1..$NF per record into B[], so the two streams parse
# byte-for-byte identically. No 3rd path is shipped: two readers are
# sufficient (DCENT_OS has od, BraiinsOS has hexdump) and an unproven
# xxd path is deliberately omitted.
#
# `SWITCH_FW_FORCE_BYTEREADER` (od|hexdump) forces one reader instead
# of auto prefer/fallback — used ONLY here, ONLY by the byte-identity
# test to exercise the hexdump path on a host that also has od. Unset
# (the default) = the prefer/fallback auto-detect above. An unknown
# value is rejected fail-closed (no patched output -> no brick).
#
# awk does ALL of: CRC32, header detection, env parse, mutate, sorted
# rebuild, pad, repack, redundant-copy, and the file write. The exit
# status of awk (propagated through the pipe via the byte_stream
# function) is the exit status of this script.

byte_stream() {
    case "${SWITCH_FW_FORCE_BYTEREADER:-}" in
        od)
            od -An -v -tu1 "$ENV_FILE"
            ;;
        hexdump)
            hexdump -v -e '1/1 "%u "' "$ENV_FILE"
            ;;
        "")
            if command -v od >/dev/null 2>&1; then
                od -An -v -tu1 "$ENV_FILE"
            elif command -v hexdump >/dev/null 2>&1; then
                hexdump -v -e '1/1 "%u "' "$ENV_FILE"
            else
                echo "ERROR: no byte reader (need od or hexdump)" >&2
                exit 1
            fi
            ;;
        *)
            echo "ERROR: unknown SWITCH_FW_FORCE_BYTEREADER='${SWITCH_FW_FORCE_BYTEREADER}' (od|hexdump)" >&2
            exit 1
            ;;
    esac
}

byte_stream | awk \
    -v copy_size="$COPY_SIZE" \
    -v target_fw="$TARGET_FW" \
    -v with_stage="$WITH_STAGE" \
    -v out_file="$OUT_FILE" '
function crc32(arr, n,   crc, i, b) {
    # Pure CRC32 (zlib / U-Boot crc32): init 0xFFFFFFFF, table from
    # poly 0xEDB88320, final ^ 0xFFFFFFFF. crc_table[] is built once
    # in BEGIN. arr is 0-indexed bytes [0 .. n-1].
    crc = 4294967295            # 0xFFFFFFFF
    for (i = 0; i < n; i++) {
        b = arr[i]
        crc = xor32(crc_table[and8(xor32(crc, b))], rshift8(crc))
    }
    return xor32(crc, 4294967295)
}
# --- 32-bit bitops without relying on gawk-only operators ---
# awk numbers are doubles; 32-bit ints fit exactly. We implement
# xor / and(0xFF) / >>8 / >>1 by hand so this is busybox-awk-safe
# (busybox awk has no & | ^ operators).
function xor32(a, b,   r, bit, x, y) {
    r = 0
    bit = 1
    while (bit <= 2147483648) {        # up to 0x80000000
        x = int(a / bit) % 2
        y = int(b / bit) % 2
        if (x != y) r = r + bit
        if (bit == 2147483648) break
        bit = bit * 2
    }
    return r
}
function and8(a) {                      # a & 0xFF
    return a % 256
}
function rshift8(a) {                    # (a >> 8), 32-bit logical
    return int(a / 256)
}
function rshift1(a) {                    # (a >> 1), logical
    return int(a / 2)
}
# Build the CRC32 table entry for index i (poly 0xEDB88320).
function crc_table_entry(i,   c, k) {
    c = i
    for (k = 0; k < 8; k++) {
        if (c % 2 == 1)
            c = xor32(3988292384, rshift1(c))   # 0xEDB88320 ^ (c>>1)
        else
            c = rshift1(c)
    }
    return c
}
# parse_env_copy: input bytes data[0 .. dlen-1] (one COPY_SIZE slice).
# Returns via globals: P_OK (1/0), P_HDR (4 or 5), P_FLAGS (byte at
# index 4 when 5-byte hdr, else -1). Fills assoc array EV[key]=value
# and ordered key discovery is irrelevant (we sort later). Mirrors the
# python exactly: stored_crc LE u32 of data[0:4]; try env=data[4:]
# crc; on mismatch try env=data[5:] crc; entries split on 0x00, list
# terminates at first 0x00 0x00 (double-NUL), key=value parsed with
# eq position > 0 (a leading "=" -> skipped, like python find()>0).
function parse_env_copy(data, dlen,   stored, calc, hs, es, i, tlen, ch) {
    P_OK = 0
    P_HDR = 0
    P_FLAGS = -1
    delete EV
    if (dlen < 8) return
    # stored CRC little-endian from data[0..3]
    stored = data[0] + data[1]*256 + data[2]*65536 + data[3]*16777216

    # Try 4-byte header: env = data[4 .. dlen-1]
    hs = 4
    es = 4
    delete TMP
    for (i = es; i < dlen; i++) TMP[i - es] = data[i]
    calc = crc32(TMP, dlen - es)
    if (calc != stored) {
        # Try 5-byte header: env = data[5 .. dlen-1]
        hs = 5
        es = 5
        delete TMP
        for (i = es; i < dlen; i++) TMP[i - es] = data[i]
        calc = crc32(TMP, dlen - es)
        if (calc != stored) return         # both mismatch -> None
    }
    P_HDR = hs

    # env bytes now in TMP[0 .. (dlen-es-1)]
    elen = dlen - es
    # find double-NUL (0x00 0x00). end_idx = position of first 0x00
    # whose next byte is also 0x00. python: env_str = env[:end_idx+1]
    # then split on \x00 keeping non-empty -> equivalent to scanning
    # NUL-separated tokens until an empty token (two consecutive 0x00)
    # OR running off the end.
    #
    # tlen = current token byte count (accumulated into the GLOBAL
    # CUR[]). MUST NOT be a parse_env_copy local — parse_kv() reads the
    # token length, and awk has no nested scope: a function only sees
    # globals + its own params. So we pass the length explicitly.
    tlen = 0
    i = 0
    while (i < elen) {
        ch = TMP[i]
        if (ch == 0) {
            if (tlen == 0) {
                # empty token => double-NUL list terminator
                break
            }
            parse_kv(tlen)        # one complete "key=value" token
            tlen = 0
        } else {
            CUR[tlen] = ch
            tlen = tlen + 1
        }
        i = i + 1
    }
    # python: if no double-NUL found, end_idx=len(env); env_str=env[:end+1]
    # which still splits the same NUL-separated tokens. A trailing
    # token with no terminating 0x00 before EOF: python split on \x00
    # would still yield it. Replicate: flush a pending non-empty token.
    if (tlen > 0) parse_kv(tlen)
    P_FLAGS = (hs == 5) ? data[4] : -1
    P_OK = 1
}
# Convert the current token bytes CUR[0..clen-1] into key=value and
# store in EV. python: v_str = bytes.decode("ascii", errors="replace");
# eq_pos = v_str.find("="); if eq_pos > 0: env[v[:eq]] = v[eq+1:].
# All real U-Boot env is ASCII; we treat each byte as its char. A
# byte >=128 would be U+FFFD in python ("replace"); env keys/values
# are ASCII in practice (the CRC already proved the copy is the real
# env) so byte==char is exact for the data this ever sees. eq position
# is the FIRST 0x3D ("="); >0 required (leading "=" -> skipped, exactly
# like the python "if eq_pos > 0"). clen is the token length passed
# explicitly by parse_env_copy (an awk function cannot see a caller
# local — only globals + its own params; CUR[] is global).
function parse_kv(clen,   p, eqpos, k, v) {
    eqpos = -1
    for (p = 0; p < clen; p++) {
        if (CUR[p] == 61) {        # 0x3D "="
            eqpos = p
            break
        }
    }
    if (eqpos > 0) {
        k = ""
        for (p = 0; p < eqpos; p++) k = k sprintf("%c", CUR[p])
        v = ""
        for (p = eqpos + 1; p < clen; p++) v = v sprintf("%c", CUR[p])
        EV[k] = v
    }
}
# emit one byte (0..255) into the single-copy output buffer OBUF[].
# Collected here, repacked (redundant/single) in END, then written to
# OUT_FILE via `printf "%c", n` under LC_ALL=C (raw single byte, 0x00
# and 0xff safe, gawk + busybox awk).
function ob(byte) {
    OBUF[ON] = byte
    ON = ON + 1
}
BEGIN {
    # Build CRC32 table once.
    for (ti = 0; ti < 256; ti++) crc_table[ti] = crc_table_entry(ti)
    NBYTES = 0
}
{
    # od gives whitespace-separated decimal bytes; accumulate into B[].
    for (f = 1; f <= NF; f++) {
        B[NBYTES] = $f + 0
        NBYTES = NBYTES + 1
    }
}
END {
    total = NBYTES
    has_redundant = (total >= copy_size * 2) ? 1 : 0

    # --- Parse copy 1 (offset 0) ---
    cs = (total < copy_size) ? total : copy_size
    delete C1
    for (i = 0; i < cs; i++) C1[i] = B[i]
    parse_env_copy(C1, cs)
    if (P_OK != 1) {
        print "ERROR: Copy 1 (offset 0) CRC mismatch!" > "/dev/stderr"
        exit 1
    }
    header_size = P_HDR
    flags1 = P_FLAGS
    # EV[] now holds env_vars.

    # --- Mutate (mirror python exactly) ---
    EV["firmware"] = target_fw
    if (with_stage == 1) {
        EV["upgrade_stage"] = "0"
        if (("first_boot" in EV)) delete EV["first_boot"]
    } else {
        if (("upgrade_stage" in EV)) delete EV["upgrade_stage"]
        if (("first_boot" in EV)) delete EV["first_boot"]
    }

    # --- Rebuild new_env: sorted(keys) of "k=v\x00" then one \x00 ---
    nk = 0
    for (k in EV) { KEYS[nk] = k; nk = nk + 1 }
    # byte-wise / codepoint sort to match python sorted() on ascii keys
    for (a = 0; a < nk; a++) {
        for (bx = a + 1; bx < nk; bx++) {
            if (keycmp(KEYS[bx], KEYS[a]) < 0) {
                t = KEYS[a]; KEYS[a] = KEYS[bx]; KEYS[bx] = t
            }
        }
    }
    enlen = 0
    for (a = 0; a < nk; a++) {
        k = KEYS[a]
        v = EV[k]
        kl = length(k)
        for (p = 1; p <= kl; p++) { NE[enlen] = ascii(substr(k,p,1)); enlen++ }
        NE[enlen] = 61; enlen++          # "="
        vl = length(v)
        for (p = 1; p <= vl; p++) { NE[enlen] = ascii(substr(v,p,1)); enlen++ }
        NE[enlen] = 0; enlen++           # \x00
    }
    NE[enlen] = 0; enlen++               # extra trailing \x00

    pad_size = copy_size - header_size
    # python: new_env = new_env + b"\xff" * (pad_size - len(new_env))
    while (enlen < pad_size) { NE[enlen] = 255; enlen++ }

    new_crc = crc32(NE, enlen)

    # --- Build single copy: <I new_crc> [+ <B (flags1+1)&0xFF>] + new_env
    ON = 0
    # struct.pack("<I", new_crc) — little-endian u32
    ob(new_crc % 256)
    ob(int(new_crc / 256) % 256)
    ob(int(new_crc / 65536) % 256)
    ob(int(new_crc / 16777216) % 256)
    if (header_size == 5) {
        nf = (flags1 >= 0) ? ((flags1 + 1) % 256) : 1   # python: 0x01 if None
        ob(nf)
    }
    for (i = 0; i < enlen; i++) ob(NE[i])
    single_len = ON

    # --- Repack: redundant -> copy1 + copy2 padded to total; else single.
    # W24-OTA-2 distinct-flag fix (mirrors switch_firmware.py): copy1 is
    # the single buffer verbatim, but for a 5-byte (CRC+flags) env copy2
    # gets a DISTINCT flag byte (copy1_flag+1)&0xFF at index 4 so the
    # U-Boot redundant-env newer-copy disambiguation stays well-defined and
    # the output byte-matches the .py (identical flags bricked .39/.139). A
    # 4-byte (no-flags) env has no flag field, so the two copies stay
    # byte-identical.
    OUT_N = 0
    if (has_redundant == 1) {
        # copy1: the single buffer verbatim (5-byte hdr flag = (flags1+1)&0xFF).
        for (i = 0; i < single_len; i++) { OUT[OUT_N] = OBUF[i]; OUT_N++ }
        # copy2: W24-OTA-2 distinct flag for a 5-byte env; else verbatim.
        for (i = 0; i < single_len; i++) {
            if (header_size == 5 && i == 4)
                OUT[OUT_N] = (OBUF[4] + 1) % 256
            else
                OUT[OUT_N] = OBUF[i]
            OUT_N++
        }
        # python pads with 0xff up to original total input size
        while (OUT_N < total) { OUT[OUT_N] = 255; OUT_N++ }
    } else {
        for (i = 0; i < single_len; i++) { OUT[OUT_N] = OBUF[i]; OUT_N++ }
    }

    # --- Write OUT_FILE byte-exact ---
    # awk `printf "%c", n` emits the RAW byte n (0..255) when the awk
    # process runs in a single-byte (C/POSIX) locale — which the shell
    # wrapper forces via `LC_ALL=C`. This is byte-faithful for 0x00 and
    # 0xff and every high byte on BOTH gawk and busybox awk (busybox
    # awk %c is always single-byte; gawk %c is single-byte under
    # LC_ALL=C). NO \NNN escape interpretation is involved (awk does
    # not expand backslash escapes in a dynamic string argument), so
    # the octal-escape approach was wrong; %c is the correct primitive.
    for (i = 0; i < OUT_N; i++) printf "%c", OUT[i] > out_file
    close(out_file)
    exit 0
}
# ascii() — char -> 0..255 code. Built once into ASC[] for all 256.
function ascii(c) {
    if (!(ASC_BUILT)) {
        for (av = 0; av < 256; av++) ASC[sprintf("%c", av)] = av
        ASC_BUILT = 1
    }
    return ASC[c]
}
# keycmp(x, y) — byte-wise compare of two ascii strings, returning
# <0, 0, >0 to match python sorted() (lexicographic by code point).
function keycmp(x, y,   lx, ly, ml, p, cx, cy) {
    lx = length(x); ly = length(y)
    ml = (lx < ly) ? lx : ly
    for (p = 1; p <= ml; p++) {
        cx = ascii(substr(x, p, 1))
        cy = ascii(substr(y, p, 1))
        if (cx < cy) return -1
        if (cx > cy) return 1
    }
    if (lx < ly) return -1
    if (lx > ly) return 1
    return 0
}
'
