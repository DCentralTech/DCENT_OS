//! `dspic-flash` -- explicit fail-closed boundary for unsupported dsPIC recovery.
//!
//! Historical `probe` and `proto-probe` commands wrote an unresolved protocol
//! dialect to caller-selected I2C endpoints before controller-family discovery.
//! They are removed together with software flashing. Use daemon-owned typed
//! telemetry for an identified controller or physical ICSP for recovery.

use std::env;
use std::process::ExitCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Status,
    Help,
}

fn parse_command(arguments: &[String]) -> Result<Command, String> {
    match arguments {
        [command] if command == "status" => Ok(Command::Status),
        [command] if matches!(command.as_str(), "help" | "-h" | "--help") => Ok(Command::Help),
        [command, ..] if matches!(command.as_str(), "probe" | "proto-probe" | "flash") => {
            Err(format!(
                "{command} is disabled: no standalone dsPIC I2C operation is safe before discovered endpoint identity, exclusive bus ownership, and a byte-exact protocol codec are proven"
            ))
        }
        [] => Err("missing command; hardware access is never implicit".into()),
        [command, ..] => Err(format!(
            "unknown command {command:?}; only the offline status command is available"
        )),
    }
}

fn print_status() {
    println!("software_recovery=unavailable");
    println!("standalone_i2c_probe=unavailable");
    println!("deterministic_recovery=physical-icsp");
    println!("reason=controller identity and byte-exact write protocol are not proven");
}

fn print_usage() {
    eprintln!("Usage: dspic-flash status");
    eprintln!();
    eprintln!("No hardware command is available. Query identified controller telemetry");
    eprintln!("through dcentrald; use physical ICSP for deterministic flash recovery.");
}

fn main() -> ExitCode {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    match parse_command(&arguments) {
        Ok(Command::Status) => {
            print_status();
            ExitCode::SUCCESS
        }
        Ok(Command::Help) => {
            print_usage();
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            print_usage();
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn only_offline_status_is_available() {
        assert_eq!(parse_command(&args(&["status"])), Ok(Command::Status));
        assert_eq!(parse_command(&args(&["--help"])), Ok(Command::Help));
    }

    #[test]
    fn every_historical_hardware_command_is_refused() {
        for command in ["probe", "proto-probe", "flash"] {
            let error = parse_command(&args(&[command, "/dev/i2c-0", "0x21"]))
                .expect_err("hardware command must be absent");
            assert!(error.contains("disabled"), "{command}: {error}");
        }
    }

    #[test]
    fn no_arguments_never_imply_hardware_access() {
        assert!(parse_command(&[]).is_err());
    }
}
