//! dspic-flash — recovery tool for downgraded dsPIC33EP16GS202 voltage
//! controllers on Antminer S17/S19j Pro hashboards.
//!
//! Subcommands:
//!   `probe`        read-only assessment — reports whether reflash is feasible
//!   `proto-probe`  raw protocol probe — dumy_read + framed GET_VERSION only
//!                  (safest possible diagnostic, no destructive ops)
//!   `flash`        reflash the app region with a firmware image
//!                  **destructive — high brick risk; requires --confirm-bricked**
//!
//! ## 2026-04-27 — BraiinsOS source-of-truth correction
//!
//! This tool now speaks the CORRECT bootloader opcodes per the
//! BraiinsOS source-of-truth at
//! :53-77`. A prior
//! 2026-04-26 AMTC inference incorrectly mapped `0x04 = RESET_PIC`;
//! the truth is `0x04 = ERASE_IIC_FLASH` and `0x07 = RESET_PIC`.
//! Both PIC16F1704 (S9) and dsPIC33EP16GS202 (S17/S19j Pro) share the
//! same bmminer-derived opcode table.
//!
//! Codex live test 2026-04-27 on the `a lab unit` unit (fw=0x86) further
//! established that framed `[55 AA 07]` does NOT enter bootloader on
//! that firmware variant — `proto-probe` is the safest aliveness check
//! (no destructive writes), `probe` is the recommended pre-flash
//! readiness check, and `flash` is the only path that ever touches
//! NAND. The `reset_pic` capability behind `flash` is gated by
//! `--confirm-bricked`.
//!
//! See `dcentrald-asic/src/dspic_flash.rs` for the protocol and safety
//! guard semantics.
//!
//! ## fw=0x86 software recovery (W12.3 / R3-6)
//!
//! For the fw=0x86 bootloader→application path, use the canonical
//! subcommands on the **`pic-recovery`** companion binary (same Cargo
//! crate, same `recovery-tool` feature gate):
//!
//! * `pic-recovery dspic-jump-to-app   --bus N --addr 0xNN --dspic-platform <p> --confirm-bricked`
//!   (Path B — 100% confidence per RE3 §5.2, non-destructive)
//! * `pic-recovery dspic-reflash-fw86  --bus N --addr 0xNN --dspic-platform <p> --hex <path> \\
//!     --confirm-bricked --i-acknowledge-60-percent-byte-exact-confidence [--serial <S>]`
//!   (Path C — 60% confidence per RE3 §3.4 + §6, double-gated +
//!   typed-serial confirmation per
//!   )

use dcentrald_asic::dspic_flash::{self, FirmwareImage};
use std::env;
use std::process::ExitCode;

fn print_usage() {
    eprintln!("Usage:");
    eprintln!("  dspic-flash probe        <i2c_path> <slave_addr>");
    eprintln!("  dspic-flash proto-probe  <i2c_path> <slave_addr>");
    eprintln!(
        "  dspic-flash flash        <i2c_path> <slave_addr> <firmware.txt> --confirm-bricked"
    );
    eprintln!();
    eprintln!("Examples:");
    eprintln!("  dspic-flash probe        /dev/i2c-0 0x21");
    eprintln!("  dspic-flash proto-probe  /dev/i2c-0 0x21");
    eprintln!(
        "  dspic-flash flash        /dev/i2c-0 0x21 dsPIC33EP16GS202_app.txt --confirm-bricked"
    );
    eprintln!();
    eprintln!("DANGER: `flash` writes to dsPIC NAND; failure leaves the chip recoverable only via");
    eprintln!("        ICSP (Pickit3 + soldering iron). Do NOT run on a working dsPIC.");
    eprintln!();
    eprintln!("For fw=0x86 software recovery, use:");
    eprintln!("  pic-recovery dspic-jump-to-app   ...   (Path B, non-destructive)");
    eprintln!("  pic-recovery dspic-reflash-fw86  ...   (Path C, double-gated)");
}

fn parse_addr(s: &str) -> Result<u8, String> {
    let s = s.trim_start_matches("0x");
    u8::from_str_radix(s, 16).map_err(|e| format!("invalid slave_addr `0x{}`: {}", s, e))
}

fn run_probe(i2c: &str, addr: u8) -> Result<(), String> {
    let report = dspic_flash::probe(i2c, addr).map_err(|e| e.to_string())?;
    println!("dsPIC at {} addr 0x{:02X} probe report:", i2c, addr);
    match report.fw_byte {
        Some(fw) => println!("  fw_byte                          = 0x{:02X}", fw),
        None => println!("  fw_byte                          = (silent)"),
    }
    println!(
        "  framed_get_version_works         = {}",
        report.set_flash_pointer_ack
    );
    println!(
        "  fw_byte_stable                   = {}",
        report.read_sector_returns_real_data
    );
    println!();
    println!("Recommendation:");
    println!("  {}", report.recommendation);
    Ok(())
}

fn run_proto_probe(i2c: &str, addr: u8) -> Result<(), String> {
    let report = dspic_flash::probe_protocol(i2c, addr).map_err(|e| e.to_string())?;
    println!(
        "dsPIC at {} addr 0x{:02X} protocol probe (read-only):",
        i2c, addr
    );
    println!(
        "  fw_byte                          = 0x{:02X}",
        report.fw_byte
    );
    println!(
        "  fw_byte_stable                   = {}",
        report.fw_byte_stable
    );
    println!(
        "  framed_get_version_works         = {}",
        report.framed_get_version_works
    );
    print!("  framed_response                  = [");
    for (i, b) in report.framed_response.iter().enumerate() {
        if i > 0 {
            print!(" ");
        }
        print!("{:02X}", b);
    }
    println!("]");
    println!();
    println!("Summary: {}", report.summary());
    Ok(())
}

fn run_flash(
    i2c: &str,
    addr: u8,
    firmware_path: &str,
    accept_brick_risk: bool,
) -> Result<(), String> {
    if !accept_brick_risk {
        return Err("flash requires --confirm-bricked flag (HIGH RISK)".into());
    }
    eprintln!("Loading firmware from {} ...", firmware_path);
    let image = FirmwareImage::parse_text(firmware_path).map_err(|e| e.to_string())?;
    eprintln!(
        "Loaded {} instruction words. Beginning reflash on dsPIC at {} addr 0x{:02X}.",
        image.instruction_count(),
        i2c,
        addr
    );
    eprintln!("Probing first ...");
    let report = dspic_flash::probe(i2c, addr).map_err(|e| e.to_string())?;
    eprintln!("Probe: {}", report.recommendation);
    if !report.read_sector_returns_real_data {
        return Err("probe failed: framed protocol unavailable. ICSP recovery required.".into());
    }
    eprintln!("Reflashing ... (this is irreversible)");
    dspic_flash::reflash(i2c, addr, &image, true).map_err(|e| e.to_string())?;
    eprintln!("Reflash write sequence finished. Power-cycle the miner and verify with `probe`.");
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        print_usage();
        return ExitCode::from(2);
    }
    let result: Result<(), String> = match args[1].as_str() {
        "probe" if args.len() == 4 => parse_addr(&args[3]).and_then(|a| run_probe(&args[2], a)),
        "proto-probe" if args.len() == 4 => {
            parse_addr(&args[3]).and_then(|a| run_proto_probe(&args[2], a))
        }
        "flash" if args.len() >= 5 => {
            let confirm = args[5..].iter().any(|s| s == "--confirm-bricked");
            parse_addr(&args[3]).and_then(|a| run_flash(&args[2], a, &args[4], confirm))
        }
        "-h" | "--help" => {
            print_usage();
            return ExitCode::SUCCESS;
        }
        _ => {
            print_usage();
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {}", e);
            ExitCode::from(1)
        }
    }
}
