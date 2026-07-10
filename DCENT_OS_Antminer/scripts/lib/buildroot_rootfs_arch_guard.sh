#!/bin/sh
#
# Buildroot rootfs architecture guards used by board post-build hooks.

dcent_rootfs_guard_error() {
    prefix=$1
    message=$2
    echo "${prefix}: ERROR: ${message}" >&2
    exit 1
}

dcent_require_armv7_eabi_elf() {
    prefix=$1
    rel_path=$2
    abs_path=$3

    if [ ! -e "$abs_path" ]; then
        dcent_rootfs_guard_error "$prefix" "${rel_path} is missing"
    fi

    desc=$(file -Lb "$abs_path" 2>/dev/null || true)
    case "$desc" in
        *"ELF 32-bit LSB"*ARM*EABI5*)
            return 0
            ;;
    esac

    dcent_rootfs_guard_error "$prefix" "${rel_path} must be ARMv7/EABI5 ELF; got: ${desc:-unknown file type}"
}

dcent_ensure_armv7_busybox_init() {
    target_dir=$1
    prefix=$2

    dcent_require_armv7_eabi_elf "$prefix" "/bin/busybox" "${target_dir}/bin/busybox"

    desc=$(file -Lb "${target_dir}/sbin/init" 2>/dev/null || true)
    case "$desc" in
        *"ELF 32-bit LSB"*ARM*EABI5*)
            return 0
            ;;
    esac

    echo "${prefix}: replacing invalid /sbin/init (${desc:-missing}) with BusyBox init symlink" >&2
    mkdir -p "${target_dir}/sbin"
    rm -f "${target_dir}/sbin/init"
    ln -s ../bin/busybox "${target_dir}/sbin/init"

    dcent_require_armv7_eabi_elf "$prefix" "/sbin/init" "${target_dir}/sbin/init"
}

dcent_require_armv7_eabi_elf_paths() {
    target_dir=$1
    prefix=$2
    shift 2

    for rel_path in "$@"; do
        dcent_require_armv7_eabi_elf "$prefix" "/${rel_path}" "${target_dir}/${rel_path}"
    done
}
