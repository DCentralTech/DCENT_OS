// DCENT_OS Init — Minimal PID 1 for BraiinsOS kernel
//
// D-Central Technologies — GPL-3.0
//
// This replaces /sbin/init on DCENT_OS rootfs images deployed onto BraiinsOS
// NAND slots. BraiinsOS's BusyBox lacks the init applet, and procd (OpenWrt)
// ignores our /etc/inittab and runs its own incompatible boot chain.
//
// PID 1 responsibilities in Linux:
//   - Mount virtual filesystems (/proc, /sys, /dev)
//   - Run init scripts in order
//   - Spawn login terminals (getty)
//   - Reap orphan zombie processes (waitpid)
//   - Handle shutdown signals (SIGTERM, SIGINT, SIGUSR1)
//   - NEVER exit (kernel panics if PID 1 exits)
//
// Design: Pure libc, no allocator-heavy code. Minimal dependencies.
// Target: armv7-unknown-linux-musleabihf (static, ~100KB)

use std::ffi::CString;
use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::{Duration, Instant};

// Signal flags — set by signal handlers, checked by main loop
static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);
static SHUTDOWN_SIGNAL: AtomicI32 = AtomicI32::new(0);

const CONSOLE_DEV: &str = "/dev/console";
const EARLY_INIT: &str = "/etc/dcentos-early-init.sh";
const INIT_D: &str = "/etc/init.d";
const GETTY_TTY: &str = "ttyPS0";
const GETTY_BAUD: &str = "115200";
/// Upper bound for an orderly shutdown before PID 1 invokes the kernel's
/// emergency reboot path.  S82dcentrald alone permits a 30-second typed
/// teardown, so the former 30-second global deadline could kill the machine
/// while the hardware owner was still producing its SafeOff evidence.
const SHUTDOWN_WATCHDOG_MS: u64 = 60_000;

fn main() {
    // Safety check: we MUST be PID 1
    let pid = unsafe { libc::getpid() };
    if pid != 1 {
        eprintln!(
            "dcentos-init: ERROR: must run as PID 1 (current PID={})",
            pid
        );
        eprintln!("dcentos-init: This binary is /sbin/init for DCENT_OS rootfs");
        std::process::exit(1);
    }

    // Set up signal handlers before anything else
    install_signal_handlers();

    // Create /dev/console early so we can print (kernel may have already done this)
    // On BraiinsOS 4.4.0-xilinx without devtmpfs, /dev might be empty
    let _ = ensure_console();

    // Version sourced from `[workspace.package]` in dcentrald/Cargo.toml.
    println!(
        "DCENTos Init v{} — D-Central Technologies",
        env!("CARGO_PKG_VERSION")
    );
    println!("PID 1 starting on BraiinsOS 4.4.0-xilinx kernel");

    // Phase 1: Mount essential virtual filesystems
    // /proc and /sys are needed by dcentos-early-init.sh and everything else
    println!("[init] Phase 1: Mounting virtual filesystems...");
    mount_virtual_fs();

    // Phase 2: Run early init script
    // This creates /dev nodes, mounts /tmp, /run, persistent storage, GPIO setup
    println!("[init] Phase 2: Running early init...");
    run_early_init();

    // Phase 3: Run S## init scripts in order
    println!("[init] Phase 3: Running init scripts...");
    run_init_scripts("start");

    // Phase 4: Spawn getty for serial console
    println!("[init] Phase 4: Spawning getty on {}...", GETTY_TTY);
    let mut getty_pid = spawn_getty();

    println!("[init] Boot complete. Entering main loop.");
    println!("[init] Console: {} @ {} baud", GETTY_TTY, GETTY_BAUD);

    // Main loop: reap zombies, respawn getty, handle shutdown
    loop {
        // Belt-and-suspenders for the classic "signal delivered just before a
        // blocking wait" race: arm a 1-second SIGALRM before blocking. Even if a
        // shutdown signal lands in the tiny window between the flag check at the
        // bottom of the loop and re-entering waitpid, the alarm guarantees
        // waitpid is interrupted (EINTR) within ~1s so SHUTDOWN_REQUESTED is
        // always observed promptly. On an idle unit (getty parked at login, no
        // child ever exiting) this is what keeps `reboot` from hanging.
        unsafe {
            libc::alarm(1);
        }

        // Wait for any child to exit (non-blocking would spin CPU, blocking is correct)
        let mut status: libc::c_int = 0;
        let waited = unsafe { libc::waitpid(-1, &mut status, 0) };

        // Disarm the alarm — we're past the blocking call.
        unsafe {
            libc::alarm(0);
        }

        if waited > 0 {
            // Check if it was getty that died — respawn it
            if waited == getty_pid {
                if !SHUTDOWN_REQUESTED.load(Ordering::Relaxed) {
                    // Respawn getty (like inittab ::respawn::)
                    getty_pid = spawn_getty();
                }
            }
            // Any other child: just reap (zombie prevention)
        }

        // Check for shutdown signal. With the non-restarting handlers installed
        // above, a `reboot`/`halt`/`poweroff` SIGTERM/SIGUSR1/SIGUSR2 interrupts
        // the blocked waitpid (EINTR -> waited == -1), so we fall straight here.
        if SHUTDOWN_REQUESTED.load(Ordering::Relaxed) {
            let sig = SHUTDOWN_SIGNAL.load(Ordering::Relaxed);
            // Map and arm the one terminal deadline before console output or any
            // stop path can block. From this point forward PID 1 owns escalation.
            let rb_action = shutdown_action_for_signal(sig);
            arm_emergency_watchdog(rb_action, SHUTDOWN_WATCHDOG_MS);
            println!(
                "[init] Shutdown requested (signal {} -> {})",
                sig,
                if rb_action == libc::RB_POWER_OFF {
                    "poweroff"
                } else {
                    "reboot"
                }
            );
            do_shutdown();
            // After shutdown scripts complete, perform the real kernel action.
            unsafe {
                libc::sync();
            }
            if rb_action == libc::RB_POWER_OFF {
                println!("[init] Powering off...");
            } else {
                println!("[init] Rebooting...");
            }
            unsafe {
                libc::reboot(rb_action);
            }
            // reboot(2) must not return; if it somehow does, force the kernel's
            // emergency path so we never strand a unit "up but asked to reboot".
            emergency_kernel_action(rb_action);
            // Last-ditch: loop forever (kernel will handle it).
            loop {
                unsafe {
                    libc::pause();
                }
            }
        }
    }
}

/// Map an incoming shutdown signal to the kernel `reboot(2)` action using the
/// BusyBox halt-applet convention:
///   `reboot`   -> SIGTERM  -> RB_AUTOBOOT
///   `halt`     -> SIGUSR1  -> RB_POWER_OFF (halt; we power off the rail)
///   `poweroff` -> SIGUSR2  -> RB_POWER_OFF
///   Ctrl+Alt+Del console -> SIGINT -> RB_AUTOBOOT (reboot)
///
/// Note the earlier code treated SIGUSR2 as *reboot*; BusyBox actually sends
/// SIGUSR2 for `poweroff`, so it now maps to power-off. SIGTERM (the
/// overwhelmingly common `reboot`/API path) correctly reboots either way.
fn shutdown_action_for_signal(sig: libc::c_int) -> libc::c_int {
    if sig == libc::SIGUSR1 || sig == libc::SIGUSR2 {
        libc::RB_POWER_OFF
    } else {
        // SIGTERM, SIGINT, and any unknown signal default to a reboot — the
        // safest default for a headless miner (a stuck "halt" is worse than a
        // reboot, and the API/`reboot` command path is always SIGTERM).
        libc::RB_AUTOBOOT
    }
}

/// Force the kernel's own reboot/poweroff path, bypassing any further userspace.
/// Used as a fallback if `reboot(2)` returns (it normally never does) or if the
/// orderly shutdown sequence stalls. This path deliberately performs no sync,
/// logging, allocation, receipt write, or lock acquisition before immediate
/// sysrq: it is the liveness escape from orderly reboot operations hanging.
fn emergency_kernel_action(rb_action: libc::c_int) {
    // The shutdown watchdog pre-enables sysrq before any stop script can block.
    // Trigger the immediate kernel path first: orderly reboot(2) can itself hang
    // in device shutdown or a reboot notifier.
    let trigger = if rb_action == libc::RB_POWER_OFF {
        b'o' // sysrq power-off
    } else {
        b'b' // sysrq immediate reboot
    };
    write_kernel_control_byte(b"/proc/sysrq-trigger\0", trigger);

    // If sysrq is unavailable or returned, try the raw syscall. No blocking
    // userspace work is permitted before this point.
    // SAFETY: PID 1 is privileged to invoke reboot(2), and rb_action is selected
    // only from libc's RB_AUTOBOOT/RB_POWER_OFF constants.
    unsafe {
        libc::reboot(rb_action);
    }
}

/// Best-effort one-byte write to a NUL-terminated procfs kernel-control path.
/// This is intentionally libc-only so the emergency path does not allocate.
fn write_kernel_control_byte(path: &'static [u8], byte: u8) {
    if path.last() != Some(&0) {
        return;
    }
    // SAFETY: `path` is a static NUL-terminated byte string; `byte` remains
    // alive for the write; every nonnegative descriptor is closed exactly once.
    unsafe {
        let fd = libc::open(path.as_ptr().cast(), libc::O_WRONLY);
        if fd >= 0 {
            let _ = libc::write(fd, (&byte as *const u8).cast(), 1);
            libc::close(fd);
        }
    }
}

/// Install signal handlers for graceful shutdown.
/// PID 1 does NOT get default signal behavior — signals are ignored unless
/// explicitly handled. This is a Linux kernel special case for init.
///
/// **Load-bearing: the handlers MUST be installed WITHOUT `SA_RESTART`** so the
/// blocking `waitpid(-1, …, 0)` in the main loop returns `EINTR` when a
/// shutdown signal arrives and the loop re-checks `SHUTDOWN_REQUESTED`. The
/// earlier revision used `libc::signal()`, whose musl/glibc BSD semantics set
/// `SA_RESTART` — so the interrupted `waitpid` was *auto-restarted by the
/// kernel* and silently blocked again. On an idle unit (getty parked at a login
/// prompt = no child ever exits) the shutdown flag was set but NEVER observed,
/// so `reboot`/`halt`/`poweroff` did nothing and the unit stayed up. That was
/// the live S9 bug: only `echo b > /proc/sysrq-trigger` (which bypasses
/// userspace init) could bring it down. Using `sigaction` with `sa_flags = 0`
/// (no `SA_RESTART`) is what makes `reboot` actually reboot.
fn install_signal_handlers() {
    // Build a sigaction with sa_flags = 0 (crucially NO SA_RESTART) so a
    // blocked syscall (waitpid) is interrupted with EINTR instead of being
    // transparently restarted by the kernel.
    install_one_handler(libc::SIGTERM, signal_handler); // `reboot`   -> SIGTERM
    install_one_handler(libc::SIGUSR1, signal_handler); // `halt`     -> SIGUSR1
    install_one_handler(libc::SIGUSR2, signal_handler); // `poweroff` -> SIGUSR2 (busybox)
    install_one_handler(libc::SIGINT, signal_handler); // Ctrl+Alt+Del console -> reboot

    // SIGALRM: a no-op handler whose ONLY job is to interrupt the blocking
    // waitpid (the per-iteration `alarm(1)` in the main loop). It must NOT set
    // the shutdown flag. Without an installed handler the default SIGALRM
    // disposition would *terminate* PID 1 (kernel panic), so this is mandatory.
    install_one_handler(libc::SIGALRM, alarm_handler);

    // Reap children manually with waitpid; default SIGCHLD disposition is fine.
    unsafe {
        libc::signal(libc::SIGCHLD, libc::SIG_DFL);
    }
}

/// Install a single interrupting (non-restarting) handler `handler` for `sig`.
fn install_one_handler(sig: libc::c_int, handler: extern "C" fn(libc::c_int)) {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        // sa_sigaction is a sighandler_t (pointer-sized). Our handlers have the
        // classic `extern "C" fn(c_int)` signature; with sa_flags lacking
        // SA_SIGINFO the kernel invokes them as plain handlers.
        sa.sa_sigaction = handler as *const () as libc::sighandler_t;
        // Empty mask + sa_flags = 0. Omitting SA_RESTART is the whole point:
        // it forces EINTR on the blocked waitpid so the shutdown is observed.
        libc::sigemptyset(&mut sa.sa_mask);
        sa.sa_flags = 0;
        libc::sigaction(sig, &sa, std::ptr::null_mut());
    }
}

extern "C" fn signal_handler(sig: libc::c_int) {
    SHUTDOWN_SIGNAL.store(sig, Ordering::Relaxed);
    SHUTDOWN_REQUESTED.store(true, Ordering::Relaxed);
}

/// No-op SIGALRM handler. Exists solely so the periodic `alarm(1)` interrupts
/// the blocking waitpid (closing the lost-signal race) — and so the default
/// "terminate PID 1" disposition never fires.
extern "C" fn alarm_handler(_sig: libc::c_int) {}

/// Ensure /dev/console exists so we can print to serial.
/// On BraiinsOS kernel without devtmpfs, /dev may be a bare directory.
fn ensure_console() -> io::Result<()> {
    if !Path::new(CONSOLE_DEV).exists() {
        // Try to create a minimal /dev with console
        // mount tmpfs on /dev first if /dev is read-only (squashfs)
        let _ = do_mount("tmpfs", "/dev", "tmpfs", 0, "size=512k,mode=0755");
        unsafe {
            let path = CString::new(CONSOLE_DEV).unwrap();
            libc::mknod(path.as_ptr(), libc::S_IFCHR | 0o600, libc::makedev(5, 1));
        }
    }
    Ok(())
}

/// Mount /proc, /sys, and ensure /dev exists as tmpfs.
/// dcentos-early-init.sh will do the detailed /dev setup (mknod for all devices),
/// but we need /proc and /sys mounted before it can run.
fn mount_virtual_fs() {
    // /proc — needed by early-init.sh and everything
    if !Path::new("/proc/self").exists() {
        match do_mount("proc", "/proc", "proc", 0, "") {
            Ok(()) => println!("  [OK] /proc mounted"),
            Err(e) => eprintln!("  [FAIL] /proc: {}", e),
        }
    } else {
        println!("  [OK] /proc already mounted");
    }

    // /sys — needed for sysfs device enumeration in early-init.sh
    if !Path::new("/sys/class").exists() {
        match do_mount("sysfs", "/sys", "sysfs", 0, "") {
            Ok(()) => println!("  [OK] /sys mounted"),
            Err(e) => eprintln!("  [FAIL] /sys: {}", e),
        }
    } else {
        println!("  [OK] /sys already mounted");
    }

    // /dev as tmpfs — early-init.sh expects to mount tmpfs on /dev and create nodes.
    // But if early-init.sh does it, we'd have a chicken-and-egg problem: we already
    // mounted tmpfs on /dev in ensure_console(). early-init.sh will mount OVER our
    // tmpfs (which is fine — mount stacking). Let it handle the detailed setup.
    // We just ensure /dev exists and has console for printing.
    if !Path::new("/dev/null").exists() {
        // /dev exists but is bare — create essential nodes so sh can run
        let nodes: &[(&str, u32, u32, u32)] = &[
            ("/dev/null", 0o666, 1, 3),
            ("/dev/zero", 0o666, 1, 5),
            ("/dev/tty", 0o666, 5, 0),
        ];
        for &(path, mode, major, minor) in nodes {
            if !Path::new(path).exists() {
                unsafe {
                    let cpath = CString::new(path).unwrap();
                    libc::mknod(
                        cpath.as_ptr(),
                        libc::S_IFCHR | mode,
                        libc::makedev(major, minor),
                    );
                }
            }
        }
    }
}

/// Run the early init script that sets up /dev, /tmp, /run, persistent storage,
/// GPIO pins, fan control, I2C, hostname, etc.
fn run_early_init() {
    if !Path::new(EARLY_INIT).exists() {
        eprintln!("  [WARN] {} not found — skipping early init", EARLY_INIT);
        // Fallback: do minimal /dev/tmpfs + /tmp + /run setup ourselves
        fallback_early_init();
        return;
    }

    // Find a working shell to execute the script
    let shell = find_shell();
    println!("  Using shell: {}", shell);

    let status = Command::new(&shell).arg(EARLY_INIT).status();

    match status {
        Ok(s) if s.success() => println!("  [OK] Early init complete"),
        Ok(s) => eprintln!("  [WARN] Early init exited with code {:?}", s.code()),
        Err(e) => {
            eprintln!("  [FAIL] Cannot run early init: {}", e);
            fallback_early_init();
        }
    }
}

/// Minimal fallback if early-init.sh is missing or fails.
/// Creates bare minimum for the system to be usable.
fn fallback_early_init() {
    println!("  [FALLBACK] Setting up minimal environment...");

    // Mount tmpfs on /dev if not already a tmpfs
    let _ = do_mount("tmpfs", "/dev", "tmpfs", 0, "size=512k,mode=0755");

    // Essential device nodes
    let nodes: &[(&str, u32, u32, u32)] = &[
        ("/dev/console", 0o600, 5, 1),
        ("/dev/null", 0o666, 1, 3),
        ("/dev/zero", 0o666, 1, 5),
        ("/dev/tty", 0o666, 5, 0),
        ("/dev/urandom", 0o444, 1, 9),
        ("/dev/random", 0o444, 1, 8),
        ("/dev/mem", 0o660, 1, 1),
        ("/dev/kmsg", 0o600, 1, 11),
    ];
    for &(path, mode, major, minor) in nodes {
        unsafe {
            let cpath = CString::new(path).unwrap();
            libc::mknod(
                cpath.as_ptr(),
                libc::S_IFCHR | mode,
                libc::makedev(major, minor),
            );
        }
    }
    let _ = fs::create_dir_all("/dev/pts");
    let _ = fs::create_dir_all("/dev/shm");
    let _ = do_mount("devpts", "/dev/pts", "devpts", 0, "gid=5,mode=620");

    // Symlinks
    let _ = std::os::unix::fs::symlink("/proc/self/fd", "/dev/fd");

    // /tmp and /run
    let _ = fs::create_dir_all("/tmp");
    let _ = do_mount("tmpfs", "/tmp", "tmpfs", 0, "size=64m,mode=1777");
    let _ = fs::create_dir_all("/run");
    let _ = do_mount("tmpfs", "/run", "tmpfs", 0, "size=1m,mode=0755");
    let _ = fs::create_dir_all("/run/lock");

    // Create ttyPS0 for serial console
    unsafe {
        let cpath = CString::new("/dev/ttyPS0").unwrap();
        libc::mknod(cpath.as_ptr(), libc::S_IFCHR | 0o660, libc::makedev(249, 0));
    }

    // Hostname
    unsafe {
        let name = CString::new("dcentos").unwrap();
        libc::sethostname(name.as_ptr(), 7);
    }

    println!("  [FALLBACK] Minimal environment ready");
}

/// Run all S## scripts in /etc/init.d/ in sorted order.
/// This mimics BusyBox init's `::sysinit:/etc/init.d/rcS` behavior.
fn run_init_scripts(action: &str) {
    let init_d = Path::new(INIT_D);
    if !init_d.is_dir() {
        eprintln!("  [WARN] {} not found — no init scripts to run", INIT_D);
        return;
    }

    // Read and sort entries
    let names: Vec<String> = match fs::read_dir(init_d) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect(),
        Err(e) => {
            eprintln!("  [FAIL] Cannot read {}: {}", INIT_D, e);
            return;
        }
    };
    // Select + order the S##-prefixed boot scripts via a pure, host-tested
    // helper. Shutdown ("stop") runs the reverse of the boot order. Only S##
    // scripts run here — there is NO K## pass (BusyBox runs K## only from a
    // separate rc level this init does not use), so the earlier "K## or S##"
    // comment was unreachable.
    let scripts = select_and_order_scripts(names, action);

    let shell = find_shell();

    for script in &scripts {
        let path = format!("{}/{}", INIT_D, script);

        // Check if executable
        if !is_executable(&path) {
            println!("  [SKIP] {} (not executable)", script);
            continue;
        }

        println!("  [RUN] {} {}", script, action);

        let status = Command::new(&shell).arg(&path).arg(action).status();

        match status {
            Ok(s) if s.success() => println!("  [OK] {}", script),
            Ok(s) => eprintln!("  [WARN] {} exited {:?}", script, s.code()),
            Err(e) => eprintln!("  [FAIL] {} error: {}", script, e),
        }
    }

    println!("  Init scripts {} complete", action);
}

/// Spawn the serial-console login terminal. Returns the child PID.
///
/// **Recovery console is load-bearing.** The UART console on `ttyPS0` is the
/// only escape hatch if a unit will not boot far enough for SSH (no network,
/// no dropbear, broken init script). It MUST present a working root prompt.
///
/// History (do not regress): an earlier revision spawned `getty` with the
/// argument vector `["-L", "ttyPS0", "115200", "vt100"]`. That is the
/// util-linux **agetty** order (`agetty [opts] TTY BAUD [TERM]`). But the
/// DCENT_OS / BraiinsOS images ship **BusyBox getty**, whose syntax is the
/// reverse — `getty [opts] BAUD TTY [TERM]`. Fed the agetty order, BusyBox
/// getty parses BAUD="ttyPS0" (invalid) and TTY="115200", opens the wrong /
/// nonexistent device, mangles the controlling-terminal setup, and the login
/// that follows **rejects the correct root password** and respawns in a loop.
/// The project's own `/etc/inittab` records the same lesson
/// ("getty -n -l broke silently") and switched to `/bin/login -f root` — but
/// `dcentos-init` is PID 1 and never reads inittab, so that fix never reached
/// the running console until now.
///
/// Strategy (most-reliable first):
///   1. `/bin/login -f root` — passwordless root, no getty arg-order pitfalls.
///      This matches the proven `/etc/inittab` + `/usr/sbin/autologin` intent
///      and is the canonical DCENT_OS recovery-console contract.
///   2. `getty` — only if login is unavailable. Argument order is chosen
///      PER BINARY (BusyBox vs util-linux agetty) so neither is ever fed the
///      other's syntax.
///   3. A bare login shell on the console — last resort so a stuck unit still
///      gets an interactive prompt.
fn spawn_getty() -> i32 {
    let tty_path = format!("/dev/{}", GETTY_TTY);

    // 1) Preferred: passwordless root login directly on the console.
    //    `login -f root` skips authentication entirely, so a corrupted or
    //    incompatible /etc/shadow hash can never lock the operator out of the
    //    recovery console. Both /bin/login (util-linux/busybox) and the
    //    overlay's /usr/sbin/autologin helper (`exec /bin/login -f root`)
    //    are accepted. login needs the tty wired to fd 0/1/2 as its
    //    controlling terminal, so we use the TTY-aware fork.
    for (prog, args) in &[
        ("/bin/login", &["-f", "root"][..]),
        ("/usr/sbin/autologin", &[][..]),
        ("/usr/bin/login", &["-f", "root"][..]),
    ] {
        if Path::new(prog).exists() {
            match fork_exec_with_tty(prog, args, &tty_path) {
                Ok(pid) => {
                    println!("  [OK] {} -f root on {} (PID {})", prog, GETTY_TTY, pid);
                    return pid;
                }
                Err(e) => eprintln!("  [WARN] {} failed: {}", prog, e),
            }
        }
    }

    // 2) Fallback: getty. Pass the argument order that matches the binary that
    //    is actually present — NEVER the wrong order (see history above).
    let getty_paths = ["/sbin/getty", "/bin/getty", "/usr/sbin/getty"];
    for getty in &getty_paths {
        if Path::new(getty).exists() {
            let args = getty_args(getty_is_busybox(getty));
            let argv: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            match fork_exec(getty, &argv) {
                Ok(pid) => {
                    println!("  [OK] getty spawned: {} {:?} (PID {})", getty, argv, pid);
                    return pid;
                }
                Err(e) => eprintln!("  [WARN] {} failed: {}", getty, e),
            }
        }
    }

    // BusyBox getty applet (busybox getty ...) — BusyBox order, always.
    let busybox_paths = ["/bin/busybox", "/usr/bin/busybox"];
    for bb in &busybox_paths {
        if Path::new(bb).exists() {
            let mut args = vec!["getty".to_string()];
            args.extend(getty_args(true)); // busybox order
            let argv: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            match fork_exec(bb, &argv) {
                Ok(pid) => {
                    println!("  [OK] busybox getty spawned: {:?} (PID {})", argv, pid);
                    return pid;
                }
                Err(e) => eprintln!("  [WARN] {} getty failed: {}", bb, e),
            }
        }
    }

    // 3) Last resort: a login shell directly on the console. This is an
    //    UNAUTHENTICATED root shell — acceptable only because it means every
    //    other console path is missing on the image, and a dead recovery
    //    console is worse than an open one on a DEV unit.
    let shell = find_shell();
    match fork_exec_with_tty(&shell, &["-l"], &tty_path) {
        Ok(pid) => {
            println!("  [OK] Recovery shell on {} (PID {})", GETTY_TTY, pid);
            pid
        }
        Err(e) => {
            eprintln!("  [FAIL] Cannot spawn login, getty, or shell: {}", e);
            -1
        }
    }
}

/// Decide whether a getty binary on disk is BusyBox getty or util-linux
/// agetty. BusyBox installs getty as a symlink (or hardlink) into the busybox
/// multi-call binary, so the resolved target name contains "busybox". A real
/// util-linux getty is `/sbin/agetty` (often with `/sbin/getty` symlinked to
/// it). When in doubt we treat it as agetty, because a util-linux getty is the
/// one that ships under the literal name `getty` in Buildroot images.
fn getty_is_busybox(path: &str) -> bool {
    // Resolve symlinks; if the resolved path mentions busybox it's BusyBox getty.
    if let Ok(real) = fs::read_link(path) {
        if real.to_string_lossy().contains("busybox") {
            return true;
        }
    }
    if let Ok(real) = fs::canonicalize(path) {
        if real.to_string_lossy().contains("busybox") {
            return true;
        }
    }
    false
}

/// Build the getty argument vector in the order the target binary expects.
///
/// - BusyBox getty:    `getty [opts] BAUD TTY [TERM]`  → `-L 115200 ttyPS0 vt100`
/// - util-linux agetty:`agetty [opts] TTY BAUD [TERM]` → `-L ttyPS0 115200 vt100`
///
/// `-L` = local line / do not require carrier-detect, correct for the Zynq
/// UART on both implementations. The TERM positional is `vt100` either way.
fn getty_args(is_busybox: bool) -> Vec<String> {
    if is_busybox {
        vec![
            "-L".to_string(),
            GETTY_BAUD.to_string(),
            GETTY_TTY.to_string(),
            "vt100".to_string(),
        ]
    } else {
        vec![
            "-L".to_string(),
            GETTY_TTY.to_string(),
            GETTY_BAUD.to_string(),
            "vt100".to_string(),
        ]
    }
}

/// Fork and exec a program with arguments. Returns child PID.
fn fork_exec(program: &str, args: &[&str]) -> io::Result<i32> {
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(io::Error::last_os_error());
    }
    if pid == 0 {
        // Child process
        // Create a new session (detach from init's controlling terminal)
        unsafe {
            libc::setsid();
        }

        let c_program = CString::new(program).unwrap();
        let mut c_args: Vec<CString> = Vec::new();
        c_args.push(CString::new(program).unwrap());
        for arg in args {
            c_args.push(CString::new(*arg).unwrap());
        }
        let c_argv: Vec<*const libc::c_char> = c_args
            .iter()
            .map(|a| a.as_ptr())
            .chain(std::iter::once(std::ptr::null()))
            .collect();

        unsafe {
            libc::execvp(c_program.as_ptr(), c_argv.as_ptr());
        }
        // If execvp returns, it failed
        eprintln!("dcentos-init: exec {} failed", program);
        unsafe {
            libc::_exit(127);
        }
    }
    Ok(pid)
}

/// Fork and exec with stdin/stdout/stderr redirected to a TTY device.
/// Used for spawning a shell on the serial console.
fn fork_exec_with_tty(program: &str, args: &[&str], tty: &str) -> io::Result<i32> {
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(io::Error::last_os_error());
    }
    if pid == 0 {
        // Child: create new session, open TTY as controlling terminal
        unsafe {
            libc::setsid();

            let c_tty = CString::new(tty).unwrap();
            let fd = libc::open(c_tty.as_ptr(), libc::O_RDWR);
            if fd >= 0 {
                // Set as controlling terminal
                libc::ioctl(fd, libc::TIOCSCTTY, 0);
                libc::dup2(fd, 0); // stdin
                libc::dup2(fd, 1); // stdout
                libc::dup2(fd, 2); // stderr
                if fd > 2 {
                    libc::close(fd);
                }
            }
        }

        let c_program = CString::new(program).unwrap();
        let mut c_args: Vec<CString> = Vec::new();
        c_args.push(CString::new(program).unwrap());
        for arg in args {
            c_args.push(CString::new(*arg).unwrap());
        }
        let c_argv: Vec<*const libc::c_char> = c_args
            .iter()
            .map(|a| a.as_ptr())
            .chain(std::iter::once(std::ptr::null()))
            .collect();

        unsafe {
            libc::execvp(c_program.as_ptr(), c_argv.as_ptr());
        }
        eprintln!("dcentos-init: exec {} failed", program);
        unsafe {
            libc::_exit(127);
        }
    }
    Ok(pid)
}

/// Graceful shutdown: run init scripts with "stop", sync, unmount.
///
/// The caller arms the emergency watchdog before entering this function or
/// performing any shutdown logging. Its deadline includes service-specific
/// stop, residual-process cleanup, and filesystem teardown.
fn do_shutdown() {
    println!("[init] Running shutdown sequence...");

    // Give each service its typed shutdown path before the global kill sweep.
    // In particular, dcentrald must fence API mutations, join its mining
    // owner, obtain checked GPIO-gate command/readback, and close its watchdog while it is
    // still alive.  The old order SIGTERM'd everything, waited only 3 seconds,
    // SIGKILL'd survivors, and *then* called S82dcentrald stop, making the stop
    // script incapable of obtaining any in-process hardware disposition.
    println!("[init] Running stop scripts...");
    run_init_scripts("stop");

    // Terminate only processes left behind after service-specific teardown.
    println!("[init] Sending SIGTERM to remaining processes...");
    unsafe {
        libc::kill(-1, libc::SIGTERM);
    }
    // Wait a few seconds for residual processes to exit gracefully.
    sleep_ms(3000);

    println!("[init] Sending SIGKILL to remaining processes...");
    unsafe {
        libc::kill(-1, libc::SIGKILL);
    }
    sleep_ms(1000);

    // Save entropy seed if possible
    println!("[init] Syncing filesystems...");
    unsafe {
        libc::sync();
    }

    // Unmount filesystems
    println!("[init] Unmounting filesystems...");
    unmount_all();

    unsafe {
        libc::sync();
    }
    println!("[init] Shutdown complete.");
}

/// Spawn a detached watchdog thread that forces the kernel reboot/poweroff
/// action after one absolute monotonic deadline if the orderly shutdown
/// sequence has not already reached `reboot(2)`. EINTR retries reuse the same
/// deadline, so signals can neither fire the watchdog early nor extend it.
fn arm_emergency_watchdog(rb_action: libc::c_int, deadline_ms: u64) {
    let deadline = monotonic_deadline_after(deadline_ms);
    let fallback_deadline = Instant::now() + Duration::from_millis(deadline_ms);
    let watchdog = std::thread::Builder::new().spawn(move || {
        let absolute_wait_completed = deadline.map(sleep_until_monotonic).unwrap_or(false);
        if !absolute_wait_completed {
            std::thread::sleep(fallback_deadline.saturating_duration_since(Instant::now()));
        }
        // Nothing that can block on logging, storage, locks, or allocation may
        // precede the emergency kernel action after the deadline is reached.
        emergency_kernel_action(rb_action);
    });
    if watchdog.is_err() {
        // No deadline exists if thread creation failed. Do not log, allocate, or
        // enter the unbounded orderly path while falsely claiming otherwise.
        emergency_kernel_action(rb_action);
        return;
    }
    // The deadline thread now exists. Prepare immediate sysrq before generic
    // stop scripts can block; even if this best-effort procfs write misbehaves,
    // the watchdog can still reach its raw reboot(2) fallback.
    write_kernel_control_byte(b"/proc/sys/kernel/sysrq\0", b'1');
}

/// Unmount all filesystems in reverse order (except /, /proc, /sys, /dev).
fn unmount_all() {
    // Read /proc/mounts and unmount in reverse order
    if let Ok(mounts) = fs::read_to_string("/proc/mounts") {
        let mut mount_points: Vec<&str> = mounts
            .lines()
            .filter_map(|line| line.split_whitespace().nth(1))
            .filter(|mp| !matches!(*mp, "/" | "/proc" | "/sys" | "/dev"))
            .collect();
        mount_points.reverse();

        for mp in mount_points {
            let cmp = match CString::new(mp) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let rc = unsafe { libc::umount2(cmp.as_ptr(), 0) };
            if rc == 0 {
                println!("  Unmounted {}", mp);
            }
            // Errors are fine — some mounts can't be unmounted (bind mounts, etc.)
        }
    }

    // Final: remount root read-only
    let root = CString::new("/").unwrap();
    let empty = CString::new("").unwrap();
    unsafe {
        libc::mount(
            std::ptr::null(),
            root.as_ptr(),
            std::ptr::null(),
            libc::MS_REMOUNT | libc::MS_RDONLY,
            empty.as_ptr() as *const libc::c_void,
        );
    }
}

/// Find a working shell on the system.
fn find_shell() -> String {
    let shells = ["/bin/sh", "/bin/bash", "/bin/ash", "/usr/bin/sh"];
    for shell in &shells {
        if Path::new(shell).exists() {
            return shell.to_string();
        }
    }
    // Last resort: busybox sh
    if Path::new("/bin/busybox").exists() {
        return "/bin/busybox".to_string();
    }
    "/bin/sh".to_string()
}

/// Check if a file is executable.
fn is_executable(path: &str) -> bool {
    let cpath = match CString::new(path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    unsafe { libc::access(cpath.as_ptr(), libc::X_OK) == 0 }
}

/// Mount a filesystem. Wraps the mount(2) syscall.
fn do_mount(
    source: &str,
    target: &str,
    fstype: &str,
    flags: libc::c_ulong,
    data: &str,
) -> io::Result<()> {
    // Ensure mount point exists
    let _ = fs::create_dir_all(target);

    let c_source = CString::new(source)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bad source"))?;
    let c_target = CString::new(target)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bad target"))?;
    let c_fstype = CString::new(fstype)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bad fstype"))?;
    let c_data =
        CString::new(data).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bad data"))?;

    let rc = unsafe {
        libc::mount(
            c_source.as_ptr(),
            c_target.as_ptr(),
            c_fstype.as_ptr(),
            flags,
            if data.is_empty() {
                std::ptr::null()
            } else {
                c_data.as_ptr() as *const libc::c_void
            },
        )
    };

    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Compute one normalized absolute CLOCK_MONOTONIC deadline.
fn monotonic_deadline_after(ms: u64) -> Option<libc::timespec> {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` points to initialized writable storage for clock_gettime.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) } != 0 {
        return None;
    }
    add_millis_to_timespec(now, ms)
}

/// Pure normalized timespec arithmetic, split out for boundary tests.
fn add_millis_to_timespec(now: libc::timespec, ms: u64) -> Option<libc::timespec> {
    // Use the smallest shipped tv_sec width as the portable input bound. This
    // avoids a target-dependent libc::time_t alias while compiling correctly
    // for both 32-bit ARM and 64-bit AArch64 musl.
    let seconds = i32::try_from(ms / 1000).ok()?;
    let nanos = (ms % 1000) * 1_000_000;
    let total_nanos = now.tv_nsec as u64 + nanos;
    let carry = i32::try_from(total_nanos / 1_000_000_000).ok()?;
    Some(libc::timespec {
        tv_sec: now
            .tv_sec
            .saturating_add(seconds.into())
            .saturating_add(carry.into()),
        tv_nsec: (total_nanos % 1_000_000_000) as libc::c_long,
    })
}

/// Retry an absolute wait only for EINTR. The closure seam lets unit tests
/// inject repeated signals without sleeping or touching the host clock.
fn retry_absolute_wait(mut wait: impl FnMut() -> libc::c_int) -> libc::c_int {
    loop {
        let result = wait();
        if result == libc::EINTR {
            continue;
        }
        return result;
    }
}

fn sleep_until_monotonic(deadline: libc::timespec) -> bool {
    retry_absolute_wait(|| {
        // SAFETY: `deadline` is a normalized initialized timespec. Linux
        // clock_nanosleep returns an errno value directly and does not mutate it
        // when TIMER_ABSTIME is used.
        unsafe {
            libc::clock_nanosleep(
                libc::CLOCK_MONOTONIC,
                libc::TIMER_ABSTIME,
                &deadline,
                std::ptr::null_mut(),
            )
        }
    }) == 0
}

/// Sleep for a given number of milliseconds against an absolute monotonic
/// deadline. The relative nanosleep fallback still retries EINTR and therefore
/// cannot return early if CLOCK_MONOTONIC acquisition unexpectedly fails.
fn sleep_ms(ms: u64) {
    if let Some(deadline) = monotonic_deadline_after(ms) {
        if sleep_until_monotonic(deadline) {
            return;
        }
    }

    relative_sleep_ms(ms);
}

fn relative_sleep_ms(ms: u64) {
    let mut remaining = libc::timespec {
        tv_sec: (ms / 1000) as _,
        tv_nsec: ((ms % 1000) * 1_000_000) as libc::c_long,
    };
    loop {
        let requested = remaining;
        // SAFETY: both pointers reference initialized writable timespec storage.
        let result = unsafe { libc::nanosleep(&requested, &mut remaining) };
        if result == 0 {
            return;
        }
        if io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return;
        }
    }
}

/// Select and order the SysV-style boot scripts found in `/etc/init.d`.
///
/// A name qualifies iff it starts with `S` and is at least 2 chars (`S` + at
/// least one more), so standard `S##name` scripts AND bare numbered scripts
/// like `S01`/`S05`/`S99` all run. The prior `name.len() > 3` filter silently
/// dropped valid <=3-char scripts (`S01`, `S05`) from BOTH the boot and shutdown
/// passes with no log line — a deterministic-startup hazard if a future overlay
/// ships a short recovery hook. This `>= 2` bound is intentionally permissive
/// and PURELY ADDITIVE versus the old filter (it never excludes anything the old
/// one ran); non-scripts that slip through (e.g. `Sfoo.bak`) are still skipped
/// downstream by the executable check, so the prefilter need not be stricter.
/// Boot order is lexical (the zero-padded `S##` prefix sorts numerically);
/// shutdown ("stop") runs the reverse. Only `S##` scripts run — there is no
/// `K##` pass. (gap-swarm no-HAL hunt #6)
fn select_and_order_scripts(mut names: Vec<String>, action: &str) -> Vec<String> {
    names.retain(|name| name.starts_with('S') && name.len() >= 2);
    names.sort();
    if action == "stop" {
        names.reverse();
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_and_order_scripts_includes_short_and_orders_numerically() {
        let names = vec![
            "S99upgrade".to_string(),
            "S05".to_string(), // bare short script — the old len()>3 filter DROPPED this
            "S37board_setup".to_string(),
            "S82dcentrald".to_string(),
            "S01".to_string(),    // bare short script — dropped by the old filter
            "rcS".to_string(),    // not S-prefixed → excluded
            "README".to_string(), // not S-prefixed → excluded
            "S".to_string(),      // len 1 → excluded
        ];
        let start = select_and_order_scripts(names.clone(), "start");
        assert_eq!(
            start,
            vec![
                "S01".to_string(),
                "S05".to_string(),
                "S37board_setup".to_string(),
                "S82dcentrald".to_string(),
                "S99upgrade".to_string(),
            ],
            "boot pass must include bare S01/S05 (the len()>3 bug) + order numerically; exclude non-S and bare 'S'"
        );
        // Shutdown runs the exact reverse of the boot order.
        let stop = select_and_order_scripts(names, "stop");
        let mut expected_stop = start.clone();
        expected_stop.reverse();
        assert_eq!(stop, expected_stop);
    }

    // --- Serial-console getty argument-order regression -------------------
    //
    // The serial root console was broken because dcentos-init spawned BusyBox
    // getty with the util-linux *agetty* argument order (TTY before BAUD).
    // BusyBox getty wants BAUD before TTY, so it opened the wrong device and
    // the login that followed rejected the correct root password. These tests
    // pin the per-binary argument order so the two syntaxes can never be
    // swapped again.

    #[test]
    fn getty_args_busybox_order_is_baud_then_tty() {
        // BusyBox: `getty [opts] BAUD TTY [TERM]`
        let args = getty_args(true);
        assert_eq!(
            args,
            vec![
                "-L".to_string(),
                GETTY_BAUD.to_string(), // 115200 FIRST
                GETTY_TTY.to_string(),  // ttyPS0 SECOND
                "vt100".to_string(),
            ],
            "BusyBox getty must receive BAUD before TTY"
        );
        // The positional after -L must be the numeric baud, never the device.
        assert_eq!(args[1], "115200");
        assert_eq!(args[2], "ttyPS0");
    }

    #[test]
    fn getty_args_agetty_order_is_tty_then_baud() {
        // util-linux agetty: `agetty [opts] TTY BAUD [TERM]`
        let args = getty_args(false);
        assert_eq!(
            args,
            vec![
                "-L".to_string(),
                GETTY_TTY.to_string(),  // ttyPS0 FIRST
                GETTY_BAUD.to_string(), // 115200 SECOND
                "vt100".to_string(),
            ],
            "util-linux agetty must receive TTY before BAUD"
        );
        assert_eq!(args[1], "ttyPS0");
        assert_eq!(args[2], "115200");
    }

    #[test]
    fn getty_args_orders_are_mutually_distinct() {
        // The whole point: the two orders must differ. A regression that fed
        // the same vector to both binaries (the original bug) would make these
        // equal again.
        assert_ne!(
            getty_args(true),
            getty_args(false),
            "BusyBox and agetty argument orders must never collapse to the same vector"
        );
    }

    #[test]
    fn getty_is_busybox_false_for_nonexistent_path() {
        // A path that doesn't resolve to a busybox symlink is treated as
        // agetty (the conservative default). A bare nonexistent path has no
        // symlink target, so it is NOT classified as busybox.
        assert!(!getty_is_busybox("/no/such/getty/binary"));
    }

    // --- BusyBox halt-applet signal -> reboot(2) action mapping --------------
    //
    // The live S9 bug had two parts: (1) handlers were installed with
    // SA_RESTART so the shutdown was never observed (covered by code review +
    // the sigaction rewrite), and (2) the signal->action map mishandled
    // BusyBox's `poweroff` (SIGUSR2). These tests pin the BusyBox convention so
    // a future edit can't silently flip `reboot` to a power-off or vice versa.

    #[test]
    fn sigterm_reboots() {
        // `reboot` (and POST /api/action/reboot -> Command "reboot") -> SIGTERM.
        // This is the overwhelmingly common path and MUST reboot, never halt.
        assert_eq!(shutdown_action_for_signal(libc::SIGTERM), libc::RB_AUTOBOOT);
    }

    #[test]
    fn sigint_reboots() {
        // Console Ctrl+Alt+Del -> SIGINT -> reboot.
        assert_eq!(shutdown_action_for_signal(libc::SIGINT), libc::RB_AUTOBOOT);
    }

    #[test]
    fn sigusr1_halts() {
        // BusyBox `halt` -> SIGUSR1 -> power off the rail.
        assert_eq!(
            shutdown_action_for_signal(libc::SIGUSR1),
            libc::RB_POWER_OFF
        );
    }

    #[test]
    fn sigusr2_powers_off_not_reboots() {
        // BusyBox `poweroff` -> SIGUSR2. The earlier code treated SIGUSR2 as a
        // REBOOT; that was wrong. It must power off.
        assert_eq!(
            shutdown_action_for_signal(libc::SIGUSR2),
            libc::RB_POWER_OFF
        );
    }

    #[test]
    fn unknown_signal_defaults_to_reboot() {
        // Any unexpected signal defaults to reboot — for a headless miner a
        // stray reboot is far safer than a stuck "halt".
        assert_eq!(shutdown_action_for_signal(libc::SIGHUP), libc::RB_AUTOBOOT);
    }

    #[test]
    fn shutdown_runs_service_teardown_before_global_kill_sweep() {
        let source = include_str!("main.rs");
        let shutdown = source
            .split_once("fn do_shutdown")
            .expect("do_shutdown definition")
            .1
            .split_once("fn arm_emergency_watchdog")
            .expect("bounded do_shutdown body")
            .0;
        let stop = shutdown
            .find("run_init_scripts(\"stop\");")
            .expect("service stop pass");
        let global_term = shutdown
            .find("libc::kill(-1, libc::SIGTERM)")
            .expect("residual-process SIGTERM sweep");
        let global_kill = shutdown
            .find("libc::kill(-1, libc::SIGKILL)")
            .expect("residual-process SIGKILL sweep");

        assert!(
            stop < global_term && global_term < global_kill,
            "typed service shutdown must precede TERM/KILL of residual processes"
        );
    }

    #[test]
    fn shutdown_watchdog_exceeds_daemon_typed_teardown_budget() {
        assert!(
            SHUTDOWN_WATCHDOG_MS > 36_000,
            "PID 1 must exceed S82's 30s TERM, 5s death check, and 1s receipt retry budget"
        );
    }

    #[test]
    fn monotonic_deadline_arithmetic_normalizes_nanosecond_carry() {
        let deadline = add_millis_to_timespec(
            libc::timespec {
                tv_sec: 10,
                tv_nsec: 900_000_000,
            },
            2_500,
        )
        .expect("2.5 seconds is representable on every shipped target");
        assert_eq!(deadline.tv_sec, 13);
        assert_eq!(deadline.tv_nsec, 400_000_000);
    }

    #[test]
    fn absolute_deadline_wait_retries_every_eintr() {
        let mut outcomes = [libc::EINTR, libc::EINTR, 0].into_iter();
        let mut calls = 0;
        let result = retry_absolute_wait(|| {
            calls += 1;
            outcomes.next().unwrap_or(0)
        });
        assert_eq!(result, 0);
        assert_eq!(calls, 3);
    }

    #[test]
    fn emergency_path_has_no_blocking_userspace_work_before_reboot() {
        let source = include_str!("main.rs");
        let emergency = source
            .split_once("fn emergency_kernel_action")
            .expect("emergency kernel action")
            .1
            .split_once("fn install_signal_handlers")
            .expect("bounded emergency section")
            .0;

        let immediate_sysrq = emergency
            .find("write_kernel_control_byte(b\"/proc/sysrq-trigger")
            .expect("immediate sysrq is the first terminal action");
        let reboot_fallback = emergency
            .find("libc::reboot(rb_action)")
            .expect("raw reboot fallback remains available");
        assert!(immediate_sysrq < reboot_fallback);
        assert!(!emergency.contains("libc::sync"));
        assert!(!emergency.contains("fs::write"));
        assert!(!emergency.contains("println!"));
        assert!(!emergency.contains("eprintln!"));

        let watchdog = source
            .split_once("fn arm_emergency_watchdog")
            .expect("watchdog definition")
            .1
            .split_once("fn unmount_all")
            .expect("bounded watchdog section")
            .0;
        let spawn = watchdog
            .find(".spawn(move ||")
            .expect("deadline thread is created");
        let enable_sysrq = watchdog
            .find("write_kernel_control_byte(b\"/proc/sys/kernel/sysrq")
            .expect("sysrq preparation remains available");
        assert!(spawn < enable_sysrq);
        assert!(watchdog.contains("sleep_until_monotonic"));
        assert!(watchdog.contains("fallback_deadline.saturating_duration_since"));
        assert!(!watchdog.contains("eprintln!"));

        let shutdown_entry = source
            .split_once("if SHUTDOWN_REQUESTED.load")
            .expect("main shutdown branch")
            .1
            .split_once("fn shutdown_action_for_signal")
            .expect("bounded main shutdown branch")
            .0;
        let arm = shutdown_entry
            .find("arm_emergency_watchdog")
            .expect("deadline armed from main");
        let log = shutdown_entry
            .find("println!(")
            .expect("shutdown request log");
        let orderly = shutdown_entry
            .find("do_shutdown();")
            .expect("orderly shutdown");
        assert!(arm < log && log < orderly);
    }
}
