#!/bin/sh
#
# DCENTos Early Init — runs from inittab BEFORE /etc/init.d/rcS
# D-Central Technologies — DCENTos v0.4.0
#
# CRITICAL: BraiinsOS 4.4.0-xilinx kernel has CONFIG_DEVTMPFS=not set
# The kernel does NOT auto-populate /dev. We must:
#   1. Mount tmpfs on /dev
#   2. Create essential device nodes with mknod
#   3. Enumerate sysfs to create MTD/UBI/TTY nodes (eudev doesn't on this kernel)
#
# Squashfs rootfs is mounted read-only by kernel (bootargs: root=... r)
# This script sets up writable areas for runtime data.
#

# --- Essential virtual filesystems ---
mount -t proc proc /proc
mount -t sysfs sysfs /sys

# --- Loopback interface (required for local DCENT_OS services and API access) ---
ip link set lo up
ip addr add 127.0.0.1/8 dev lo 2>/dev/null

# --- Writable /dev on tmpfs (NO devtmpfs in kernel!) ---
mount -t tmpfs -o size=512k,mode=0755 tmpfs /dev

# Essential device nodes (minimum for BusyBox init + early scripts)
mknod -m 600 /dev/console c 5 1
mknod -m 666 /dev/null c 1 3
mknod -m 666 /dev/zero c 1 5
mknod -m 666 /dev/tty c 5 0
mknod -m 444 /dev/urandom c 1 9
mknod -m 444 /dev/random c 1 8
mknod -m 660 /dev/mem c 1 1
mknod -m 660 /dev/kmem c 1 2
mknod -m 600 /dev/kmsg c 1 11
mkdir -p /dev/pts /dev/shm

# Standard /dev symlinks
ln -s /proc/self/fd /dev/fd
ln -s fd/0 /dev/stdin
ln -s fd/1 /dev/stdout
ln -s fd/2 /dev/stderr
ln -s ../tmp/log /dev/log

# devpts for pseudo-terminals (SSH sessions)
mount -t devpts devpts /dev/pts -o gid=5,mode=620

# --- Create device nodes from sysfs ---
# eudev/udevd on this 4.4.0 kernel doesn't create nodes for MTD/UBI/TTY.
# We enumerate sysfs and create them manually.

# MTD devices (NAND flash partitions)
for mtd in /sys/class/mtd/mtd*; do
    name=$(basename "$mtd")
    dev=$(cat "$mtd/dev" 2>/dev/null)
    if [ -n "$dev" ]; then
        major=${dev%%:*}
        minor=${dev##*:}
        case "$name" in
            *ro) mknod -m 444 "/dev/$name" c "$major" "$minor" 2>/dev/null ;;
            *)   mknod -m 660 "/dev/$name" c "$major" "$minor" 2>/dev/null ;;
        esac
    fi
done

# UBI devices (UBI volumes on NAND)
for ubi in /sys/class/ubi/ubi*; do
    name=$(basename "$ubi")
    dev=$(cat "$ubi/dev" 2>/dev/null)
    if [ -n "$dev" ]; then
        major=${dev%%:*}
        minor=${dev##*:}
        mknod -m 660 "/dev/$name" c "$major" "$minor" 2>/dev/null
    fi
done

# UBI control device
if [ -e /sys/class/misc/ubi_ctrl/dev ]; then
    dev=$(cat /sys/class/misc/ubi_ctrl/dev)
    major=${dev%%:*}
    minor=${dev##*:}
    mknod -m 660 /dev/ubi_ctrl c "$major" "$minor" 2>/dev/null
fi

# UBI block devices
for blk in /sys/class/block/ubiblock*; do
    name=$(basename "$blk")
    dev=$(cat "$blk/dev" 2>/dev/null)
    if [ -n "$dev" ]; then
        major=${dev%%:*}
        minor=${dev##*:}
        mknod -m 660 "/dev/$name" b "$major" "$minor" 2>/dev/null
    fi
done

# TTY devices (serial ports)
for tty_dev in /sys/class/tty/ttyPS*; do
    name=$(basename "$tty_dev")
    dev=$(cat "$tty_dev/dev" 2>/dev/null)
    if [ -n "$dev" ]; then
        major=${dev%%:*}
        minor=${dev##*:}
        mknod -m 660 "/dev/$name" c "$major" "$minor" 2>/dev/null
    fi
done

# I2C devices (for PIC voltage controllers, TMP75 sensors, EEPROM, PSU)
for i2c in /sys/class/i2c-dev/i2c-*; do
    name=$(basename "$i2c")
    dev=$(cat "$i2c/dev" 2>/dev/null)
    if [ -n "$dev" ]; then
        major=${dev%%:*}
        minor=${dev##*:}
        mknod -m 660 "/dev/$name" c "$major" "$minor" 2>/dev/null
    fi
done

# UIO devices (FPGA access — critical for hash board communication)
for uio in /sys/class/uio/uio*; do
    name=$(basename "$uio")
    dev=$(cat "$uio/dev" 2>/dev/null)
    if [ -n "$dev" ]; then
        major=${dev%%:*}
        minor=${dev##*:}
        mknod -m 666 "/dev/$name" c "$major" "$minor" 2>/dev/null
    fi
done

# --- Writable tmpfs for runtime data ---
# /tmp: logs, temp files (squashfs /var/log -> ../tmp)
mount -t tmpfs -o size=64m,mode=1777 tmpfs /tmp

# /run: PID files, lock files (squashfs /var/run -> ../run, /var/lock -> ../run/lock)
mount -t tmpfs -o size=1m,mode=0755 tmpfs /run
mkdir -p /run/lock /run/dropbear

# --- Writable resolv.conf for DHCP/DNS ---
# Bind-mount a tmpfs-backed file over the read-only squashfs placeholder
touch /tmp/resolv.conf
mount --bind /tmp/resolv.conf /etc/resolv.conf

# --- SSH key setup (writable /root) ---
# Copy /root to tmpfs and overlay, so SSH authorized_keys can be written
if [ -d /root ]; then
    cp -a /root /tmp/root-copy 2>/dev/null
    mkdir -p /tmp/root-copy/.ssh
    chmod 700 /tmp/root-copy
    chmod 700 /tmp/root-copy/.ssh
    mount --bind /tmp/root-copy /root
fi

# --- Mount persistent storage (UBIFS on rootfs_data UBI volume) ---
# rootfs_data is a 72MB dynamic UBI volume on mtd8 (NAND flash).
# UBIFS auto-creates the filesystem on first mount. Data survives reboots
# without modifying the read-only squashfs root image.
mkdir -p /data
mount -t ubifs ubi0:rootfs_data /data 2>/dev/null
if [ $? -eq 0 ]; then
    echo "[OK] Persistent storage mounted at /data"
    # Create standard directories
    mkdir -p /data/config /data/profiles /data/keys /data/logs /data/overlay

    # Overlay /etc for persistent config changes
    # Lower = read-only squashfs /etc, Upper = writable NAND-backed overlay
    # Any writes to /etc go to upper layer on NAND, surviving reboots.
    mkdir -p /data/overlay/etc/upper /data/overlay/etc/work
    mount -t overlay overlay \
        -o lowerdir=/etc,upperdir=/data/overlay/etc/upper,workdir=/data/overlay/etc/work \
        /etc
    echo "[OK] Persistent /etc overlay active"

    # Reconcile the persistent saved-seed transaction.  The native helper
    # verifies /dev/urandom identity, mixes trusted saved bytes through the
    # normal write path, and accounts them separately with RNDADDENTROPY.
    # Input-pool accounting is observability only; successor generation is
    # independently gated on nonblocking-CRNG readiness.
    if [ -e /data/keys/random-seed ] || [ -L /data/keys/random-seed ] || \
       [ -e /data/keys/.random-seed.consumed ] || [ -L /data/keys/.random-seed.consumed ] || \
       [ -e /data/keys/.random-seed.credited ] || [ -L /data/keys/.random-seed.credited ] || \
       [ -e /data/keys/.random-seed.born ] || [ -L /data/keys/.random-seed.born ] || \
       [ -e /data/keys/.random-seed.born.new ] || [ -L /data/keys/.random-seed.born.new ] || \
       [ -e /data/keys/.random-seed.new ] || [ -L /data/keys/.random-seed.new ]; then
        _ent_before=$(cat /proc/sys/kernel/random/entropy_avail)
        if [ -x /usr/sbin/seed-entropy ]; then
            if /usr/sbin/seed-entropy /data/keys/random-seed; then
                _rc=0
            else
                _rc=$?
            fi
            _ent_after=$(cat /proc/sys/kernel/random/entropy_avail)
            echo "seed-entropy: ${_ent_before} -> ${_ent_after} bits (rc=${_rc})" > /dev/kmsg 2>/dev/null
            echo "seed-entropy: ${_ent_before} -> ${_ent_after} bits (rc=${_rc})" >> /tmp/seed-debug.log
            if [ "$_rc" -eq 0 ]; then
                echo "[OK] Entropy lifecycle reconciled (${_ent_before} -> ${_ent_after} bits)"
            else
                echo "[!!] Entropy lifecycle incomplete; replay remains blocked (seed-entropy rc=$_rc)"
            fi
        else
            _ent_after=$(cat /proc/sys/kernel/random/entropy_avail)
            echo "seed-entropy: helper missing; seed left untouched" >> /tmp/seed-debug.log
            echo "[!!] Entropy helper missing; refusing uncredited seed fallback"
        fi
    else
        echo "seed-entropy: NO SEED FILE" >> /tmp/seed-debug.log
        echo "[!!] No saved entropy seed; relying on live kernel CRNG initialization"
    fi

    # Ensure /etc/dropbear/ exists (overlayfs whiteout cleanup)
    # On older kernels, overlayfs can create whiteout entries that hide directories.
    # Force-create via the upper layer if the directory isn't visible.
    if [ ! -d /etc/dropbear ]; then
        # Remove any whiteout in upper layer, then create directory
        rm -f /data/overlay/etc/upper/dropbear 2>/dev/null
        mkdir -p /data/overlay/etc/upper/dropbear
        # Remount overlay to pick up the change
        umount /etc 2>/dev/null
        mount -t overlay overlay \
            -o lowerdir=/etc,upperdir=/data/overlay/etc/upper,workdir=/data/overlay/etc/work \
            /etc
    fi

    # Persist SSH host keys (no more regeneration on reboot!)
    if [ -d /data/keys/dropbear ] && [ -d /etc/dropbear ]; then
        # Restore saved keys into the overlayed /etc
        cp /data/keys/dropbear/* /etc/dropbear/ 2>/dev/null
        echo "[OK] SSH host keys restored from persistent storage"
    fi
else
    echo "[!!] Failed to mount persistent storage — running without persistence"
    # CRITICAL FALLBACK: Even without persistent storage, /etc MUST be writable
    # for dropbear SSH host key generation and DHCP resolv.conf updates.
    mkdir -p /tmp/etc-overlay/upper /tmp/etc-overlay/work
    mount -t overlay overlay \
        -o lowerdir=/etc,upperdir=/tmp/etc-overlay/upper,workdir=/tmp/etc-overlay/work \
        /etc
    echo "[OK] Tmpfs /etc overlay active (non-persistent)"
    # Ensure /etc/dropbear exists even without persistent storage
    mkdir -p /etc/dropbear 2>/dev/null
fi

# --- Export FPGA GPIO pins (platform-dependent) ---
# FIX (2026-04-13, swarm #4): Detect platform before writing GPIO registers.
# S9 (am1-s9) and S19 Pro (am2-s17) have different FPGA GPIO layouts.
IS_AM2=false
if ls /sys/class/uio/*/name 2>/dev/null | xargs cat 2>/dev/null | grep -q "board-control"; then
    IS_AM2=true
fi

if [ "$IS_AM2" = "true" ]; then
    # am2-s17 (S19 Pro / S19j Pro Zynq): board-control register block at
    # 0x42810000 stays UIO-managed by dcentrald. The S9 FPGA GPIO addresses
    # (0x41200000/0x41210000) MUST NOT be written here — wrong peripheral.
    #
    #  Part 3 (RE-018 Agent 1, 2026-05-25): the BM1362 hashboard
    # reset lines (HB0..3_RESET) are exposed via kernel gpiochip897 as
    # gpio897..gpio900. BraiinsOS pre-exports these via libgpiod
    # (gpiod-0.2.3 — Phase 13D Ghidra RE) so its bosminer can long-hold
    # them LOW during the 4-second reset dance. DCENT_OS s19j_hybrid_mining
    # Phase 2b-extended drives the same long-hold via the sysfs
    # /sys/class/gpio/gpioN/value path. Without the exports below,
    # those sysfs writes ENOENT silently and the long-hold reaches NOTHING
    # (only the brief ~20 ms devmem pre-pulse fires).  LIVE log
    # captured 8 sysfs WRITE FAILED messages on .25 as direct evidence.
    #
    # We do NOT take ownership away from the kernel gpiochip — we only
    # export the individual lines so sysfs writes succeed. Reset polarity
    # is active-LOW for BM1362 (CH0-2 board reset OUT), so value=1 means
    # "released from reset" — chips can communicate. dcentrald's
    # Phase 2b-extended then drives them LOW for the 4 s long-hold and
    # back HIGH to release.
    for gpio in 897 898 899 900; do
        if [ ! -d /sys/class/gpio/gpio${gpio} ]; then
            echo $gpio > /sys/class/gpio/export 2>/dev/null || true
        fi
        if [ -d /sys/class/gpio/gpio${gpio} ]; then
            echo out > /sys/class/gpio/gpio${gpio}/direction 2>/dev/null || true
            echo 1 > /sys/class/gpio/gpio${gpio}/value 2>/dev/null || true
        fi
    done
    echo "[OK] am2 HB_RESET gpio897-900 exported (released; dcentrald owns the long-hold reset dance)"
    echo "[OK] am2-s17 detected — board-control register block managed by dcentrald (skipping S9 GPIO setup)"
else
    # am1-s9: S9-specific FPGA GPIO
    # gpiochip897 (FPGA 0x41200000): pins 902-904 = plug detect (chains 6,7,8)
    # gpiochip884 (FPGA 0x41210000): pins 893-895 = board enable/reset (chains 6,7,8)

    # Plug detect: INPUT
    for gpio in 902 903 904; do
        echo $gpio > /sys/class/gpio/export 2>/dev/null
        echo "in" > /sys/class/gpio/gpio${gpio}/direction 2>/dev/null
    done

    # Board enable/reset: OUTPUT HIGH
    for gpio in 893 894 895; do
        echo $gpio > /sys/class/gpio/export 2>/dev/null
        echo "out" > /sys/class/gpio/gpio${gpio}/direction 2>/dev/null
        echo "1" > /sys/class/gpio/gpio${gpio}/value 2>/dev/null
    done

    if command -v devmem > /dev/null 2>&1; then
        devmem 0x41210000 32 0x00000E01 2>/dev/null
    fi

    echo "[OK] FPGA GPIO pins exported (plug-detect IN, board-enable OUT+HIGH)"

    # --- Hash board reset dance (hold in RESET for dcentrald) ---
    echo "Asserting hash board RESET (held LOW for dcentrald)..."
    for gpio in 893 894 895; do
        echo "0" > /sys/class/gpio/gpio${gpio}/value 2>/dev/null
    done
    if command -v devmem > /dev/null 2>&1; then
        devmem 0x41210000 32 0x00000001 2>/dev/null  # LED on, boards LOW
    fi
    sleep 3
    echo "[OK] Hash boards held in RESET (dcentrald controls release)"
fi

# --- I2C controller ---
# The kernel xiic-i2c driver initializes the AXI IIC controller during probe().
# v0.8.4 FIX: Removed devmem writes to AXI IIC CR (0x41600100) that were
# corrupting the kernel driver's cached register state. The devmem SOFTR reset
# cleared THIGH/TLOW/TBUF to 0 (max I2C speed), causing PICs to NACK and
# making 2/3 hash boards unreachable (0xFF on i2cdetect). This was the ROOT
# CAUSE of the "loses 2 hashboards every time" bug.
# See: ,
echo "[OK] I2C controller managed by kernel xiic-i2c driver"

# --- Set hostname ---
hostname dcentos

# --- Fan control: Quiet Home Mode ---
# DCENTos is designed for home mining. Command a low fan PWM on boot.
# On S9 this maps to a quiet curve; on AM2/XIL the physical fan controller may
# still hold a loud low-PWM/failsafe floor, so tach/RPM is the proof.
# The mining daemon (dcentrald) will manage fans dynamically once it starts.
# Safety rule: if dcentrald crashes or isn't running, hash power is cut before
# fan noise is raised. Powered/thermal-unknown failure paths use the home cap
# (PWM 30); a proven powered-off/parked unit uses idle PWM 10.
FAN_BASE=0x42800000
FAN_IDLE_PWM=10
# Platform dispatch:
# the am2 (S19/S19j Pro Zynq) fan-control IP @ 0x42800000 is UIO-bound, so
# devmem reads/writes are a proven NO-OP on am2 (a /dev/mem shadow — fans
# stay at hardware default). Reliable am2 fan control is the fan-control UIO mmap path,
# exposed as the `dcentrald --set-fan <PWM>` one-shot (R2; fan-register-only,
# clamped <= 30, synchronous). am1-s9 keeps the devmem path (correct there;
# IS_AM2 is set ~line 233 by the board-control UIO probe). If dcentrald is
# not present yet at this early-init point we fall back to devmem and note it
# (am2 devmem is a no-op, but dcentrald's own quiet-boot/park sets the real
# fan-control UIO PWM once it starts).
if [ "$IS_AM2" = "true" ]; then
    if [ -x /usr/local/bin/dcentrald ]; then
        /usr/local/bin/dcentrald --set-fan $FAN_IDLE_PWM
        echo "[OK] am2 fans commanded to home idle via fan-control UIO (PWM $FAN_IDLE_PWM)"
    else
        echo "[!!] am2: dcentrald absent at early-init — fans at hardware default until dcentrald starts (devmem is a no-op on am2)"
    fi
elif command -v devmem > /dev/null 2>&1; then
    devmem $((FAN_BASE + 0x10)) 32 $FAN_IDLE_PWM 2>/dev/null
    devmem $((FAN_BASE + 0x14)) 32 $FAN_IDLE_PWM 2>/dev/null
    echo "[OK] Fans commanded to quiet mode (PWM $FAN_IDLE_PWM; S9 curve ~1260 RPM)"
else
    echo "[!!] devmem not available — fans at hardware default"
fi

echo "DCENTos early-init: /dev /tmp /run mounted, devices created"
