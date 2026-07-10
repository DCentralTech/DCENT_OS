#!/bin/sh
#
# AM335x-safe early init for DCENT_OS BB SD/initramfs boots.

PATH=/usr/bin:/bin:/usr/sbin:/sbin

mountpoint_quiet() {
    grep -qs " $1 " /proc/mounts 2>/dev/null
}

mountpoint_quiet /proc || mount -t proc proc /proc 2>/dev/null || true
mountpoint_quiet /sys || mount -t sysfs sysfs /sys 2>/dev/null || true

if [ ! -e /dev/console ]; then
    mount -t devtmpfs devtmpfs /dev 2>/dev/null || mount -t tmpfs -o size=1m,mode=0755 tmpfs /dev 2>/dev/null || true
    [ -e /dev/console ] || mknod -m 600 /dev/console c 5 1 2>/dev/null || true
    [ -e /dev/null ] || mknod -m 666 /dev/null c 1 3 2>/dev/null || true
    [ -e /dev/zero ] || mknod -m 666 /dev/zero c 1 5 2>/dev/null || true
    [ -e /dev/tty ] || mknod -m 666 /dev/tty c 5 0 2>/dev/null || true
    [ -e /dev/random ] || mknod -m 444 /dev/random c 1 8 2>/dev/null || true
    [ -e /dev/urandom ] || mknod -m 444 /dev/urandom c 1 9 2>/dev/null || true
fi

mkdir -p /dev/pts /dev/shm /tmp /run/lock /run/dropbear /var/run /var/log /etc/dropbear /data/dcent
mountpoint_quiet /dev/pts || mount -t devpts devpts /dev/pts -o gid=5,mode=620 2>/dev/null || true
mountpoint_quiet /tmp || mount -t tmpfs -o size=64m,mode=1777 tmpfs /tmp 2>/dev/null || true
mountpoint_quiet /run || mount -t tmpfs -o size=4m,mode=0755 tmpfs /run 2>/dev/null || true

ln -sf /proc/self/fd /dev/fd 2>/dev/null || true
ln -sf fd/0 /dev/stdin 2>/dev/null || true
ln -sf fd/1 /dev/stdout 2>/dev/null || true
ln -sf fd/2 /dev/stderr 2>/dev/null || true
ln -sf ../tmp/log /dev/log 2>/dev/null || true

ip link set lo up 2>/dev/null || true
ip addr add 127.0.0.1/8 dev lo 2>/dev/null || true

for class_dir in /sys/class/mtd/mtd* /sys/class/tty/ttyO* /sys/class/i2c-dev/i2c-* /sys/class/gpio/gpiochip*; do
    [ -e "$class_dir/dev" ] || continue
    name=$(basename "$class_dir")
    dev=$(cat "$class_dir/dev" 2>/dev/null || true)
    [ -n "$dev" ] || continue
    major=${dev%%:*}
    minor=${dev##*:}
    case "$class_dir" in
        /sys/class/mtd/*) mknod -m 660 "/dev/$name" c "$major" "$minor" 2>/dev/null || true ;;
        /sys/class/tty/*) mknod -m 660 "/dev/$name" c "$major" "$minor" 2>/dev/null || true ;;
        /sys/class/i2c-dev/*) mknod -m 660 "/dev/$name" c "$major" "$minor" 2>/dev/null || true ;;
        /sys/class/gpio/*) mknod -m 660 "/dev/$name" c "$major" "$minor" 2>/dev/null || true ;;
    esac
done

touch /tmp/resolv.conf 2>/dev/null || true
[ -e /etc/resolv.conf ] || ln -s /tmp/resolv.conf /etc/resolv.conf 2>/dev/null || true

chmod 0700 /data/dcent 2>/dev/null || true
hostname dcentos-bb 2>/dev/null || true

echo "DCENT_OS AM335x early init complete"
