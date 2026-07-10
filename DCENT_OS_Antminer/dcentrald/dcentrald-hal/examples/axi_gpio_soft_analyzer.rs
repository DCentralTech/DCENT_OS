//! Soft logic analyzer for the am2 PSU SMBus AXI GPIO bank.
//!
//!  ground-truth probe (2026-05-23): bosminer mmaps `/dev/mem`
//! at `0x41220000` to bit-bang the PSU SMBus directly ( strace
//! evidence). Its register writes are MMIO and don't generate any
//! syscall — strace and ftrace can't catch the actual bytes.
//!
//! This program mmaps the SAME region READ-ONLY, polls the DATA
//! register at high frequency, and records every transition with a
//! high-resolution timestamp. Reads don't interfere with bosminer's
//! writes (MMIO has no read-disturb on the Xilinx AXI GPIO IP).
//!
//! Run while bosminer is actively initializing the PSU; the resulting
//! transition log lets us reconstruct the I2C waveform and decode the
//! actual byte sequence bosminer puts on the wire.
//!
//! Layout (matches `psu_gpio_i2c.rs` constants):
//!   - Base: `0x41220000` (4 KiB window)
//!   - DATA register: +0x000
//!   - SDA = bit 0 (gpio895)
//!   - SCL = bit 1 (gpio896)
//!
//! Usage:
//!   axi_gpio_soft_analyzer [duration_secs] [output_path]
//! Defaults: 10 s, /tmp/axi_gpio_trace.csv
//!
//! Output CSV columns: `ns_since_start,data_hex,sda,scl,event`

use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

const AXI_GPIO_BASE: u64 = 0x4122_0000;
const AXI_GPIO_SIZE: usize = 4096;
const DATA_OFFSET: usize = 0x000;
const SDA_BIT: u32 = 1 << 0;
const SCL_BIT: u32 = 1 << 1;

fn main() -> std::io::Result<()> {
    let args: Vec<String> = env::args().collect();
    let duration_secs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10);
    let output_path = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "/tmp/axi_gpio_trace.csv".to_string());

    eprintln!(
        "Wave-38 soft analyzer: polling 0x{:08X} (DATA+0x{:03X}) for {}s -> {}",
        AXI_GPIO_BASE, DATA_OFFSET, duration_secs, output_path
    );

    let mem_file = OpenOptions::new().read(true).open("/dev/mem")?;

    let ptr = unsafe {
        nix::sys::mman::mmap(
            None,
            NonZeroUsize::new(AXI_GPIO_SIZE).expect("nonzero"),
            nix::sys::mman::ProtFlags::PROT_READ,
            nix::sys::mman::MapFlags::MAP_SHARED,
            &mem_file,
            AXI_GPIO_BASE as nix::libc::off_t,
        )
    }
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("mmap: {}", e)))?;

    let base = ptr.as_ptr() as *const u8;
    let data_reg = unsafe { base.add(DATA_OFFSET) as *const u32 };

    let mut out = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&output_path)?;
    writeln!(out, "ns_since_start,data_hex,sda,scl,event")?;

    let start = Instant::now();
    let deadline = start + Duration::from_secs(duration_secs);

    // Prime: capture initial state
    let mut last = unsafe { std::ptr::read_volatile(data_reg) };
    let initial_sda = (last & SDA_BIT) != 0;
    let initial_scl = (last & SCL_BIT) != 0;
    writeln!(
        out,
        "0,0x{:08X},{},{},INITIAL",
        last, initial_sda as u8, initial_scl as u8
    )?;

    let mut transitions: u64 = 0;
    let mut samples: u64 = 0;

    while Instant::now() < deadline {
        // Tight read loop — no sleep, no syscalls beyond MMIO
        let cur = unsafe { std::ptr::read_volatile(data_reg) };
        samples = samples.wrapping_add(1);
        if cur != last {
            let elapsed_ns = start.elapsed().as_nanos() as u64;
            let sda = (cur & SDA_BIT) != 0;
            let scl = (cur & SCL_BIT) != 0;
            let prev_sda = (last & SDA_BIT) != 0;
            let prev_scl = (last & SCL_BIT) != 0;
            let event = if sda != prev_sda && scl != prev_scl {
                "BOTH"
            } else if sda != prev_sda {
                if scl {
                    if sda {
                        "STOP?"
                    } else {
                        "START?"
                    }
                } else {
                    "SDA"
                }
            } else if scl {
                "SCL_HI"
            } else {
                "SCL_LO"
            };
            writeln!(
                out,
                "{},0x{:08X},{},{},{}",
                elapsed_ns, cur, sda as u8, scl as u8, event
            )?;
            last = cur;
            transitions = transitions.wrapping_add(1);
        }
        // Light spin hint
        std::hint::spin_loop();
    }

    let elapsed = start.elapsed();
    eprintln!(
        "Wave-38 soft analyzer: done. {} samples, {} transitions in {}.{:03} s, sample rate ~{:.1} MHz",
        samples,
        transitions,
        elapsed.as_secs(),
        elapsed.subsec_millis(),
        (samples as f64) / elapsed.as_secs_f64() / 1e6
    );
    writeln!(
        out,
        "# SUMMARY: {} samples, {} transitions in {}.{:03} s",
        samples,
        transitions,
        elapsed.as_secs(),
        elapsed.subsec_millis()
    )?;

    Ok(())
}
