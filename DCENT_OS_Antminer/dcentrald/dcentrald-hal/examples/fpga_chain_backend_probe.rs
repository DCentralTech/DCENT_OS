//! `fpga_chain_backend_probe` — read-only probe for the FPGA-FIFO chain.
//!
//!  (2026-05-23) Phase-1 skeleton. Opens chain 1 (BraiinsOS UIO
//! map `chain1-common` / `chain1-cmd-rx` / `chain1-work-rx` /
//! `chain1-work-tx`) over the FPGA-FIFO backend and reports the
//! transport label. Phase 2 adds the real body: register snapshot
//! (BUILD_ID / CTRL / BAUD / STAT), optional single GetAddress, and
//! response readback.
//!
//! ## Why
//!
//! The  hypothesis (per
//! )
//! is that `a lab unit`'s BraiinsOS bitstream wires the chain UART through the
//! FPGA FIFO IP blocks at `0x43C0Nxxx`, not the PL UART at
//! `0x41001000`. This probe is the **operator-paced Phase-0 validator**:
//! before the daemon flips `DCENT_AM2_USE_FPGA_CHAIN=1` on a live unit,
//! the operator runs this binary on `a lab unit` and confirms (a) the UIO
//! devices open without error, (b) the register snapshot matches the
//! captured baseline (`CONTEXT-LINKS.md` §"`a lab unit` chain1-common register
//! state"), and (c) the chain returns at least one GetAddress response
//! when the full sequence is exercised.
//!
//! ## Usage (Phase 1 — skeleton)
//!
//! ```text
//! ./fpga_chain_backend_probe              # default chain 1 (DCENT_OS slot)
//! ./fpga_chain_backend_probe --chain 2    # chain 2 (slot 1 on `a lab unit`)
//! ./fpga_chain_backend_probe --devmem     # use /dev/mem instead of UIO
//! ```
//!
//! Phase 1 only prints the backend identity; no register read is
//! issued. Phase 2 will add `--snapshot` and `--get-address` flags.

use dcentrald_hal::chain_backend::Bm1397PlusChainBackend;
use dcentrald_hal::fpga_chain_backend::FpgaChainBackend;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut chain: u8 = 1;
    let mut use_devmem = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--chain" => {
                i += 1;
                chain = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .expect("--chain requires a u8 argument");
            }
            "--devmem" => use_devmem = true,
            "--help" | "-h" => {
                println!(
                    "Wave-26 FPGA-chain-backend probe\n\
                     \n\
                     Usage:\n  fpga_chain_backend_probe [--chain N] [--devmem]\n\
                     \n\
                     Phase 1: prints backend identity only. Phase 2 adds register\n\
                     snapshot + GetAddress probe. The daemon's live path is gated\n\
                     by DCENT_AM2_USE_FPGA_CHAIN; this binary opens directly."
                );
                return;
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }

    // Devmem path needs the chain phys base. BraiinsOS am2 bitstream
    // layout (CONTEXT-LINKS.md): chain 0 = 0x43C00000, chain 1 =
    // 0x43C10000, chain 2 = 0x43C20000, chain 3 = 0x43C30000.
    let phys_base = 0x43C0_0000u64 + (chain as u64) * 0x1_0000;
    let backend = if use_devmem {
        FpgaChainBackend::open_am2_devmem(chain, phys_base)
    } else {
        FpgaChainBackend::open_am2_uio(chain)
    };

    let backend = match backend {
        Ok(b) => b,
        Err(e) => {
            eprintln!("open failed: {e}");
            std::process::exit(1);
        }
    };

    println!(
        "Wave-26 FPGA-chain-backend probe (Phase 1 skeleton)\n\
         chain_id        = {}\n\
         transport_label = {}\n\
         \n\
         Phase 2 will add: register snapshot (BUILD_ID/CTRL/BAUD/STAT),\n\
         FIFO reset + BAUD configure, single GetAddress, response readback.\n\
         Until then, `DCENT_AM2_USE_FPGA_CHAIN=1` is not safe to set on a live unit.",
        backend.chain_id(),
        backend.transport_label()
    );
}
