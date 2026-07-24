//! `pic-recovery` -- fail-closed controller inspection for Antminer S9 boards.
//!
//! This binary deliberately contains no erase, program, reset, jump, voltage,
//! FPGA-swap, or rail-control operation. Historical versions mixed multiple
//! PIC/dsPIC families behind caller-selected buses and addresses, accepted
//! unbound confirmation flags as authority, and could report success after
//! partial writes. Those mutation paths are removed until recovery is backed
//! by discovered endpoint identity, a manifested artifact, a target-bound
//! single-use permit, readback verification, and fault-injection tests.
//!
//! The only remaining hardware operation is an exact one-byte read from the
//! three documented S9 PIC16F1704 addresses. Inspection requires the image's
//! authoritative `am1-s9` board marker and refuses to run while `dcentrald` is
//! alive. It uses `I2C_SLAVE`, never `I2C_SLAVE_FORCE`, and emits no I2C write.

use std::env;
use std::fs;
use std::io;
use std::os::unix::io::RawFd;
use std::path::Path;
use std::process::ExitCode;

use dcentrald_fabric_lease::{I2cLeasePurpose, OsI2cFabricLease, PhysicalI2cFabricId};

const BOARD_TARGET_PATH: &str = "/etc/dcentos/board_target";
const SUBTYPE_PATH: &str = "/etc/subtype";
const I2C_PATH: &str = "/dev/i2c-0";
const I2C_SLAVE: u64 = 0x0703;
const PIC_ADDRS: [(u8, &str); 3] = [(0x55, "Chain 6"), (0x56, "Chain 7"), (0x57, "Chain 8")];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct S9PicAddress(u8);

impl S9PicAddress {
    fn parse(value: &str) -> Result<Self, String> {
        let digits = value
            .strip_prefix("0x")
            .or_else(|| value.strip_prefix("0X"))
            .unwrap_or(value);
        let address = u8::from_str_radix(digits, 16)
            .map_err(|error| format!("invalid S9 PIC address {value:?}: {error}"))?;
        if PIC_ADDRS.iter().any(|(known, _)| *known == address) {
            Ok(Self(address))
        } else {
            Err(format!(
                "unsupported S9 PIC address 0x{address:02X}; expected exactly 0x55, 0x56, or 0x57"
            ))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetSelection {
    AllKnown,
    One(S9PicAddress),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InspectCommand {
    targets: TargetSelection,
    verbose: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Inspect(InspectCommand),
    Help,
}

fn take_value(arguments: &[String], index: &mut usize, option: &str) -> Result<String, String> {
    *index += 1;
    arguments
        .get(*index)
        .filter(|value| !value.starts_with('-'))
        .cloned()
        .ok_or_else(|| format!("{option} requires a value"))
}

fn parse_inspect(arguments: &[String]) -> Result<Command, String> {
    let mut all = false;
    let mut address = None;
    let mut platform = None;
    let mut verbose = false;
    let mut index = 0;

    while let Some(argument) = arguments.get(index) {
        match argument.as_str() {
            "--all" if !all => all = true,
            "--all" => return Err("duplicate --all".into()),
            "--addr" if address.is_none() => {
                address = Some(S9PicAddress::parse(&take_value(
                    arguments, &mut index, "--addr",
                )?)?);
            }
            "--addr" => return Err("duplicate --addr".into()),
            "--platform" if platform.is_none() => {
                platform = Some(take_value(arguments, &mut index, "--platform")?);
            }
            "--platform" => return Err("duplicate --platform".into()),
            "--verbose" | "-v" if !verbose => verbose = true,
            "--verbose" | "-v" => return Err("duplicate --verbose".into()),
            option => return Err(format!("unknown pic16-inspect option {option:?}")),
        }
        index += 1;
    }

    if all && address.is_some() {
        return Err("--all and --addr are mutually exclusive".into());
    }
    if platform.as_deref() != Some("am1-s9") {
        return Err("pic16-inspect requires exact --platform am1-s9".into());
    }

    Ok(Command::Inspect(InspectCommand {
        targets: address.map_or(TargetSelection::AllKnown, TargetSelection::One),
        verbose,
    }))
}

fn parse_command(arguments: &[String]) -> Result<Command, String> {
    match arguments {
        [] => Err("missing command; hardware access is never implicit".into()),
        [flag] if matches!(flag.as_str(), "-h" | "--help" | "help") => Ok(Command::Help),
        [command, rest @ ..] if command == "pic16-inspect" => parse_inspect(rest),
        [command, ..]
            if command == "pic16-recover"
                || command == "--fpga-flash"
                || command.starts_with("pic1704-")
                || command.starts_with("dspic-") =>
        {
            Err(format!(
                "{command} is disabled: no mutation executor may run until endpoint identity, artifact identity, target-bound authority, readback verification, and fault-injection evidence are implemented"
            ))
        }
        [command, ..] => Err(format!(
            "unknown command {command:?}; only pic16-inspect is available"
        )),
    }
}

fn validate_s9_identity(board_target: &str, subtype: Option<&str>) -> Result<(), String> {
    if board_target.trim() != "am1-s9" {
        return Err(format!(
            "refusing S9 PIC access: {BOARD_TARGET_PATH} must contain exact am1-s9, got {:?}",
            board_target.trim()
        ));
    }

    if let Some(subtype) = subtype.map(str::trim).filter(|value| !value.is_empty()) {
        let normalized = subtype.to_ascii_uppercase();
        if !matches!(normalized.as_str(), "S9" | "S9J" | "S9K" | "S9_BHB09001") {
            return Err(format!(
                "refusing S9 PIC access: {SUBTYPE_PATH} contradicts am1-s9 with {subtype:?}"
            ));
        }
    }
    Ok(())
}

fn validate_system_identity() -> Result<(), String> {
    let board_target = fs::read_to_string(BOARD_TARGET_PATH)
        .map_err(|error| format!("cannot read authoritative {BOARD_TARGET_PATH}: {error}"))?;
    let subtype = match fs::read_to_string(SUBTYPE_PATH) {
        Ok(value) => Some(value),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("cannot read {SUBTYPE_PATH}: {error}")),
    };
    validate_s9_identity(&board_target, subtype.as_deref())
}

fn dcentrald_processes(proc_root: &Path) -> io::Result<Vec<u32>> {
    let mut matches = Vec::new();
    for entry in fs::read_dir(proc_root)? {
        let entry = entry?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let comm = fs::read_to_string(entry.path().join("comm")).unwrap_or_default();
        let cmdline = fs::read(entry.path().join("cmdline")).unwrap_or_default();
        let executable = cmdline.split(|byte| *byte == 0).next().unwrap_or_default();
        let executable_name = Path::new(std::str::from_utf8(executable).unwrap_or_default())
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if comm.trim() == "dcentrald" || executable_name == "dcentrald" {
            matches.push(pid);
        }
    }
    matches.sort_unstable();
    Ok(matches)
}

fn require_exclusive_i2c_ownership() -> Result<(), String> {
    let processes = dcentrald_processes(Path::new("/proc"))
        .map_err(|error| format!("cannot prove I2C exclusivity from /proc: {error}"))?;
    if processes.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "refusing standalone I2C access while dcentrald is alive (pid(s) {}); stop the daemon first",
            processes
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}

struct I2cBus {
    _fabric_lease: OsI2cFabricLease,
    fd: RawFd,
}

impl I2cBus {
    fn open() -> io::Result<Self> {
        // This lease closes the /proc-scan-to-device-open race with every
        // cooperating DCENT_OS daemon/tool. It must precede even the read-only
        // device open because selecting and reading a slave consumes the whole
        // physical adapter.
        let fabric_lease = OsI2cFabricLease::acquire(
            PhysicalI2cFabricId::linux_adapter(0),
            I2cLeasePurpose::RecoveryInspection,
        )
        .map_err(|error| error.into_io_error())?;
        let path = b"/dev/i2c-0\0";
        let fd = unsafe { libc::open(path.as_ptr().cast(), libc::O_RDONLY | libc::O_CLOEXEC) };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self {
                _fabric_lease: fabric_lease,
                fd,
            })
        }
    }

    fn read_exact_byte(&self, address: S9PicAddress) -> io::Result<u8> {
        self._fabric_lease
            .validate_current_process()
            .map_err(|error| error.into_io_error())?;
        let selected = unsafe {
            #[cfg(target_env = "musl")]
            {
                libc::ioctl(
                    self.fd,
                    I2C_SLAVE as libc::c_int,
                    address.0 as libc::c_ulong,
                )
            }
            #[cfg(not(target_env = "musl"))]
            {
                libc::ioctl(
                    self.fd,
                    I2C_SLAVE as libc::c_ulong,
                    address.0 as libc::c_ulong,
                )
            }
        };
        if selected < 0 {
            return Err(io::Error::last_os_error());
        }

        let mut byte = 0_u8;
        let read = unsafe { libc::read(self.fd, (&mut byte as *mut u8).cast(), 1) };
        match read {
            1 => Ok(byte),
            0 => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "I2C read returned zero bytes",
            )),
            value if value < 0 => Err(io::Error::last_os_error()),
            value => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("one-byte I2C read returned impossible length {value}"),
            )),
        }
    }
}

impl Drop for I2cBus {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.fd);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PicState {
    Bootloader,
    Application(u8),
    Silent,
    Unknown(u8),
}

fn classify_state(byte: u8) -> PicState {
    match byte {
        0xcc => PicState::Bootloader,
        0x03 | 0x60 => PicState::Application(byte),
        0xff => PicState::Silent,
        value => PicState::Unknown(value),
    }
}

fn selected_targets(selection: TargetSelection) -> Vec<(S9PicAddress, &'static str)> {
    PIC_ADDRS
        .iter()
        .filter(|(address, _)| match selection {
            TargetSelection::AllKnown => true,
            TargetSelection::One(selected) => *address == selected.0,
        })
        .map(|(address, name)| (S9PicAddress(*address), *name))
        .collect()
}

fn inspect(command: InspectCommand) -> Result<(), String> {
    validate_system_identity()?;
    require_exclusive_i2c_ownership()?;
    let bus = I2cBus::open().map_err(|error| format!("cannot open {I2C_PATH}: {error}"))?;
    let mut inconclusive = Vec::new();

    println!("PIC16F1704 read-only inspection (no I2C writes)");
    for (address, name) in selected_targets(command.targets) {
        match bus.read_exact_byte(address) {
            Ok(raw) => {
                let state = classify_state(raw);
                println!("  0x{:02X} {name}: {state:?}", address.0);
                if command.verbose {
                    println!("    exact raw byte: 0x{raw:02X}");
                }
                if matches!(state, PicState::Silent | PicState::Unknown(_)) {
                    inconclusive.push(format!("0x{:02X}={state:?}", address.0));
                }
            }
            Err(error) => {
                eprintln!("  0x{:02X} {name}: transport error: {error}", address.0);
                inconclusive.push(format!("0x{:02X}=transport-error", address.0));
            }
        }
    }

    if inconclusive.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "inspection was inconclusive for {}; no recovery capability was issued",
            inconclusive.join(", ")
        ))
    }
}

fn print_usage() {
    eprintln!("Usage:");
    eprintln!("  pic-recovery pic16-inspect (--all | --addr 0x55|0x56|0x57) \\");
    eprintln!("      --platform am1-s9 [--verbose]");
    eprintln!();
    eprintln!("All mutation routes are disabled. Physical ICSP is the deterministic");
    eprintln!("recovery path until a controller-specific executor satisfies the");
    eprintln!("documented recovery-authority contract.");
}

fn main() -> ExitCode {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    let command = match parse_command(&arguments) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("error: {error}");
            print_usage();
            return ExitCode::from(2);
        }
    };

    match command {
        Command::Help => {
            print_usage();
            ExitCode::SUCCESS
        }
        Command::Inspect(command) => match inspect(command) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::from(1)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn inspection_requires_explicit_platform_and_known_target() {
        assert!(matches!(
            parse_command(&args(&[
                "pic16-inspect",
                "--addr",
                "0x56",
                "--platform",
                "am1-s9"
            ])),
            Ok(Command::Inspect(InspectCommand {
                targets: TargetSelection::One(S9PicAddress(0x56)),
                verbose: false,
            }))
        ));
        assert!(parse_command(&args(&["pic16-inspect", "--all"])).is_err());
        assert!(parse_command(&args(&[
            "pic16-inspect",
            "--addr",
            "0x20",
            "--platform",
            "am1-s9"
        ]))
        .is_err());
    }

    #[test]
    fn every_historical_mutation_family_is_refused() {
        for command in [
            "pic16-recover",
            "--fpga-flash",
            "pic1704-reflash-stock",
            "pic1704-erase",
            "dspic-reflash",
        ] {
            let error = parse_command(&args(&[command, "--confirm-bricked"]))
                .expect_err("mutation command must be absent");
            assert!(error.contains("disabled"), "{command}: {error}");
        }
    }

    #[test]
    fn hardware_access_is_never_implicit() {
        assert!(parse_command(&[]).is_err());
        assert!(parse_command(&args(&["--all"])).is_err());
    }

    #[test]
    fn identity_requires_exact_board_target_and_noncontradictory_subtype() {
        for subtype in [None, Some("S9"), Some("s9j"), Some("S9_BHB09001")] {
            assert!(validate_s9_identity("am1-s9\n", subtype).is_ok());
        }
        assert!(validate_s9_identity("am1-s9-extra", Some("S9")).is_err());
        assert!(validate_s9_identity("am2-s19j", Some("S9")).is_err());
        assert!(validate_s9_identity("am1-s9", Some("AMLCtrl_BHB56902")).is_err());
    }

    #[test]
    fn state_classification_does_not_fabricate_success() {
        assert_eq!(classify_state(0xcc), PicState::Bootloader);
        assert_eq!(classify_state(0x60), PicState::Application(0x60));
        assert_eq!(classify_state(0x03), PicState::Application(0x03));
        assert_eq!(classify_state(0xff), PicState::Silent);
        assert_eq!(classify_state(0x42), PicState::Unknown(0x42));
    }

    #[test]
    fn process_scan_finds_comm_and_cmdline_without_shelling_out() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = env::temp_dir().join(format!("pic-recovery-proc-{unique}"));
        let first = root.join("123");
        let second = root.join("456");
        let ignored = root.join("not-a-pid");
        fs::create_dir_all(&first).expect("first proc entry");
        fs::create_dir_all(&second).expect("second proc entry");
        fs::create_dir_all(&ignored).expect("ignored entry");
        fs::write(first.join("comm"), "dcentrald\n").expect("comm");
        fs::write(first.join("cmdline"), b"/usr/bin/other\0").expect("cmdline");
        fs::write(second.join("comm"), "wrapper\n").expect("comm");
        fs::write(second.join("cmdline"), b"/usr/sbin/dcentrald\0--flag\0").expect("cmdline");

        assert_eq!(dcentrald_processes(&root).expect("scan"), vec![123, 456]);
        fs::remove_dir_all(root).expect("cleanup");
    }
}
