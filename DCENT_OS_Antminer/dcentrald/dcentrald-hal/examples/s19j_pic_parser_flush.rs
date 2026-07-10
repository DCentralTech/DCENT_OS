//! One-shot PIC parser flush helper for S19j Pro `a lab unit` recovery.
//!
//! Writes 16 zero bytes to `/dev/i2c-0` at PIC address `0x21` as a single I2C
//! transaction, matching the documented "clear parser state" recovery step for
//! the persistent `0x86 / ERR:V3` restart loop.

use std::env;

use dcentrald_hal::i2c::I2cBus;

fn parse_u8_arg(value: &str, name: &str) -> Result<u8, String> {
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u8::from_str_radix(hex, 16).map_err(|e| format!("invalid {} '{}': {}", name, value, e))
    } else {
        value
            .parse::<u8>()
            .map_err(|e| format!("invalid {} '{}': {}", name, value, e))
    }
}

fn usage() {
    eprintln!("usage: s19j_pic_parser_flush [--bus N] [--addr 0x21] [--count N]");
    eprintln!("defaults: --bus 0 --addr 0x21 --count 1");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut bus: u8 = 0;
    let mut addr: u8 = 0x21;
    let mut count: u8 = 1;

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                usage();
                return Ok(());
            }
            "--bus" => {
                let value = args.next().ok_or("missing value for --bus")?;
                bus = parse_u8_arg(&value, "bus")?;
            }
            "--addr" => {
                let value = args.next().ok_or("missing value for --addr")?;
                addr = parse_u8_arg(&value, "addr")?;
            }
            "--count" => {
                let value = args.next().ok_or("missing value for --count")?;
                count = parse_u8_arg(&value, "count")?;
                if count == 0 {
                    return Err("count must be > 0".into());
                }
            }
            other => {
                return Err(format!("unknown argument: {}", other).into());
            }
        }
    }

    // W2.3: raw bus access goes through the `recovery-tool`-gated escape
    // hatch. The example's `required-features = ["recovery-tool"]` entry in
    // `dcentrald-hal/Cargo.toml` ensures this compiles only when the caller
    // has explicitly opted in, mirroring the `pic-recovery` binary contract.
    let mut i2c = I2cBus::open_for_recovery(bus)?;
    i2c.set_slave(addr)?;

    for iter in 0..count {
        let written = i2c.write(&[0u8; 16])?;
        println!(
            "flush {} ok: bus={} addr=0x{:02X} bytes={}",
            iter + 1,
            bus,
            addr,
            written
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    Ok(())
}
