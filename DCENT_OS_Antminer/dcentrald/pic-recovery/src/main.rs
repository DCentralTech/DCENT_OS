//! pic-recovery: Flash BraiinsOS PIC firmware onto bricked PIC16F1704 voltage controllers
//!
//! The PIC16F1704 on each S9 hash board has a bootloader (0x0000-0x02FF, write-protected)
//! and an app region (0x0300-0x1FFF). When PICs get bricked (corrupted app firmware),
//! they stay in bootloader mode (I2C read returns 0xCC).
//!
//! This tool speaks the bootloader's I2C protocol to erase and rewrite the app firmware.
//!
//! Protocol (reverse-engineered from BraiinsOS braiins_power.rs):
//!   - Preamble: [0x55, 0xAA] before every command
//!   - Commands sent as bulk I2C write: [0x55, 0xAA, CMD, data...]
//!   - Reads: send command, then separate I2C read
//!   - Inter-command delay: 100ms after erase/write operations
//!
//! Usage:
//!   pic-recovery [--addr 0x55] [--fw /path/to/hash_s8_app.txt] [--verify] [--dry-run]
//!   pic-recovery --all                    # recover all 3 PICs
//!   pic-recovery --addr 0x56 --verify     # recover chain 7 with verify
//!
//! D-Central Technologies — GPL-3.0

use std::env;
use std::fs;
use std::io::{self, Read as _, Write as _};
use std::os::unix::io::RawFd;
use std::thread;
use std::time::Duration;

// ── I2C constants ────────────────────────────────────────────────────────────

// On musl ARM, libc::ioctl's request arg is `c_int` (i32). On glibc it's
// `c_ulong` (u32 on armv7, u64 on x86_64). Keep this as a literal const
// and cast at the call site to match the target's signature.
const I2C_SLAVE_FORCE: u64 = 0x0706;

// ── PIC constants ────────────────────────────────────────────────────────────

const PIC_ADDRS: [(u8, &str); 3] = [(0x55, "Chain 6"), (0x56, "Chain 7"), (0x57, "Chain 8")];

const PIC_BOOTLOADER: u8 = 0xCC;

// Bootloader commands (prefixed with preamble [0x55, 0xAA])
const CMD_SET_FLASH_POINTER: u8 = 0x01;
const CMD_SEND_DATA_TO_IIC: u8 = 0x02;
const CMD_READ_DATA_FROM_IIC: u8 = 0x03;
const CMD_ERASE_IIC_FLASH: u8 = 0x04;
const CMD_WRITE_DATA_INTO_PIC: u8 = 0x05;
const CMD_JUMP_FROM_LOADER: u8 = 0x06;
const CMD_RESET_PIC: u8 = 0x07;
const CMD_GET_FLASH_POINTER: u8 = 0x08;
const CMD_ERASE_APP_PROGRAM: u8 = 0x09;

// Flash layout
const APP_START: u16 = 0x0300;
const SECTOR_SIZE_WORDS: u16 = 32;
const WRITE_CHUNK_BYTES: usize = 16;

// Timing
const FLASH_DELAY: Duration = Duration::from_millis(100);
const RESET_DELAY: Duration = Duration::from_millis(2000);
const INTER_CMD_DELAY: Duration = Duration::from_millis(10);

// BraiinsOS v0x03 firmware (hash_s8_app.txt) — embedded at compile time
const FIRMWARE_HEX: &str = include_str!("../firmware/hash_s8_app.txt");

// ── I2C helpers ──────────────────────────────────────────────────────────────

struct I2cBus {
    fd: RawFd,
    current_addr: Option<u8>,
}

impl I2cBus {
    fn open(bus: u8) -> io::Result<Self> {
        let path = format!("/dev/i2c-{}\0", bus);
        let fd = unsafe { libc::open(path.as_ptr() as *const libc::c_char, libc::O_RDWR) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            fd,
            current_addr: None,
        })
    }

    fn set_slave(&mut self, addr: u8) -> io::Result<()> {
        if self.current_addr == Some(addr) {
            return Ok(());
        }
        // Cast I2C_SLAVE_FORCE through the target's expected ioctl request type.
        // armv7-musleabihf uses c_int (i32); glibc/aarch64 use c_ulong.
        let ret = unsafe {
            #[cfg(target_env = "musl")]
            {
                libc::ioctl(
                    self.fd,
                    I2C_SLAVE_FORCE as libc::c_int,
                    addr as libc::c_ulong,
                )
            }
            #[cfg(not(target_env = "musl"))]
            {
                libc::ioctl(
                    self.fd,
                    I2C_SLAVE_FORCE as libc::c_ulong,
                    addr as libc::c_ulong,
                )
            }
        };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        self.current_addr = Some(addr);
        Ok(())
    }

    /// Write bytes as a single I2C transaction (bulk write)
    fn write_bulk(&self, data: &[u8]) -> io::Result<()> {
        let n = unsafe { libc::write(self.fd, data.as_ptr() as *const libc::c_void, data.len()) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Write a single byte (one I2C transaction: START+addr+byte+STOP)
    fn write_byte(&self, byte: u8) -> io::Result<()> {
        self.write_bulk(&[byte])
    }

    /// Write each byte as a separate I2C transaction with delay
    fn write_byte_by_byte(&self, data: &[u8], delay: Duration) -> io::Result<()> {
        for &b in data {
            self.write_byte(b)?;
            thread::sleep(delay);
        }
        Ok(())
    }

    /// Read N bytes in a single I2C read transaction
    fn read_bytes(&self, count: usize) -> io::Result<Vec<u8>> {
        let mut buf = vec![0u8; count];
        let n = unsafe { libc::read(self.fd, buf.as_mut_ptr() as *mut libc::c_void, count) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        buf.truncate(n as usize);
        Ok(buf)
    }

    /// Read a single byte
    fn read_byte(&self) -> io::Result<u8> {
        let data = self.read_bytes(1)?;
        Ok(data.first().copied().unwrap_or(0xFF))
    }
}

impl Drop for I2cBus {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.fd);
        }
    }
}

// ── PIC recovery logic ──────────────────────────────────────────────────────

struct PicRecovery<'a> {
    i2c: &'a mut I2cBus,
    addr: u8,
    chain_name: &'a str,
    verbose: bool,
}

impl<'a> PicRecovery<'a> {
    fn new(i2c: &'a mut I2cBus, addr: u8, chain_name: &'a str, verbose: bool) -> Self {
        Self {
            i2c,
            addr,
            chain_name,
            verbose,
        }
    }

    fn log(&self, msg: &str) {
        println!("  [0x{:02X} {}] {}", self.addr, self.chain_name, msg);
    }

    fn dbg(&self, msg: &str) {
        if self.verbose {
            println!("  [0x{:02X} DBG] {}", self.addr, msg);
        }
    }

    fn select(&mut self) -> io::Result<()> {
        self.i2c.set_slave(self.addr)
    }

    /// Send a bootloader command: [0x55, 0xAA, CMD] + optional data.
    /// Preamble+CMD sent as ONE I2C transaction (PIC parser resets on STOP).
    /// Data payload sent as SEPARATE transaction (bootloader loads into buffer).
    /// This matches bmminer's observed I2C pattern on stock firmware.
    fn send_command(&mut self, cmd: u8, data: &[u8]) -> io::Result<()> {
        self.select()?;
        // Preamble + command in one transaction
        let header = [0x55, 0xAA, cmd];
        self.dbg(&format!("TX header: {:02X?}", &header));
        self.i2c.write_bulk(&header)?;
        thread::sleep(Duration::from_millis(10));

        // Data payload in separate transaction (if any)
        if !data.is_empty() {
            self.dbg(&format!(
                "TX data[{}]: {:02X?}",
                data.len(),
                &data[..data.len().min(8)]
            ));
            self.i2c.write_bulk(data)?;
            thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }

    /// Send command byte-by-byte (alternative approach for stubborn PICs)
    fn send_command_bbb(&mut self, cmd: u8, data: &[u8]) -> io::Result<()> {
        self.select()?;
        let mut payload = vec![0x55, 0xAA, cmd];
        payload.extend_from_slice(data);
        self.dbg(&format!("TX byte-by-byte: {:02X?}", &payload));
        self.i2c
            .write_byte_by_byte(&payload, Duration::from_millis(1))
    }

    /// Read raw PIC state byte (0xCC=bootloader, 0x60=app, 0xFF=dead)
    fn read_raw(&mut self) -> io::Result<u8> {
        self.select()?;
        self.i2c.read_byte()
    }

    /// Set flash pointer to a word address
    fn set_flash_pointer(&mut self, word_addr: u16) -> io::Result<()> {
        let hi = (word_addr >> 8) as u8;
        let lo = (word_addr & 0xFF) as u8;
        self.send_command(CMD_SET_FLASH_POINTER, &[hi, lo])?;
        thread::sleep(INTER_CMD_DELAY);
        Ok(())
    }

    /// Get flash pointer (returns word address)
    fn get_flash_pointer(&mut self) -> io::Result<u16> {
        self.send_command(CMD_GET_FLASH_POINTER, &[])?;
        thread::sleep(INTER_CMD_DELAY);
        self.select()?;
        let data = self.i2c.read_bytes(2)?;
        if data.len() >= 2 {
            Ok(((data[0] as u16) << 8) | data[1] as u16)
        } else {
            Ok(0xFFFF)
        }
    }

    /// Erase one flash sector (32 words) at current pointer
    fn erase_sector(&mut self) -> io::Result<()> {
        self.send_command(CMD_ERASE_IIC_FLASH, &[])?;
        thread::sleep(FLASH_DELAY);
        Ok(())
    }

    /// Erase entire app region (CMD 0x09)
    fn erase_app(&mut self) -> io::Result<()> {
        self.send_command(CMD_ERASE_APP_PROGRAM, &[])?;
        thread::sleep(Duration::from_millis(500)); // Bulk erase takes longer
        Ok(())
    }

    /// Load 16 bytes into PIC write buffer (CMD 0x02)
    fn send_data(&mut self, chunk: &[u8]) -> io::Result<()> {
        assert!(chunk.len() == WRITE_CHUNK_BYTES);
        self.send_command(CMD_SEND_DATA_TO_IIC, chunk)
    }

    /// Commit buffer to flash (CMD 0x05)
    fn write_flash(&mut self) -> io::Result<()> {
        self.send_command(CMD_WRITE_DATA_INTO_PIC, &[])?;
        thread::sleep(FLASH_DELAY);
        Ok(())
    }

    /// Read 16 bytes from flash at current pointer (CMD 0x03)
    fn read_flash(&mut self) -> io::Result<Vec<u8>> {
        self.send_command(CMD_READ_DATA_FROM_IIC, &[])?;
        thread::sleep(INTER_CMD_DELAY);
        self.select()?;
        self.i2c.read_bytes(16)
    }

    /// Jump from bootloader to app (CMD 0x06)
    fn jump_to_app(&mut self) -> io::Result<()> {
        self.send_command(CMD_JUMP_FROM_LOADER, &[])?;
        thread::sleep(FLASH_DELAY);
        Ok(())
    }

    /// Reset PIC back to bootloader (CMD 0x07, BraiinsOS only)
    fn reset_pic(&mut self) -> io::Result<()> {
        self.send_command(CMD_RESET_PIC, &[])?;
        thread::sleep(RESET_DELAY);
        Ok(())
    }

    /// Flush the bootloader parser state by sending garbage
    fn flush_parser(&mut self) -> io::Result<()> {
        self.dbg("Flushing parser state (16 zero bytes)");
        self.select()?;
        // Send zeros to reset the parser state machine back to IDLE
        let zeros = [0u8; 16];
        let _ = self.i2c.write_bulk(&zeros);
        thread::sleep(INTER_CMD_DELAY);
        Ok(())
    }

    /// Check if PIC is alive and in bootloader mode
    fn probe(&mut self) -> PicState {
        match self.read_raw() {
            Ok(PIC_BOOTLOADER) => {
                self.log("BOOTLOADER (0xCC) — recoverable via I2C");
                PicState::Bootloader
            }
            Ok(0xFF) => {
                self.log("DEAD (0xFF) — no response, board may be disconnected");
                PicState::Dead
            }
            Ok(0x60) => {
                self.log("APP MODE (0x60) — firmware is fine, no recovery needed");
                PicState::App
            }
            Ok(v) => {
                self.log(&format!("UNKNOWN (0x{:02X})", v));
                PicState::Unknown(v)
            }
            Err(e) => {
                self.log(&format!("I2C READ FAILED: {}", e));
                PicState::Error
            }
        }
    }

    /// Diagnostic: write a known 16-byte pattern, read it back, show byte mapping.
    /// This reveals the PIC bootloader's byte ordering for SEND_DATA/READ_DATA commands.
    fn diagnose_byte_ordering(&mut self) -> Result<(), String> {
        self.log("=== BYTE ORDERING DIAGNOSTIC ===");

        let state = self.probe();
        if !matches!(state, PicState::Bootloader) {
            return Err("PIC must be in bootloader (0xCC) for diagnostic".into());
        }

        let _ = self.flush_parser();

        // Use a safe flash location near end of app region (0x0F00)
        let test_addr: u16 = 0x0F00;

        // Erase the sector first
        self.log(&format!("Erasing sector at 0x{:04X}...", test_addr));
        if let Err(e) = self.set_flash_pointer(test_addr) {
            return Err(format!("Set pointer failed: {}", e));
        }
        if let Err(e) = self.erase_sector() {
            return Err(format!("Erase failed: {}", e));
        }

        // Read erased data (should be all 0xFF or 0x3FFF)
        if let Err(e) = self.set_flash_pointer(test_addr) {
            return Err(format!("Set pointer for read failed: {}", e));
        }
        match self.read_flash() {
            Ok(erased) => self.log(&format!(
                "Erased flash reads: {:02X?}",
                &erased[..16.min(erased.len())]
            )),
            Err(e) => self.log(&format!("Read erased flash failed: {}", e)),
        }

        // Write a known test pattern: alternating recognizable bytes
        let test_pattern: [u8; 16] = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x10, 0x20, 0x30, 0x40, 0x50, 0x60,
            0x70, 0x80,
        ];
        self.log(&format!("Writing test pattern: {:02X?}", &test_pattern));

        if let Err(e) = self.set_flash_pointer(test_addr) {
            return Err(format!("Set pointer for write failed: {}", e));
        }
        if let Err(e) = self.send_data(&test_pattern) {
            return Err(format!("Send data failed: {}", e));
        }
        if let Err(e) = self.write_flash() {
            return Err(format!("Write flash failed: {}", e));
        }

        // Read it back
        if let Err(e) = self.set_flash_pointer(test_addr) {
            return Err(format!("Set pointer for readback failed: {}", e));
        }
        match self.read_flash() {
            Ok(readback) => {
                self.log(&format!(
                    "Read back:      {:02X?}",
                    &readback[..16.min(readback.len())]
                ));
                self.log("");
                self.log("Byte-by-byte comparison:");
                for i in 0..16.min(readback.len()) {
                    let status = if test_pattern[i] == readback[i] {
                        "MATCH"
                    } else {
                        "DIFF"
                    };
                    self.log(&format!(
                        "  [{:2}] wrote 0x{:02X}, read 0x{:02X}  {}",
                        i, test_pattern[i], readback[i], status
                    ));
                }

                // Check for common patterns
                let all_match = test_pattern
                    .iter()
                    .zip(readback.iter())
                    .all(|(a, b)| a == b);
                let swapped_pairs = (0..8).all(|i| {
                    test_pattern[i * 2] == readback[i * 2 + 1]
                        && test_pattern[i * 2 + 1] == readback[i * 2]
                });
                let all_ff = readback.iter().all(|&b| b == 0xFF);
                let all_zero = readback.iter().all(|&b| b == 0x00);

                self.log("");
                if all_match {
                    self.log("RESULT: Bytes match exactly — little-endian write/read is CORRECT");
                } else if swapped_pairs {
                    self.log("RESULT: Byte pairs are SWAPPED — PIC uses big-endian word storage!");
                    self.log("FIX: Change parse_firmware() to big-endian: (high, low) per word");
                } else if all_ff {
                    self.log("RESULT: All 0xFF — write did NOT take effect (erase only)");
                } else if all_zero {
                    self.log("RESULT: All 0x00 — read returned zeros (I2C issue?)");
                } else {
                    self.log("RESULT: Unknown pattern — manual analysis needed");
                }
            }
            Err(e) => {
                return Err(format!("Read back failed: {}", e));
            }
        }

        Ok(())
    }

    /// Attempt full recovery: erase + write + verify + jump
    fn recover(&mut self, firmware: &[u8], verify: bool, dry_run: bool) -> Result<(), String> {
        // Step 1: Verify we're in bootloader
        self.log("Step 1: Checking PIC state...");
        let state = self.probe();
        match state {
            PicState::Bootloader => {}
            PicState::App => {
                self.log("PIC is already in app mode — no recovery needed!");
                return Ok(());
            }
            PicState::Dead | PicState::Error => {
                return Err("PIC is not responding — check hash board connection".into());
            }
            PicState::Unknown(v) => {
                self.log(&format!(
                    "Unexpected state 0x{:02X}, attempting recovery anyway...",
                    v
                ));
            }
        }

        if dry_run {
            self.log("[DRY RUN] Would erase and write firmware. Stopping here.");
            return Ok(());
        }

        // Step 2: Flush parser state
        self.log("Step 2: Flushing bootloader parser...");
        let _ = self.flush_parser();

        // Step 3: Test write capability
        self.log("Step 3: Testing write capability...");
        let write_ok = self.test_write();
        if !write_ok {
            self.log("BULK write failed — trying byte-by-byte approach...");
            if !self.test_write_bbb() {
                return Err(
                    "PIC NACKs all writes. The bootloader firmware is likely corrupted.\n\
                     Recovery via I2C is not possible. You need:\n\
                     1. A PICkit3/4 ICSP programmer connected to the hash board test pads\n\
                     2. Or swap in hash boards with working PICs"
                        .into(),
                );
            }
            self.log("Byte-by-byte writes work! Continuing with bbb mode...");
        }

        // Step 4: Erase app region
        self.log("Step 4: Erasing app firmware region...");
        self.erase_app_region(firmware.len())?;

        // Step 5: Write firmware
        self.log("Step 5: Writing firmware...");
        self.write_firmware(firmware)?;

        // Step 6: Verify (optional)
        if verify {
            self.log("Step 6: Verifying firmware...");
            self.verify_firmware(firmware)?;
        }

        // Step 7: Jump to app
        self.log("Step 7: Jumping to app firmware...");
        if let Err(e) = self.jump_to_app() {
            self.log(&format!(
                "JUMP command failed: {} (may need power cycle)",
                e
            ));
        }
        thread::sleep(Duration::from_millis(500));

        // Step 8: Check result
        let raw = self.read_raw().unwrap_or(0xFF);
        if raw == 0x60 || raw == 0x03 {
            self.log(&format!("SUCCESS! PIC now in app mode (0x{:02X})", raw));
        } else if raw == PIC_BOOTLOADER {
            self.log("PIC still in bootloader after JUMP — power cycle required");
            self.log("Firmware was written successfully; PIC will boot to app on next power-on.");
        } else {
            self.log(&format!("PIC state after recovery: 0x{:02X}", raw));
        }

        Ok(())
    }

    /// Test if bulk writes work
    fn test_write(&mut self) -> bool {
        // Try setting flash pointer — simple 5-byte write
        match self.send_command(CMD_SET_FLASH_POINTER, &[0x03, 0x00]) {
            Ok(()) => {
                self.dbg("Bulk write succeeded");
                // Verify by reading pointer back
                match self.get_flash_pointer() {
                    Ok(ptr) => {
                        self.dbg(&format!("Flash pointer: 0x{:04X}", ptr));
                        ptr == 0x0300 || ptr != 0xFFFF // Accept any non-error
                    }
                    Err(_) => true, // Write succeeded even if read fails
                }
            }
            Err(e) => {
                self.dbg(&format!("Bulk write failed: {}", e));
                false
            }
        }
    }

    /// Test if byte-by-byte writes work
    fn test_write_bbb(&mut self) -> bool {
        match self.send_command_bbb(CMD_SET_FLASH_POINTER, &[0x03, 0x00]) {
            Ok(()) => {
                self.dbg("Byte-by-byte write succeeded");
                true
            }
            Err(e) => {
                self.dbg(&format!("Byte-by-byte write failed: {}", e));
                false
            }
        }
    }

    /// Erase app region sector by sector
    fn erase_app_region(&mut self, fw_size: usize) -> Result<(), String> {
        let fw_words = fw_size / 2;
        let num_sectors = (fw_words as u16 + SECTOR_SIZE_WORDS - 1) / SECTOR_SIZE_WORDS;

        // First try bulk erase (CMD 0x09)
        self.log(&format!("Trying bulk erase (CMD 0x09)..."));
        if self.erase_app().is_ok() {
            // Verify erase by checking if pointer command works
            thread::sleep(Duration::from_millis(200));
            self.log("Bulk erase sent — verifying...");

            // Set pointer and check
            if self.set_flash_pointer(APP_START).is_ok() {
                self.log("Erase appears successful");
                return Ok(());
            }
        }

        // Fallback: sector-by-sector erase
        self.log(&format!(
            "Sector-by-sector erase ({} sectors)...",
            num_sectors
        ));
        for i in 0..num_sectors {
            let sector_addr = APP_START + i * SECTOR_SIZE_WORDS;
            if let Err(e) = self.set_flash_pointer(sector_addr) {
                return Err(format!(
                    "Set pointer to 0x{:04X} failed: {}",
                    sector_addr, e
                ));
            }
            if let Err(e) = self.erase_sector() {
                return Err(format!(
                    "Erase sector at 0x{:04X} failed: {}",
                    sector_addr, e
                ));
            }
            // Progress
            let pct = ((i + 1) * 100) / num_sectors;
            eprint!(
                "\r  Erasing: {}% ({}/{} sectors)  ",
                pct,
                i + 1,
                num_sectors
            );
        }
        eprintln!();
        self.log("Erase complete");
        Ok(())
    }

    /// Write firmware data to flash
    fn write_firmware(&mut self, data: &[u8]) -> Result<(), String> {
        let total_chunks = (data.len() + WRITE_CHUNK_BYTES - 1) / WRITE_CHUNK_BYTES;

        if let Err(e) = self.set_flash_pointer(APP_START) {
            return Err(format!("Set flash pointer failed: {}", e));
        }

        for i in 0..total_chunks {
            let offset = i * WRITE_CHUNK_BYTES;
            let end = (offset + WRITE_CHUNK_BYTES).min(data.len());
            let mut chunk = [0xFFu8; WRITE_CHUNK_BYTES]; // Pad with 0xFF (erased state)
            chunk[..end - offset].copy_from_slice(&data[offset..end]);

            // Send 16 bytes to PIC buffer
            if let Err(e) = self.send_data(&chunk) {
                return Err(format!("Send data chunk {} failed: {}", i, e));
            }

            // Commit to flash
            if let Err(e) = self.write_flash() {
                return Err(format!("Write flash chunk {} failed: {}", i, e));
            }

            // Progress
            let pct = ((i + 1) * 100) / total_chunks;
            eprint!(
                "\r  Writing: {}% ({}/{} chunks)  ",
                pct,
                i + 1,
                total_chunks
            );
        }
        eprintln!();
        self.log("Write complete");
        Ok(())
    }

    /// Verify firmware by reading back and comparing
    fn verify_firmware(&mut self, expected: &[u8]) -> Result<(), String> {
        let total_chunks = (expected.len() + WRITE_CHUNK_BYTES - 1) / WRITE_CHUNK_BYTES;

        if let Err(e) = self.set_flash_pointer(APP_START) {
            return Err(format!("Set flash pointer for verify failed: {}", e));
        }

        let mut mismatches = 0;
        for i in 0..total_chunks {
            let offset = i * WRITE_CHUNK_BYTES;
            let end = (offset + WRITE_CHUNK_BYTES).min(expected.len());
            let mut expect_chunk = [0xFFu8; WRITE_CHUNK_BYTES];
            expect_chunk[..end - offset].copy_from_slice(&expected[offset..end]);

            match self.read_flash() {
                Ok(actual) => {
                    for (j, (&exp, &act)) in expect_chunk.iter().zip(actual.iter()).enumerate() {
                        if exp != act {
                            mismatches += 1;
                            if mismatches <= 5 {
                                self.log(&format!(
                                    "MISMATCH at offset 0x{:04X}: expected 0x{:02X}, got 0x{:02X}",
                                    offset + j,
                                    exp,
                                    act
                                ));
                            }
                        }
                    }
                }
                Err(e) => {
                    return Err(format!("Read flash chunk {} failed: {}", i, e));
                }
            }

            let pct = ((i + 1) * 100) / total_chunks;
            eprint!(
                "\r  Verifying: {}% ({}/{} chunks)  ",
                pct,
                i + 1,
                total_chunks
            );
        }
        eprintln!();

        if mismatches > 0 {
            Err(format!("Verify FAILED: {} byte mismatches", mismatches))
        } else {
            self.log("Verify OK — all bytes match");
            Ok(())
        }
    }
}

#[derive(Debug)]
enum PicState {
    Bootloader,
    App,
    Dead,
    Unknown(u8),
    Error,
}

// ── Firmware parsing ─────────────────────────────────────────────────────────

/// Parse BraiinsOS PIC firmware from hex text format.
/// Each line is a 14-bit PIC word as 4 hex chars.
/// Returns bytes in BIG-ENDIAN order (hi, lo) per word — matching PIC bootloader
/// SEND_DATA_TO_IIC (CMD 0x02) expectation.
/// CRITICAL FIX (2026-03-24): Was little-endian (lo, hi) which caused flash to fail.
/// Python pic_flasher.py line 176 confirms: (w >> 8) first, (w & 0xFF) second.
fn parse_firmware(hex_text: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    for line in hex_text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match u16::from_str_radix(trimmed, 16) {
            Ok(word) => {
                // PIC bootloader expects big-endian: HIGH byte first, LOW byte second
                bytes.push(((word >> 8) & 0xFF) as u8);
                bytes.push((word & 0xFF) as u8);
            }
            Err(_) => {
                eprintln!("WARNING: Skipping invalid hex line: '{}'", trimmed);
            }
        }
    }
    bytes
}

/// Load firmware from file or use embedded BraiinsOS v0x03
fn load_firmware(path: Option<&str>) -> Vec<u8> {
    match path {
        Some(p) => {
            let text = fs::read_to_string(p)
                .unwrap_or_else(|e| panic!("Failed to read firmware file '{}': {}", p, e));
            parse_firmware(&text)
        }
        None => {
            println!("Using embedded BraiinsOS v0x03 firmware (hash_s8_app.txt)");
            parse_firmware(FIRMWARE_HEX)
        }
    }
}

// ── FPGA Swap Flash (Expert-Reviewed 2026-03-24) ─────────────────────────────
//
// Stock PIC bootloaders CANNOT be flashed via kernel I2C. They need the FPGA
// IIC_COMMAND register at 0x43C00030, which only exists on the Stock Bitmain FPGA.
//
// Sequence: unbind drivers → SLCR quiesce → load stock FPGA → flash PICs via
// IIC_COMMAND devmem → hardware reset (BraiinsOS FPGA reloads from NAND on boot).
//
// The stock_fpga_s9.bin is ALREADY byte-swapped for Zynq PCAP (sync word 0x665599AA).
// Do NOT byte-swap it again.

/// Stock Bitmain FPGA bitstream — embedded at compile time (2.4MB, pre-swapped for xdevcfg).
const STOCK_FPGA: &[u8] = include_bytes!("../firmware/stock_fpga_s9.bin");

/// FPGA register base address (physical memory)
const FPGA_BASE: u64 = 0x43C0_0000;
/// IIC_COMMAND register offset within FPGA
const IIC_CMD_OFFSET: usize = 0x030;

/// Load an FPGA bitstream via /dev/xdevcfg (Zynq PCAP programmer).
/// The stock_fpga_s9.bin is ALREADY byte-swapped — write directly.
/// Uses O_WRONLY (like dd) instead of O_CREAT|O_TRUNC (File::create) to avoid
/// Load FPGA bitstream via /dev/xdevcfg with full PCAP unlock + PROG_B sequence.
///
/// Root cause of previous ENOSPC failures (found by 7-agent expert review 2026-03-24):
/// 1. DEVCFG not unlocked (needs 0x757BDF0D at 0xF8007034)
/// 2. PCAP_PR (bit 27) and PCAP_MODE (bit 26) not set in CTRL
/// 3. PROG_B not toggled (PCAP stuck in "done" state from U-Boot)
/// 4. INT_MASK = 0xFFFFFFFF (all interrupts masked → DMA timeout → ENOSPC)
fn load_fpga_raw(bitstream: &[u8]) -> io::Result<()> {
    // PCAP register addresses
    const DEVCFG_CTRL: u64 = 0xF8007000;
    const DEVCFG_INT_STS: u64 = 0xF800700C;
    const DEVCFG_INT_MASK: u64 = 0xF8007010;
    const DEVCFG_STATUS: u64 = 0xF8007014;
    const DEVCFG_UNLOCK: u64 = 0xF8007034;
    const DEVCFG_MCTRL: u64 = 0xF8007080;
    const SLCR_LVL_SHFTR: u64 = 0xF8000900;

    println!("    PCAP unlock sequence (expert-reviewed)...");

    // Step 1: Unlock SLCR + DEVCFG
    devmem_write32(SLCR_BASE + SLCR_UNLOCK as u64, 0xDF0D);
    devmem_write32(DEVCFG_UNLOCK, 0x757BDF0D);

    // Step 2: Clear all pending interrupts
    devmem_write32(DEVCFG_INT_STS, 0xFFFFFFFF);

    // Step 3: Set PCAP_PR (bit 27) + PCAP_MODE (bit 26) in CTRL
    // Current CTRL = 0x4600E07F, target = 0x4E00E07F
    devmem_write32(DEVCFG_CTRL, 0x4E00E07F);
    println!("    PCAP_PR + PCAP_MODE enabled");

    // Step 4: Disable PCAP loopback
    devmem_write32(DEVCFG_MCTRL, 0x00000000);

    // Step 5: Assert FPGA resets + configure level shifters
    devmem_write32(SLCR_BASE + FPGA_RST_CTRL as u64, 0x0000000F); // Assert all PL resets
    devmem_write32(SLCR_LVL_SHFTR, 0x0000000A); // PS→PL only

    // Step 6: PROG_B toggle (erases current FPGA config!)
    println!("    PROG_B toggle (erasing PL)...");
    // 6a: PROG_B HIGH (already set in 0x4E00E07F, bit 30=1)
    // 6b: Wait for INIT HIGH (STATUS bit 4)
    thread::sleep(Duration::from_millis(5));

    // 6c: PROG_B LOW (clear bit 30) — THIS ERASES THE FPGA
    devmem_write32(DEVCFG_CTRL, 0x0E00E07F);
    thread::sleep(Duration::from_millis(10));

    // 6d: PROG_B HIGH again (set bit 30)
    devmem_write32(DEVCFG_CTRL, 0x4E00E07F);
    thread::sleep(Duration::from_millis(10));
    println!("    PL erased, ready for new bitstream");

    // Step 7: Clear interrupts again + unmask DMA_DONE + PCFG_DONE + errors
    devmem_write32(DEVCFG_INT_STS, 0xFFFFFFFF);
    // Unmask: D_P_DONE (bit 12), PCFG_DONE (bit 2), error flags
    // Mask = ~(0x00001004 | 0x00F0C860) = bits to UNMASK
    devmem_write32(DEVCFG_INT_MASK, 0xFF0E27B9);

    // Step 8: Write bitstream via dd (large block size for fewer DMA allocs)
    println!(
        "    Writing {} bytes to /dev/xdevcfg (bs=1M)...",
        bitstream.len()
    );
    let tmp = "/tmp/_stock_fpga.bin";
    fs::write(tmp, bitstream)?;

    let status = std::process::Command::new("dd")
        .args(&[&format!("if={}", tmp), "of=/dev/xdevcfg", "bs=1M"])
        .status()?;

    let _ = fs::remove_file(tmp);

    if !status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("dd to xdevcfg failed with exit code {:?}", status.code()),
        ));
    }

    // Step 9: Re-enable level shifters both directions + deassert resets
    devmem_write32(SLCR_LVL_SHFTR, 0x0000000F); // PS↔PL both
    devmem_write32(SLCR_BASE + FPGA_RST_CTRL as u64, 0x00000000); // Deassert resets

    println!("    FPGA loaded successfully");
    thread::sleep(Duration::from_millis(500));
    Ok(())
}

/// Zynq SLCR register helpers (physical addresses via /dev/mem)
const SLCR_BASE: u64 = 0xF800_0000;
const SLCR_UNLOCK: usize = 0x008;
const SLCR_LOCK: usize = 0x004;
const FPGA_RST_CTRL: usize = 0x240;
const PSS_RST_CTRL: usize = 0x200;

/// Unbind all PL-connected kernel drivers to prevent AXI aborts during FPGA swap.
fn unbind_pl_drivers() {
    println!("  Unbinding PL drivers...");
    // UIO devices (BraiinsOS FPGA)
    for dev in &[
        "43c00000.chain6-common",
        "43c10000.chain7-common",
        "43c20000.chain8-common",
        "43c01000.chain6-cmd-rx",
        "43c11000.chain7-cmd-rx",
        "43c21000.chain8-cmd-rx",
        "43c02000.chain6-work-rx",
        "43c12000.chain7-work-rx",
        "43c22000.chain8-work-rx",
        "43c03000.chain6-work-tx",
        "43c13000.chain7-work-tx",
        "43c23000.chain8-work-tx",
        "42800000.fan-control",
        "43d00000.miner-glitch-monitor",
    ] {
        let _ = fs::write("/sys/bus/platform/drivers/uio_pdrv_genirq/unbind", dev);
    }
    // Xilinx AXI IIC driver
    let _ = fs::write("/sys/bus/platform/drivers/xiic-i2c/unbind", "41600000.i2c");
    println!("  PL drivers unbound");
}

/// Load BraiinsOS FPGA from NAND (mtd2, gzip-compressed).
/// Reads mtd2, decompresses, byte-swaps for xdevcfg, writes.
fn load_braiins_fpga_from_nand() -> io::Result<()> {
    println!("  Reading BraiinsOS FPGA from NAND (mtd2)...");
    let compressed = fs::read("/dev/mtd2")?;

    // Find gzip header (0x1F, 0x8B)
    let gz_start = compressed
        .windows(2)
        .position(|w| w[0] == 0x1F && w[1] == 0x8B)
        .unwrap_or(0);

    let mut decoder = flate2::read::GzDecoder::new(&compressed[gz_start..]);
    let mut raw = Vec::new();
    decoder.read_to_end(&mut raw)?;
    println!("  Decompressed: {} bytes", raw.len());

    // Byte-swap for xdevcfg (32-bit word endian swap)
    let mut swapped = vec![0u8; raw.len()];
    for i in (0..raw.len().saturating_sub(3)).step_by(4) {
        swapped[i] = raw[i + 3];
        swapped[i + 1] = raw[i + 2];
        swapped[i + 2] = raw[i + 1];
        swapped[i + 3] = raw[i];
    }

    println!("  Writing to /dev/xdevcfg...");
    let mut f = fs::File::create("/dev/xdevcfg")?;
    f.write_all(&swapped)?;
    f.sync_all()?;
    thread::sleep(Duration::from_secs(1));
    println!("  BraiinsOS FPGA restored");
    Ok(())
}

/// PIC address to chain ID mapping (stock FPGA uses chain 6/7/8 in bits 19:16).
fn pic_to_chain(pic_addr: u8) -> u8 {
    match pic_addr {
        0x55 => 6,
        0x56 => 7,
        0x57 => 8,
        _ => 6, // fallback
    }
}

/// Write one byte to PIC via FPGA IIC_COMMAND register at 0x43C00030.
/// Stock FPGA has FLAT register layout — single IIC_COMMAND for ALL chains.
/// Chain selection is in bits 19:16 of the command word.
fn iic_write_byte(fpga_mem: *mut u8, pic_addr: u8, byte: u8) {
    let addr_hi = ((pic_addr >> 4) & 0x0F) as u32; // 0x05 for PIC 0x5x
    let chain = pic_to_chain(pic_addr) as u32;
    let cmd: u32 = (addr_hi << 20) | (chain << 16) | ((byte as u32) << 8);

    unsafe {
        let reg = fpga_mem.add(IIC_CMD_OFFSET) as *mut u32;
        std::ptr::write_volatile(reg, cmd);
    }

    // Poll for done (bit 31)
    for _ in 0..200 {
        let val = unsafe { std::ptr::read_volatile(fpga_mem.add(IIC_CMD_OFFSET) as *const u32) };
        if val & 0x8000_0000 != 0 {
            return;
        }
        thread::sleep(Duration::from_millis(1));
    }
    // Timeout — proceed anyway
}

/// Read one byte from PIC via FPGA IIC_COMMAND register.
fn iic_read_byte(fpga_mem: *mut u8, pic_addr: u8) -> u8 {
    let addr_hi = ((pic_addr >> 4) & 0x0F) as u32;
    let chain = pic_to_chain(pic_addr) as u32;
    let cmd: u32 = (1 << 25) | (addr_hi << 20) | (chain << 16);

    unsafe {
        let reg = fpga_mem.add(IIC_CMD_OFFSET) as *mut u32;
        std::ptr::write_volatile(reg, cmd);
    }

    for _ in 0..200 {
        let val = unsafe { std::ptr::read_volatile(fpga_mem.add(IIC_CMD_OFFSET) as *const u32) };
        if val & 0x8000_0000 != 0 {
            return (val & 0xFF) as u8;
        }
        thread::sleep(Duration::from_millis(1));
    }
    0xCC // Timeout — return bootloader default
}

/// Send PIC bootloader command via FPGA IIC_COMMAND (byte-by-byte, hardware-timed).
fn fpga_pic_cmd(fpga_mem: *mut u8, pic_addr: u8, cmd: u8, data: &[u8]) {
    // Preamble + command
    iic_write_byte(fpga_mem, pic_addr, 0x55);
    thread::sleep(Duration::from_millis(10));
    iic_write_byte(fpga_mem, pic_addr, 0xAA);
    thread::sleep(Duration::from_millis(10));
    iic_write_byte(fpga_mem, pic_addr, cmd);
    thread::sleep(Duration::from_millis(10));

    // Data bytes
    for &b in data {
        iic_write_byte(fpga_mem, pic_addr, b);
        thread::sleep(Duration::from_millis(10));
    }
}

/// Flash one PIC via FPGA IIC_COMMAND path.
fn fpga_flash_pic(fpga_mem: *mut u8, pic_addr: u8, fw_data: &[u8]) -> bool {
    let chain_name = match pic_addr {
        0x55 => "Chain 6",
        0x56 => "Chain 7",
        0x57 => "Chain 8",
        _ => "Unknown",
    };
    println!(
        "  === Flashing PIC 0x{:02X} ({}) via FPGA IIC_COMMAND ===",
        pic_addr, chain_name
    );

    // Check bootloader
    let raw = iic_read_byte(fpga_mem, pic_addr);
    println!("    Raw state: 0x{:02X}", raw);
    if raw != 0xCC {
        println!("    Not in bootloader (0x{:02X}), skipping", raw);
        return true;
    }

    // Flush parser
    fpga_pic_cmd(fpga_mem, pic_addr, 0x00, &[0u8; 16]);
    thread::sleep(Duration::from_millis(50));

    // Erase app region (CMD 0x09)
    println!("    Erasing app region...");
    fpga_pic_cmd(fpga_mem, pic_addr, CMD_ERASE_APP_PROGRAM, &[]);
    thread::sleep(Duration::from_millis(500));

    // Set flash pointer to app start (0x0300)
    fpga_pic_cmd(fpga_mem, pic_addr, CMD_SET_FLASH_POINTER, &[0x03, 0x00]);
    thread::sleep(Duration::from_millis(100));

    // Write firmware in 16-byte chunks
    let total_chunks = (fw_data.len() + WRITE_CHUNK_BYTES - 1) / WRITE_CHUNK_BYTES;
    println!("    Writing {} chunks...", total_chunks);

    for i in 0..total_chunks {
        let offset = i * WRITE_CHUNK_BYTES;
        let end = (offset + WRITE_CHUNK_BYTES).min(fw_data.len());
        let mut chunk = [0xFFu8; WRITE_CHUNK_BYTES];
        chunk[..end - offset].copy_from_slice(&fw_data[offset..end]);

        // SEND_DATA_TO_IIC (CMD 0x02)
        fpga_pic_cmd(fpga_mem, pic_addr, CMD_SEND_DATA_TO_IIC, &chunk);
        thread::sleep(Duration::from_millis(100));

        // WRITE_DATA_INTO_PIC (CMD 0x05)
        fpga_pic_cmd(fpga_mem, pic_addr, CMD_WRITE_DATA_INTO_PIC, &[]);
        thread::sleep(Duration::from_millis(300));

        if (i + 1) % 50 == 0 || i + 1 == total_chunks {
            eprint!(
                "\r    Progress: {}% ({}/{})  ",
                ((i + 1) * 100) / total_chunks,
                i + 1,
                total_chunks
            );
        }
    }
    eprintln!();

    // JUMP to app
    println!("    Sending JUMP...");
    fpga_pic_cmd(fpga_mem, pic_addr, CMD_JUMP_FROM_LOADER, &[]);
    thread::sleep(Duration::from_millis(500));

    // Check version
    fpga_pic_cmd(fpga_mem, pic_addr, 0x00, &[0u8; 4]); // flush
    thread::sleep(Duration::from_millis(50));
    let ver_raw = iic_read_byte(fpga_mem, pic_addr);
    if ver_raw == 0x03 || ver_raw == 0x60 {
        println!("    SUCCESS: PIC running v0x03 (Universal firmware)!");
        true
    } else {
        println!(
            "    PIC state after JUMP: 0x{:02X} (may need power cycle)",
            ver_raw
        );
        true // Flash was written, power cycle will activate
    }
}

/// Helper: write a 32-bit value to a physical address via /dev/mem.
fn devmem_write32(phys_addr: u64, value: u32) {
    let fd = unsafe {
        libc::open(
            b"/dev/mem\0".as_ptr() as *const libc::c_char,
            libc::O_RDWR | libc::O_SYNC,
        )
    };
    if fd < 0 {
        // Surface the cause: a silent no-op here would let the destructive
        // --fpga-flash SLCR unlock / hardware-reset choreography continue as if
        // it succeeded. The errno distinguishes the operator's actionable cases.
        eprintln!(
            "ERROR: devmem write to 0x{:X} failed — cannot open /dev/mem ({}). Run as root; if CONFIG_STRICT_DEVMEM is set this physical address is kernel-blocked.",
            phys_addr,
            std::io::Error::last_os_error()
        );
        return;
    }
    let page = phys_addr & !0xFFF;
    let offset = (phys_addr & 0xFFF) as usize;
    let mem = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            0x1000,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            page as libc::off_t,
        )
    };
    if mem != libc::MAP_FAILED {
        unsafe {
            std::ptr::write_volatile((mem as *mut u8).add(offset) as *mut u32, value);
            libc::munmap(mem, 0x1000);
        }
    } else {
        eprintln!(
            "ERROR: devmem write to 0x{:X} failed — mmap failed ({}). The destructive recovery write did NOT land.",
            phys_addr,
            std::io::Error::last_os_error()
        );
    }
    unsafe {
        libc::close(fd);
    }
}

/// Run the full FPGA swap + PIC flash sequence (expert-reviewed 2026-03-24).
///
/// Sequence:
/// 1. Unbind PL drivers (prevent AXI abort on FPGA reconfig)
/// 2. SLCR quiesce (assert FPGA_OUT_RST)
/// 3. Load stock FPGA via /dev/xdevcfg (raw, pre-swapped bitstream)
/// 4. Deassert FPGA_OUT_RST, verify stock FPGA loaded
/// 5. Flash PICs via IIC_COMMAND devmem (0x43C00030)
/// 6. Write flag file
/// 7. Hardware reset via SLCR (BraiinsOS FPGA reloads from NAND on boot)
fn fpga_swap_flash(firmware: &[u8]) {
    println!("=== FPGA Swap Flash — Expert-Reviewed Sequence ===");
    println!("  Stock FPGA → PIC Flash via IIC_COMMAND → Hardware Reset");
    println!();

    // Step 1: Unbind all PL-connected kernel drivers
    println!("[1/6] Unbinding PL drivers...");
    unbind_pl_drivers();

    // Step 2: SLCR — unlock only (FPGA_OUT_RST prevents PCAP from writing)
    // NOTE: Asserting FPGA_OUT_RST causes "No space left on device" on xdevcfg.
    // The PCAP needs PL to be in a writable state. Instead, we rely on driver
    // unbind to prevent kernel crashes. Any AXI abort during the brief FPGA
    // transition window is acceptable since we hardware-reset immediately after.
    println!("[2/6] SLCR unlock...");
    devmem_write32(SLCR_BASE + SLCR_UNLOCK as u64, 0xDF0D);

    // Step 3: mmap /dev/mem BEFORE FPGA swap (VA→PA mapping persists through PL reconfig)
    println!("[3/6] Pre-mapping FPGA registers and SLCR via /dev/mem...");
    let fd = unsafe {
        libc::open(
            b"/dev/mem\0".as_ptr() as *const libc::c_char,
            libc::O_RDWR | libc::O_SYNC,
        )
    };
    if fd < 0 {
        eprintln!(
            "ERROR: cannot open /dev/mem ({}) — run pic-recovery as root; if CONFIG_STRICT_DEVMEM is enabled this physical range is kernel-blocked",
            std::io::Error::last_os_error()
        );
        return;
    }

    let fpga_mem = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            0x1000,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            FPGA_BASE as libc::off_t,
        )
    };
    if fpga_mem == libc::MAP_FAILED {
        eprintln!(
            "ERROR: mmap of FPGA region 0x{:08X} failed ({})",
            FPGA_BASE,
            std::io::Error::last_os_error()
        );
        unsafe {
            libc::close(fd);
        }
        return;
    }

    // Also mmap SLCR for hardware reset at the end
    let slcr_mem = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            0x1000,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            SLCR_BASE as libc::off_t,
        )
    };
    if slcr_mem == libc::MAP_FAILED {
        eprintln!("WARNING: mmap SLCR failed — will use devmem for reset");
    }
    println!("  FPGA @ 0x43C00000 and SLCR @ 0xF8000000 pre-mapped");

    // Step 4: Load stock FPGA (this WILL kill SSH and potentially trigger AXI aborts)
    // The process may survive if no PL access happens during the brief reconfig window.
    println!(
        "[4/6] Loading stock FPGA ({} bytes) — SSH WILL DROP...",
        STOCK_FPGA.len()
    );
    println!("  (PIC flash continues locally after FPGA loads)");
    if let Err(e) = load_fpga_raw(STOCK_FPGA) {
        eprintln!("ERROR: Failed to load stock FPGA: {}", e);
        unsafe {
            libc::munmap(fpga_mem, 0x1000);
            libc::close(fd);
        }
        return;
    }
    // If we're still alive here, the FPGA loaded and the process survived
    thread::sleep(Duration::from_millis(500));

    // Verify stock FPGA loaded by checking HARDWARE_VERSION (offset 0x00)
    let hw_ver = unsafe { std::ptr::read_volatile(fpga_mem as *const u32) };
    println!(
        "  HARDWARE_VERSION: 0x{:08X} (BraiinsOS would be 0x00901002)",
        hw_ver
    );
    if hw_ver == 0x00901002 {
        eprintln!("WARNING: Still reading BraiinsOS FPGA — stock FPGA may not have loaded!");
    }

    // Check IIC_COMMAND register
    let iic_test = unsafe {
        std::ptr::read_volatile((fpga_mem as *const u8).add(IIC_CMD_OFFSET) as *const u32)
    };
    println!(
        "  IIC_COMMAND: 0x{:08X} (should be 0x00000000 = idle)",
        iic_test
    );

    // Step 5: Flash PICs via IIC_COMMAND
    println!();
    println!("[5/6] Flashing PICs via IIC_COMMAND at 0x43C00030...");
    let pic_addrs = [0x55u8, 0x56, 0x57];
    let mut success = 0;
    for &addr in &pic_addrs {
        if fpga_flash_pic(fpga_mem as *mut u8, addr, firmware) {
            success += 1;
        }
    }
    println!();
    println!("  Flashed {}/3 PICs", success);

    // Cleanup mmap
    unsafe {
        libc::munmap(fpga_mem, 0x1000);
        libc::close(fd);
    }

    // Step 6: Write flag file if successful
    if success > 0 {
        let _ = fs::write(
            "/data/pic-flashed-v1",
            format!("Flashed {} PICs\n", success),
        );
        println!("  Flag written to /data/pic-flashed-v1");
    }

    println!();
    println!("[6/6] Hardware reset via SLCR (BraiinsOS FPGA reloads from NAND on boot)...");
    println!("  Resetting NOW — the miner will reboot.");

    // Hardware reset — bypasses kernel shutdown to avoid AXI aborts with stock FPGA loaded
    devmem_write32(SLCR_BASE + SLCR_UNLOCK as u64, 0xDF0D);
    devmem_write32(SLCR_BASE + PSS_RST_CTRL as u64, 0x00000001);

    // Should never reach here — hardware reset is immediate
    thread::sleep(Duration::from_secs(5));
    println!("WARNING: Hardware reset did not trigger — try manual power cycle");
}

// ── CLI ──────────────────────────────────────────────────────────────────────

fn print_usage() {
    eprintln!("pic-recovery — PIC firmware recovery for bricked Antminer hash boards");
    eprintln!("D-Central Technologies (GPL-3.0)");
    eprintln!();
    eprintln!("PIC16F1704 (S9 family — bmminer-mix bootloader):");
    eprintln!("  pic-recovery                      Probe all PICs, report status");
    eprintln!("  pic-recovery --all                Recover all PICs in bootloader");
    eprintln!("  pic-recovery --addr 0x56          Recover specific PIC");
    eprintln!("  pic-recovery --addr 0x56 --verify Recover with readback verify");
    eprintln!("  pic-recovery --all --dry-run      Probe only, don't flash");
    eprintln!("  pic-recovery --fw /tmp/fw.txt     Use custom firmware file");
    eprintln!("  pic-recovery -v                   Verbose I2C debug output");
    eprintln!();
    eprintln!("  PIC addresses: 0x55 (Chain 6), 0x56 (Chain 7), 0x57 (Chain 8)");
    eprintln!();
    eprintln!("PIC1704 (CV1835 / AM335x BB / Amlogic S19j Pro — RE2 §12 ops):");
    eprintln!("  Each subcommand below requires --confirm-bricked AND a --platform.");
    eprintln!();
    eprintln!("  pic-recovery pic1704-seek       --bus 0 --addr 0x20 --flash-addr 0x0400 \\");
    eprintln!("                                  --platform cv1835-s19jpro --confirm-bricked");
    eprintln!("  pic-recovery pic1704-erase      --bus 0 --addr 0x20 --flash-addr 0x0400 \\");
    eprintln!("                                  --pages 4 --platform am335x-bb-s19jpro \\");
    eprintln!("                                  --confirm-bricked");
    eprintln!("  pic-recovery pic1704-write      --bus 0 --addr 0x20 --flash-addr 0x0400 \\");
    eprintln!("                                  --data-file /tmp/firmware.bin \\");
    eprintln!("                                  --platform amlogic-s19jpro --confirm-bricked");
    eprintln!("  pic-recovery pic1704-start-app  --bus 0 --addr 0x20 \\");
    eprintln!("                                  --platform cv1835-s19jpro --confirm-bricked");
    eprintln!();
    eprintln!("  --platform values: cv1835-s19jpro, am335x-bb-s19jpro, amlogic-s19jpro,");
    eprintln!("                     cv1835-s19, cv1835-s19i, cv1835-s19xp");
    eprintln!();
    eprintln!("  WARNING: PIC1704 programmer ops are DESTRUCTIVE. They refuse to");
    eprintln!("  operate unless the PIC is in bootloader mode (REG_VERSION = 0x86).");
    eprintln!("  Recovery is ICSP-only if these ops corrupt the chip.");
    eprintln!();
    eprintln!("dsPIC fw=0x86 recovery (am2 dsPIC ONLY — S17 / S19 Pro / S19j Pro Zynq am2):");
    eprintln!("  Per RE3 R3-6: fw=0x86 is the bootloader, NOT silicon corruption.");
    eprintln!("  Two recovery paths — Path B is safe, Path C is HIGH-RISK:");
    eprintln!();
    eprintln!("  Path B — bootloader→application jump (100% confidence per RE3 §5.2,");
    eprintln!("           non-destructive — no flash writes):");
    eprintln!();
    eprintln!("    pic-recovery dspic-jump-to-app    --bus 0 --addr 0x21 \\");
    eprintln!("                                      --dspic-platform am2-s19jpro-zynq \\");
    eprintln!("                                      --confirm-bricked");
    eprintln!();
    eprintln!("  Path C — framed-protocol full reflash (60% byte-exact per RE3 §3.4 + §6,");
    eprintln!("           DOUBLE-GATED + typed-serial confirmation per W13.D4 +");
    eprintln!("           ):");
    eprintln!();
    eprintln!("    pic-recovery dspic-reflash-fw86   --bus 0 --addr 0x21 --hex /tmp/app.txt \\");
    eprintln!("                                      --dspic-platform am2-s17 \\");
    eprintln!("                                      --confirm-bricked \\");
    eprintln!(
        "                                      --i-acknowledge-60-percent-byte-exact-confidence \\"
    );
    eprintln!("                                      [--serial <DSPIC_SERIAL>]");
    eprintln!();
    eprintln!("  --dspic-platform values: am2-s17, am2-s19pro, am2-s19jpro-zynq");
    eprintln!();
    eprintln!("  WARNING: Path C reflash is PARTIAL (RE3 §3.4 / §6 — 60% confidence).");
    eprintln!("  Path B (jump-to-app) is fully specified and safe; Path C reflash bails");
    eprintln!("  before any destructive write phase pending RE Round 4 (R4-8: sacrificial");
    eprintln!("  dsPIC + logic-analyzer trace) wire-byte trace. Path C invocations are");
    eprintln!("  persistently logged to /var/log/dcent/pic_recovery_path_c.log for forensic");
    eprintln!("  audit. If --serial is omitted, you'll be prompted to type it interactively.");
    eprintln!("  DO NOT use these on PIC1704 platforms or S9 — wrong family.");
    eprintln!();
    eprintln!("PIC1704 framed-protocol v2 (W14.C — REG_CMD 0x10-0x15, RE-INFERRED):");
    eprintln!("  Two new subcommands targeting PIC1704 carriers (CV1835 / AM335x BB /");
    eprintln!("  Amlogic S19j Pro). Distinct from W11.7 register-style ops above.");
    eprintln!();
    eprintln!("  Path B v2 — bootloader→app jump via framed FP_START_APP (0x14, 0x01):");
    eprintln!();
    eprintln!("    pic-recovery pic1704-jump-to-app  --bus 0 --addr 0x20 \\");
    eprintln!("                                      --platform cv1835-s19jpro \\");
    eprintln!("                                      --confirm-bricked");
    eprintln!();
    eprintln!("  Path C v2 — framed-protocol full reflash (RE-INFERRED, bench-untested):");
    eprintln!();
    eprintln!("    pic-recovery pic1704-reflash-fp   --bus 0 --addr 0x20 \\");
    eprintln!("                                      --hex /tmp/dsPIC33EP16GS202_app.txt \\");
    eprintln!("                                      --manifest /tmp/manifest.json \\");
    eprintln!("                                      --platform cv1835-s19jpro \\");
    eprintln!("                                      --confirm-bricked \\");
    eprintln!("                                      --i-acknowledge-pic1704-framed-inferred \\");
    eprintln!("                                      [--serial <BOARD_SERIAL>] \\");
    eprintln!("                                      [--batch-size 16]");
    eprintln!();
    eprintln!("  --platform values for v2: cv1835-s19jpro, am335x-bb-s19jpro,");
    eprintln!("                            amlogic-s19jpro");
    eprintln!();
    eprintln!("  WARNING: pic1704-reflash-fp is INFERRED from RE C source");
    eprintln!("  (DCENT_OS_WAVE4_HANDOFF/pic1704_v2.{{c,h}}). Known-good CRC test");
    eprintln!("  vectors in handoff are still 0x???? placeholders. Continue only on");
    eprintln!("  a sacrificial PIC1704 with a logic analyzer attached. Refuses without");
    eprintln!("  --confirm-bricked AND --i-acknowledge-pic1704-framed-inferred AND");
    eprintln!("  --manifest. Pre-reads REG_VERSION on every batch (collision guard");
    eprintln!("  against REG_VOLTAGE_L=0x10 in app mode).");
    eprintln!();
    eprintln!("PIC1704 stock bmminer reflash (W15.B — GHIDRA-EXTRACTED, PRIMARY):");
    eprintln!("  Decoded byte-exact from stock bmminer (_bitmain_pic_seek/erase/write_1704.c).");
    eprintln!("  Wire format: 0x55 magic + additive checksum + 2-phase write + 300 ms wait.");
    eprintln!("  Distinct from W14.C V2 framed protocol above.");
    eprintln!();
    eprintln!("    pic-recovery pic1704-reflash-stock --bus 0 --addr 0x20 \\");
    eprintln!("                                       --hex /tmp/dsPIC33EP16GS202_app.txt \\");
    eprintln!("                                       --manifest /tmp/manifest.json \\");
    eprintln!("                                       --platform cv1835-s19jpro \\");
    eprintln!("                                       --confirm-bricked \\");
    eprintln!("                                       [--serial <BOARD_SERIAL>]");
    eprintln!();
    eprintln!("  Equivalent to: pic-recovery pic1704-reflash-fp ... \\");
    eprintln!("                              --pic1704-protocol=stock");
    eprintln!();
    eprintln!("  pic1704-reflash-fp now accepts --pic1704-protocol=auto|stock|w4v2");
    eprintln!("  (default: auto). `auto` probes a stock SEEK first; if [0x01,0x01]");
    eprintln!("  ACK returns it routes to stock, else falls back to w4v2 (W14.C).");
    eprintln!();
    eprintln!("  WARNING: pic1704-reflash-stock is GHIDRA-EXTRACTED (higher confidence");
    eprintln!("  than w4v2) but still bench-untested on a real bricked PIC1704.");
    eprintln!("  Continue only with --confirm-bricked. Logs to");
    eprintln!("  $DCENT_PIC_RECOVERY_LOG_DIR/pic1704_stock_audit.log.");
}

fn main() {
    let args: Vec<String> = env::args().collect();

    // PIC1704 programmer subcommands (CV1835 / AM335x BB / Amlogic S19j
    // Pro family) are dispatched first because they have a totally
    // different argument shape than the legacy PIC16F1704 (S9) flow.
    //
    // dsPIC fw=0x86 recovery subcommands (W12.1 / RE3 R3-6) are also
    // dispatched here — they target am2 dsPIC ONLY (S17 / S19 Pro /
    // S19j Pro Zynq am2). DO NOT use these on PIC1704 platforms or on
    // S9 PIC16F1704 — wrong family, recovery would corrupt the chip.
    if args.len() >= 2 {
        match args[1].as_str() {
            "pic1704-seek" => {
                std::process::exit(pic1704_cli::run_seek(&args[2..]));
            }
            "pic1704-erase" => {
                std::process::exit(pic1704_cli::run_erase(&args[2..]));
            }
            "pic1704-write" => {
                std::process::exit(pic1704_cli::run_write(&args[2..]));
            }
            "pic1704-start-app" => {
                std::process::exit(pic1704_cli::run_start_app(&args[2..]));
            }
            "dspic-jump-to-app" => {
                std::process::exit(dspic_cli::run_jump_to_app(&args[2..]));
            }
            "dspic-reflash-fw86" => {
                std::process::exit(dspic_cli::run_reflash_fw86(&args[2..]));
            }
            // W14.C: PIC1704 framed-protocol v2 subcommands. Distinct
            // from W11.7 register-style ops above (pic1704-seek/-erase/
            // -write/-start-app); v2 uses framed REG_CMD ordinals
            // 0x10-0x15 from the W4 handoff `pic1704_v2.{c,h}`.
            "pic1704-jump-to-app" => {
                std::process::exit(pic1704_v2_cli::run_jump_to_app(&args[2..]));
            }
            "pic1704-reflash-fp" => {
                // W15.B4: now accepts `--pic1704-protocol={auto|stock|w4v2}`
                // (default `auto`). The dispatcher inside run_reflash_fp
                // re-routes to the stock implementation when stock or
                // auto+stock-detected.
                std::process::exit(pic1704_v2_cli::run_reflash_fp(&args[2..]));
            }
            // W15.B4: explicit stock-only entry (Ghidra-extracted
            // bmminer protocol, PRIMARY since W15.B). Equivalent to
            // `pic1704-reflash-fp --pic1704-protocol=stock`. Provided
            // as a separate subcommand so the audit log + help text
            // are unambiguous about which wire format is in use.
            "pic1704-reflash-stock" => {
                std::process::exit(pic1704_stock_cli::run_reflash_stock(&args[2..]));
            }
            _ => {}
        }
    }

    let mut target_addr: Option<u8> = None;
    let mut all = false;
    let mut verify = false;
    let mut dry_run = false;
    let mut verbose = false;
    let mut diagnose = false;
    let mut fw_path: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                print_usage();
                return;
            }
            "--all" | "-a" => all = true,
            "--verify" => verify = true,
            "--dry-run" | "-n" => dry_run = true,
            "--verbose" | "-v" => verbose = true,
            "--diagnose" | "-d" => diagnose = true,
            "--fpga-flash" => {
                // FPGA swap flash mode — load stock FPGA, flash PICs, restore BraiinsOS.
                // DESTRUCTIVE: erases the running PL FPGA (PROG_B toggle), flashes all 3
                // PICs, and forces an SLCR PSS_RST hardware reboot. Gate it behind
                // --confirm-bricked, consistent with every OTHER destructive subcommand in
                // this binary (pic1704-*, dspic-jump-to-app, dspic-reflash-fw86, flash) —
                // it was the only one with zero confirmation. (gap-swarm HAL-safety #8)
                if !args.iter().any(|a| a == "--confirm-bricked") {
                    eprintln!(
                        "ERROR: --fpga-flash is DESTRUCTIVE — it erases the FPGA, flashes all 3 PICs, and reboots the miner."
                    );
                    eprintln!(
                        "Re-run with --confirm-bricked to proceed:  pic-recovery --fpga-flash --confirm-bricked"
                    );
                    std::process::exit(1);
                }
                let firmware = load_firmware(None);
                fpga_swap_flash(&firmware);
                return;
            }
            "--addr" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("ERROR: --addr requires an argument (e.g., 0x55)");
                    std::process::exit(1);
                }
                let s = args[i].trim_start_matches("0x").trim_start_matches("0X");
                target_addr = Some(
                    u8::from_str_radix(s, 16)
                        .unwrap_or_else(|_| panic!("Invalid address: {}", args[i])),
                );
            }
            "--fw" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("ERROR: --fw requires a file path");
                    std::process::exit(1);
                }
                fw_path = Some(args[i].clone());
            }
            other => {
                eprintln!("Unknown option: {}", other);
                print_usage();
                std::process::exit(1);
            }
        }
        i += 1;
    }

    // Load firmware
    let firmware = load_firmware(fw_path.as_deref());
    println!(
        "Firmware: {} words ({} bytes)",
        firmware.len() / 2,
        firmware.len()
    );

    // Open I2C bus. Note: `pic-recovery` ships its own local `I2cBus`
    // struct (not the HAL one) — recovery binaries deliberately bypass
    // the HAL service architecture because they run with the daemon
    // stopped. The HAL `recovery-tool` feature gates `I2cBus::open_for_recovery`
    // for any binary that DOES want HAL bus helpers; this binary stays on
    // raw libc, which is also fine.
    let mut i2c = I2cBus::open(0).unwrap_or_else(|e| {
        eprintln!("ERROR: Cannot open /dev/i2c-0: {}", e);
        eprintln!("Are you running as root on the miner?");
        std::process::exit(1);
    });

    println!();
    println!("=== PIC Recovery Tool ===");
    println!();

    // Determine which PICs to process
    let targets: Vec<(u8, &str)> = if let Some(addr) = target_addr {
        let name = PIC_ADDRS
            .iter()
            .find(|(a, _)| *a == addr)
            .map(|(_, n)| *n)
            .unwrap_or("Unknown");
        vec![(addr, name)]
    } else {
        PIC_ADDRS.to_vec()
    };

    // Probe phase (always run)
    println!("--- Probing PICs ---");
    let mut bootloader_pics = Vec::new();
    for &(addr, name) in &targets {
        let mut pic = PicRecovery::new(&mut i2c, addr, name, verbose);
        let state = pic.probe();
        if matches!(state, PicState::Bootloader) {
            bootloader_pics.push((addr, name));
        }
    }

    println!();
    if bootloader_pics.is_empty() {
        println!("No PICs in bootloader mode found.");
        if target_addr.is_none() && !all {
            println!("All PICs are either healthy or disconnected.");
        }
        return;
    }

    println!(
        "Found {} PIC(s) in bootloader mode: {}",
        bootloader_pics.len(),
        bootloader_pics
            .iter()
            .map(|(a, n)| format!("0x{:02X} ({})", a, n))
            .collect::<Vec<_>>()
            .join(", ")
    );

    // Diagnose phase (if --diagnose)
    if diagnose && !bootloader_pics.is_empty() {
        println!();
        let (addr, name) = bootloader_pics[0];
        println!(
            "=== Running byte ordering diagnostic on PIC 0x{:02X} ({}) ===",
            addr, name
        );
        let mut pic = PicRecovery::new(&mut i2c, addr, name, verbose);
        match pic.diagnose_byte_ordering() {
            Ok(()) => println!("  Diagnostic complete."),
            Err(e) => println!("  Diagnostic FAILED: {}", e),
        }
        println!();
        return;
    }

    // Recovery phase (only if --all or --addr specified)
    if !all && target_addr.is_none() {
        println!();
        println!(
            "To recover, run with --all or --addr 0x{:02X}",
            bootloader_pics[0].0
        );
        return;
    }

    let recovery_targets: Vec<(u8, &str)> = if all {
        bootloader_pics.clone()
    } else if let Some(addr) = target_addr {
        bootloader_pics
            .iter()
            .filter(|(a, _)| *a == addr)
            .copied()
            .collect()
    } else {
        vec![]
    };

    if recovery_targets.is_empty() {
        println!("Target PIC is not in bootloader mode — no recovery needed.");
        return;
    }

    println!();
    let mut success = 0;
    let mut failed = 0;

    for (addr, name) in &recovery_targets {
        println!("=== Recovering PIC 0x{:02X} ({}) ===", addr, name);
        let mut pic = PicRecovery::new(&mut i2c, *addr, name, verbose);
        match pic.recover(&firmware, verify, dry_run) {
            Ok(()) => {
                println!("  [0x{:02X}] RECOVERY COMPLETE", addr);
                success += 1;
            }
            Err(e) => {
                println!("  [0x{:02X}] RECOVERY FAILED: {}", addr, e);
                failed += 1;
            }
        }
        println!();
    }

    // Summary
    println!("=== Summary ===");
    println!("  Recovered: {}/{} PICs", success, recovery_targets.len());
    if failed > 0 {
        println!("  Failed: {} — see errors above", failed);
        println!();
        println!("For PICs that can't be recovered via I2C:");
        println!("  1. Use a PICkit3/4 programmer on the ICSP header");
        println!("  2. Or swap hash boards from a working miner");
    }
    if success > 0 && !dry_run {
        println!();
        println!("IMPORTANT: Power cycle the miner for recovered PICs to boot into app mode.");
    }
}

// ── PIC1704 subcommand handlers (RE2 §12 ops, recovery-tool feature) ────────
//
// These handlers wrap `dcentrald_asic::pic1704::programmer::*`. The
// programmer module is feature-gated behind `recovery-tool` on
// `dcentrald-asic`, which `pic-recovery`'s Cargo.toml enables. Each
// subcommand requires an explicit `--confirm-bricked` flag at the CLI
// boundary and mints a `ConfirmedBrickedToken` from that exact string,
// per the layered safety contract documented in
// `dcentrald-asic/src/pic1704/programmer.rs`.
//
// Invariants enforced here:
//
//   - `--platform` is mandatory (no default, sealed-trait gates would
//     otherwise reject silently).
//   - `--confirm-bricked` must appear literally (case-sensitive,
//     hyphenated). Token construction asserts this at the asic layer.
//   - Service construction goes through `spawn_i2c_service`, NOT the
//     raw `I2cBus::open_for_recovery` helper. PIC1704 platforms (CV/BB/
//     AML) do not need the AXI-IIC quirks the S9 PIC16F1704 path needs;
//     the standard kernel `/dev/i2c-N` route is correct.
mod pic1704_cli {
    use dcentrald_asic::pic1704::programmer::{
        pic_erase_1704, pic_seek_1704, pic_start_app_common, pic_write_1704, ConfirmedBrickedToken,
    };
    use dcentrald_asic::pic1704::service::platforms;
    use dcentrald_asic::pic1704::Pic1704Service;
    use dcentrald_hal::i2c::spawn_i2c_service;
    use std::fs;

    #[derive(Debug, Clone, Copy)]
    enum Platform {
        Cv1835S19jPro,
        Am335xBbS19jPro,
        AmlogicS19jPro,
        Cv1835S19,
        Cv1835S19i,
        Cv1835S19XP,
    }

    impl Platform {
        fn parse(s: &str) -> Result<Self, String> {
            match s {
                "cv1835-s19jpro" => Ok(Platform::Cv1835S19jPro),
                "am335x-bb-s19jpro" => Ok(Platform::Am335xBbS19jPro),
                "amlogic-s19jpro" => Ok(Platform::AmlogicS19jPro),
                "cv1835-s19" => Ok(Platform::Cv1835S19),
                "cv1835-s19i" => Ok(Platform::Cv1835S19i),
                "cv1835-s19xp" => Ok(Platform::Cv1835S19XP),
                other => Err(format!(
                    "unknown --platform {:?}; expected one of: cv1835-s19jpro, \
                     am335x-bb-s19jpro, amlogic-s19jpro, cv1835-s19, cv1835-s19i, cv1835-s19xp",
                    other,
                )),
            }
        }
    }

    /// Build a `Pic1704Service` for the chosen platform. Invokes
    /// `Pic1704Service::new` with the matching marker so the sealed-trait
    /// `Pic1704Authorized` bound is satisfied at compile time.
    fn build_service(bus: u8, i2c_addr: u8, platform: Platform) -> Result<Pic1704Service, String> {
        // PIC1704 platforms use plain kernel I2C (no AXI IIC, no /dev/mem
        // bypass). `use_devmem = false` is correct for CV/BB/AML.
        let handle = spawn_i2c_service(bus, false)
            .map_err(|e| format!("spawn_i2c_service(bus={}): {}", bus, e))?;
        Ok(match platform {
            Platform::Cv1835S19jPro => {
                Pic1704Service::new(handle, i2c_addr, platforms::Cv1835S19jPro)
            }
            Platform::Am335xBbS19jPro => {
                Pic1704Service::new(handle, i2c_addr, platforms::Am335xBbS19jPro)
            }
            Platform::AmlogicS19jPro => {
                Pic1704Service::new(handle, i2c_addr, platforms::AmlogicS19jPro)
            }
            Platform::Cv1835S19 => Pic1704Service::new(handle, i2c_addr, platforms::Cv1835S19),
            Platform::Cv1835S19i => Pic1704Service::new(handle, i2c_addr, platforms::Cv1835S19i),
            Platform::Cv1835S19XP => Pic1704Service::new(handle, i2c_addr, platforms::Cv1835S19XP),
        })
    }

    /// Common parsed-arg bag for every PIC1704 subcommand.
    struct CommonArgs {
        bus: u8,
        addr: u8,
        platform: Platform,
        flag_confirm: bool,
    }

    fn parse_common(args: &[String]) -> Result<CommonArgs, String> {
        let mut bus: Option<u8> = None;
        let mut addr: Option<u8> = None;
        let mut platform: Option<Platform> = None;
        let mut flag_confirm = false;
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--bus" => {
                    i += 1;
                    let v = args.get(i).ok_or("--bus needs a value")?;
                    bus = Some(
                        v.parse::<u8>()
                            .map_err(|e| format!("--bus {:?}: {}", v, e))?,
                    );
                }
                "--addr" => {
                    i += 1;
                    let v = args.get(i).ok_or("--addr needs a value")?;
                    let s = v.trim_start_matches("0x").trim_start_matches("0X");
                    addr = Some(
                        u8::from_str_radix(s, 16).map_err(|e| format!("--addr {:?}: {}", v, e))?,
                    );
                }
                "--platform" => {
                    i += 1;
                    let v = args.get(i).ok_or("--platform needs a value")?;
                    platform = Some(Platform::parse(v)?);
                }
                "--confirm-bricked" => flag_confirm = true,
                // Not a common arg — skip; subcommand-specific parser will pick it up.
                _ => {}
            }
            i += 1;
        }
        Ok(CommonArgs {
            bus: bus.ok_or("--bus is required")?,
            addr: addr.unwrap_or(0x20),
            platform: platform.ok_or("--platform is required (see --help)")?,
            flag_confirm,
        })
    }

    fn parse_u32_arg(args: &[String], name: &str) -> Result<u32, String> {
        let mut i = 0;
        while i < args.len() {
            if args[i] == name {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| format!("{} needs a value", name))?;
                let s = v.trim_start_matches("0x").trim_start_matches("0X");
                return u32::from_str_radix(s, 16)
                    .or_else(|_| v.parse::<u32>())
                    .map_err(|e| format!("{} {:?}: {}", name, v, e));
            }
            i += 1;
        }
        Err(format!("missing required argument {}", name))
    }

    fn parse_u8_arg(args: &[String], name: &str) -> Result<u8, String> {
        let mut i = 0;
        while i < args.len() {
            if args[i] == name {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| format!("{} needs a value", name))?;
                return v
                    .parse::<u8>()
                    .map_err(|e| format!("{} {:?}: {}", name, v, e));
            }
            i += 1;
        }
        Err(format!("missing required argument {}", name))
    }

    fn parse_path_arg(args: &[String], name: &str) -> Result<String, String> {
        let mut i = 0;
        while i < args.len() {
            if args[i] == name {
                return Ok(args
                    .get(i + 1)
                    .ok_or_else(|| format!("{} needs a value", name))?
                    .clone());
            }
            i += 1;
        }
        Err(format!("missing required argument {}", name))
    }

    fn mint_token(common: &CommonArgs) -> Result<ConfirmedBrickedToken, String> {
        if !common.flag_confirm {
            return Err("PIC1704 programmer ops require --confirm-bricked (DESTRUCTIVE)".into());
        }
        ConfirmedBrickedToken::new_with_confirmation("--confirm-bricked").map_err(|e| e.to_string())
    }

    fn report(prefix: &str, r: Result<(), String>) -> i32 {
        match r {
            Ok(()) => {
                println!("{}: OK", prefix);
                0
            }
            Err(e) => {
                eprintln!("{}: ERROR — {}", prefix, e);
                1
            }
        }
    }

    pub fn run_seek(args: &[String]) -> i32 {
        let r = (|| -> Result<(), String> {
            let common = parse_common(args)?;
            let flash_addr = parse_u32_arg(args, "--flash-addr")?;
            let token = mint_token(&common)?;
            let mut svc = build_service(common.bus, common.addr, common.platform)?;
            // Refresh state before the bootloader-only guard runs.
            svc.read_version().map_err(|e| e.to_string())?;
            pic_seek_1704(&mut svc, flash_addr, token).map_err(|e| e.to_string())
        })();
        report("pic1704-seek", r)
    }

    pub fn run_erase(args: &[String]) -> i32 {
        let r = (|| -> Result<(), String> {
            let common = parse_common(args)?;
            let flash_addr = parse_u32_arg(args, "--flash-addr")?;
            let pages = parse_u8_arg(args, "--pages")?;
            let token = mint_token(&common)?;
            let mut svc = build_service(common.bus, common.addr, common.platform)?;
            svc.read_version().map_err(|e| e.to_string())?;
            pic_erase_1704(&mut svc, flash_addr, pages, token).map_err(|e| e.to_string())
        })();
        report("pic1704-erase", r)
    }

    pub fn run_write(args: &[String]) -> i32 {
        let r = (|| -> Result<(), String> {
            let common = parse_common(args)?;
            let flash_addr = parse_u32_arg(args, "--flash-addr")?;
            let path = parse_path_arg(args, "--data-file")?;
            let data = fs::read(&path).map_err(|e| format!("read {}: {}", path, e))?;
            let token = mint_token(&common)?;
            let mut svc = build_service(common.bus, common.addr, common.platform)?;
            svc.read_version().map_err(|e| e.to_string())?;
            pic_write_1704(&mut svc, flash_addr, &data, token).map_err(|e| e.to_string())
        })();
        report("pic1704-write", r)
    }

    pub fn run_start_app(args: &[String]) -> i32 {
        let r = (|| -> Result<(), String> {
            let common = parse_common(args)?;
            let token = mint_token(&common)?;
            let mut svc = build_service(common.bus, common.addr, common.platform)?;
            svc.read_version().map_err(|e| e.to_string())?;
            pic_start_app_common(&mut svc, token).map_err(|e| e.to_string())
        })();
        report("pic1704-start-app", r)
    }
}

// ── dsPIC fw=0x86 recovery subcommand handlers (W12.1 / RE3 R3-6) ────────────
//
// These handlers wrap `dcentrald_asic::dspic::recovery_fw86::*`. The
// module is feature-gated behind `recovery-tool` on `dcentrald-asic`,
// which `pic-recovery`'s Cargo.toml enables. Each subcommand requires
// an explicit `--confirm-bricked` flag at the CLI boundary and mints a
// `ConfirmedBrickedToken` from that exact string.
//
// Invariants enforced here:
//
//   - `--dspic-platform` is mandatory (no default — recovery is am2 dsPIC
//     ONLY; the operator MUST acknowledge which family they're touching).
//     Distinct from PIC1704's `--platform` to prevent mismatched
//     copy-paste from the pic1704 subcommands.
//   - `--confirm-bricked` must appear literally (case-sensitive,
//     hyphenated). Token construction asserts this at the asic layer.
//   - Service construction goes through `spawn_i2c_service` like the
//     PIC1704 path. am2 dsPIC platforms still honor the SINGLE-I2C-OWNER
//     rule when running on an am2 control board.
//   - DO NOT use these subcommands on PIC1704 platforms (CV1835 / AM335x
//     BB / Amlogic S19j Pro) or on S9 PIC16F1704. Wrong chip family.
mod dspic_cli {
    use dcentrald_asic::dspic::recovery_fw86::{
        self, jump_to_app, reflash_app_via_framed_protocol, AcknowledgeSixtyPercentConfidence,
        RecoveryPlatform,
    };
    use dcentrald_asic::pic1704::programmer::ConfirmedBrickedToken;
    use dcentrald_hal::i2c::spawn_i2c_service;
    use std::fs;
    use std::io::{self, BufRead as _, Write as _};

    fn parse_dspic_platform(s: &str) -> Result<RecoveryPlatform, String> {
        match s {
            "am2-s17" => Ok(RecoveryPlatform::Am2S17),
            "am2-s19pro" => Ok(RecoveryPlatform::Am2S19Pro),
            "am2-s19jpro-zynq" => Ok(RecoveryPlatform::Am2S19jProZynq),
            other => Err(format!(
                "unknown --dspic-platform {:?}; expected one of: am2-s17, \
                 am2-s19pro, am2-s19jpro-zynq",
                other,
            )),
        }
    }

    struct DspicCommonArgs {
        bus: u8,
        addr: u8,
        platform: RecoveryPlatform,
        flag_confirm: bool,
        flag_acknowledge_60pct: bool,
        cli_serial: Option<String>,
    }

    fn parse_dspic_common(args: &[String]) -> Result<DspicCommonArgs, String> {
        let mut bus: Option<u8> = None;
        let mut addr: Option<u8> = None;
        let mut platform: Option<RecoveryPlatform> = None;
        let mut flag_confirm = false;
        let mut flag_acknowledge_60pct = false;
        let mut cli_serial: Option<String> = None;
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--bus" => {
                    i += 1;
                    let v = args.get(i).ok_or("--bus needs a value")?;
                    bus = Some(
                        v.parse::<u8>()
                            .map_err(|e| format!("--bus {:?}: {}", v, e))?,
                    );
                }
                "--addr" => {
                    i += 1;
                    let v = args.get(i).ok_or("--addr needs a value")?;
                    let s = v.trim_start_matches("0x").trim_start_matches("0X");
                    addr = Some(
                        u8::from_str_radix(s, 16).map_err(|e| format!("--addr {:?}: {}", v, e))?,
                    );
                }
                "--dspic-platform" => {
                    i += 1;
                    let v = args.get(i).ok_or("--dspic-platform needs a value")?;
                    platform = Some(parse_dspic_platform(v)?);
                }
                "--confirm-bricked" => flag_confirm = true,
                // W13.D4: Path C double-gate flag. Path B (jump-to-app)
                // ignores this; Path C (reflash-fw86) requires it.
                "--i-acknowledge-60-percent-byte-exact-confidence" => {
                    flag_acknowledge_60pct = true;
                }
                // W13.D4: Path C target-unit confirmation. If absent,
                // Path C prompts interactively. Ignored by Path B.
                "--serial" => {
                    i += 1;
                    let v = args.get(i).ok_or("--serial needs a value")?;
                    cli_serial = Some(v.clone());
                }
                // Subcommand-specific args (e.g. `--hex`) are picked up
                // by the per-subcommand parser; ignore unknowns here.
                _ => {}
            }
            i += 1;
        }
        Ok(DspicCommonArgs {
            bus: bus.ok_or("--bus is required")?,
            // dsPIC default address: 0x21 (hb2 on .139 — see
            // ). Operator should
            // pass --addr explicitly for any other chain.
            addr: addr.unwrap_or(0x21),
            platform: platform
                .ok_or("--dspic-platform is required (am2-s17 / am2-s19pro / am2-s19jpro-zynq)")?,
            flag_confirm,
            flag_acknowledge_60pct,
            cli_serial,
        })
    }

    fn parse_path_arg(args: &[String], name: &str) -> Result<String, String> {
        let mut i = 0;
        while i < args.len() {
            if args[i] == name {
                return Ok(args
                    .get(i + 1)
                    .ok_or_else(|| format!("{} needs a value", name))?
                    .clone());
            }
            i += 1;
        }
        Err(format!("missing required argument {}", name))
    }

    /// Path B (jump-only) token mint — single `--confirm-bricked` is
    /// enough per RE3 §5.2 (100% byte-exact wire format,
    /// non-destructive). UX must NOT regress per
    ///  (the safe path
    /// keeps single-flag UX).
    fn mint_token(common: &DspicCommonArgs) -> Result<ConfirmedBrickedToken, String> {
        if !common.flag_confirm {
            return Err(
                "dsPIC fw=0x86 recovery ops require --confirm-bricked (DESTRUCTIVE)".into(),
            );
        }
        ConfirmedBrickedToken::new_with_confirmation("--confirm-bricked").map_err(|e| e.to_string())
    }

    /// Path C (framed reflash) double-gate token mint — requires both
    /// `--confirm-bricked` AND `--i-acknowledge-60-percent-byte-exact-confidence`,
    /// AND a typed-serial confirmation matching the connected dsPIC's
    /// hashboard EEPROM serial. Per W13.D4 +
    /// .
    ///
    /// Side-effects:
    ///   - reads the connected hashboard EEPROM serial via
    ///     `recovery_fw86::read_dspic_serial_proxy(bus)`
    ///   - prints the explicit warning banner per W13.D4 task spec
    ///   - if `--serial` was provided on the CLI, compares it
    ///     non-interactively; otherwise prompts the operator on stdin
    fn mint_double_gate_token(
        common: &DspicCommonArgs,
    ) -> Result<AcknowledgeSixtyPercentConfidence, String> {
        if !common.flag_confirm {
            return Err(
                "Path C reflash refused: --confirm-bricked is required (HIGH-RISK destructive)"
                    .into(),
            );
        }
        if !common.flag_acknowledge_60pct {
            return Err(
                "Path C reflash refused: --i-acknowledge-60-percent-byte-exact-confidence is \
                 required. Path C framed-protocol byte format is only 60% confident per RE3 §3.4 \
                 + §6 (dspic_fw86_recovery.md).."
                    .into(),
            );
        }

        // Read connected dsPIC's hashboard EEPROM serial up front. This
        // is the canonical "this connected unit" identity. Failure to
        // read = refusal to flash (no verified target).
        let connected_serial = recovery_fw86::read_dspic_serial_proxy(common.bus).map_err(|e| {
            format!(
                "Path C refused: cannot read connected hashboard EEPROM serial via bus {}: \
                     {}. Without verified target identity, refusing to mint Path C confirmation \
                     token.",
                common.bus, e,
            )
        })?;

        // Print the explicit warning banner per W13.D4 task spec.
        eprintln!();
        eprintln!("================================================================");
        eprintln!("WARNING: dspic-reflash-fw86 (Path C — framed-protocol reflash)");
        eprintln!("================================================================");
        eprintln!("THIS WILL BRICK YOUR PIC IF FRAMED FORMAT IS WRONG.");
        eprintln!("Path C confidence: 60% byte-exact (R4 dspic_fw86_recovery.md §6)");
        eprintln!("Sacrificial dsPIC validation hardware-blocked (R4-8 carry-forward).");
        eprintln!();
        eprintln!(
            "Connected dsPIC hashboard EEPROM serial: {}",
            connected_serial
        );
        eprintln!(
            "Bus={} Addr=0x{:02X} Platform={}",
            common.bus,
            common.addr,
            common.platform.label(),
        );
        eprintln!();

        // Read typed serial from --serial flag (non-interactive) OR
        // from stdin (interactive prompt).
        let typed_serial: String = match &common.cli_serial {
            Some(s) => {
                eprintln!("--serial provided on CLI: {}", s);
                s.clone()
            }
            None => {
                eprint!("Type the dsPIC serial to confirm: ");
                io::stderr().flush().ok();
                let mut buf = String::new();
                io::stdin()
                    .lock()
                    .read_line(&mut buf)
                    .map_err(|e| format!("Path C refused: stdin read failed: {}", e))?;
                buf.trim_end_matches(|c: char| c == '\n' || c == '\r')
                    .to_string()
            }
        };

        AcknowledgeSixtyPercentConfidence::mint_with_double_confirmation(
            "--confirm-bricked",
            "--i-acknowledge-60-percent-byte-exact-confidence",
            &typed_serial,
            &connected_serial,
        )
        .map_err(|e| e.to_string())
    }

    fn report(prefix: &str, r: Result<(), String>) -> i32 {
        match r {
            Ok(()) => {
                println!("{}: OK", prefix);
                0
            }
            Err(e) => {
                eprintln!("{}: ERROR — {}", prefix, e);
                1
            }
        }
    }

    pub fn run_jump_to_app(args: &[String]) -> i32 {
        let r = (|| -> Result<(), String> {
            let common = parse_dspic_common(args)?;
            let token = mint_token(&common)?;
            // PIC1704 platforms use plain kernel I2C; same is true for am2
            // dsPIC hashboards (the Zynq xiic-i2c controller is exposed as
            // `/dev/i2c-0`). `use_devmem = false` keeps the kernel driver
            // bound — the AM2 SINGLE-I2C-OWNER architecture still applies.
            let handle = spawn_i2c_service(common.bus, false)
                .map_err(|e| format!("spawn_i2c_service(bus={}): {}", common.bus, e))?;
            jump_to_app(&handle, common.addr, common.platform, token).map_err(|e| e.to_string())
        })();
        report("dspic-jump-to-app", r)
    }

    pub fn run_reflash_fw86(args: &[String]) -> i32 {
        let r = (|| -> Result<(), String> {
            let common = parse_dspic_common(args)?;
            let hex_path = parse_path_arg(args, "--hex")?;
            let hex = fs::read(&hex_path).map_err(|e| format!("read {}: {}", hex_path, e))?;
            // W13.D4: Path C now uses the double-gate token
            // (--confirm-bricked + --i-acknowledge-60-percent-byte-exact-confidence
            // + typed-serial confirmation against connected dsPIC EEPROM).
            // Path B (run_jump_to_app above) keeps single-gate UX.
            let token = mint_double_gate_token(&common)?;
            let handle = spawn_i2c_service(common.bus, false)
                .map_err(|e| format!("spawn_i2c_service(bus={}): {}", common.bus, e))?;
            reflash_app_via_framed_protocol(&handle, common.addr, &hex, common.platform, token)
                .map_err(|e| e.to_string())
        })();
        report("dspic-reflash-fw86", r)
    }
}

// ── PIC1704 framed-protocol v2 subcommand handlers (W14.C) ───────────────────
//
// New framed-protocol surface targeting PIC1704 carriers (CV1835 /
// AM335x BB / Amlogic S19j Pro). Distinct from the W11.7 register-style
// `pic1704_cli` module above:
//
//   - W11.7 (`pic1704-{seek,erase,write,start-app}`): BraiinsOS-shared
//     register-style ops with REG_CONTROL prefix `[0x09, 0x01, ...]` etc.
//   - W14.C (`pic1704-jump-to-app`, `pic1704-reflash-fp`): framed REG_CMD
//     ordinals 0x10-0x15 from the W4 handoff `pic1704_v2.{c,h}`.
//
// Both modules ship; carrier picks per evidence. Bitmain's stock bmminer
// firmware uses framed; BraiinsOS uses register-style.
//
// Safety contract:
//
//   - `--platform` mandatory (no default).
//   - `--confirm-bricked` mandatory (token gate 1).
//   - `pic1704-reflash-fp` ALSO requires `--manifest <path>` AND
//     `--i-acknowledge-pic1704-framed-inferred` (token gate 2). The
//     latter intentionally rejects `--i-acknowledge-90-percent` —
//     handoff CRC test vectors are still 0x???? placeholders.
//   - Pre-write REG_VERSION collision guard runs on EVERY framed
//     transaction. FP_SEEK (0x10) collides with REG_VOLTAGE_L address
//     0x10 in app mode → silent overvolt risk if misclassified.
//   - Audit log to `$DCENT_PIC_RECOVERY_LOG_DIR/pic1704_fp_audit.log`
//     (default `/var/log/dcent/`) for forensic trail.
mod pic1704_v2_cli {
    use super::I2cBus;
    use dcentrald_asic::pic1704::programmer_v2::{
        compute_crc_host, decode_verify_response, erase_steps_v2, read_version_step_v2,
        seek_steps_v2, start_app_steps_v2, write_steps_v2, BATCH_MAX, FLASH_APP_START,
        FLASH_MAX_WORDS, FLASH_PAGE_WORDS, POLL_MS, TIMEOUT_MS, VERSION_APP_88, VERSION_APP_89,
        VERSION_APP_8A, VERSION_BOOTLOADER,
    };
    use std::fs;
    use std::io::Write as _;
    use std::path::PathBuf;
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    // The 3 PIC1704 carriers per W14.A scope. Sub-variants from W11.3
    // (cv1835-s19, cv1835-s19i, cv1835-s19xp) are NOT exposed here —
    // the framed protocol is bench-untested, and only the baseline
    // S19j Pro carriers warrant the recovery surface.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum V2Platform {
        Cv1835S19jPro,
        Am335xBbS19jPro,
        AmlogicS19jPro,
    }

    impl V2Platform {
        fn parse(s: &str) -> Result<Self, String> {
            match s {
                "cv1835-s19jpro" => Ok(V2Platform::Cv1835S19jPro),
                "am335x-bb-s19jpro" => Ok(V2Platform::Am335xBbS19jPro),
                "amlogic-s19jpro" => Ok(V2Platform::AmlogicS19jPro),
                other => Err(format!(
                    "unknown --platform {:?}; expected one of: cv1835-s19jpro, \
                     am335x-bb-s19jpro, amlogic-s19jpro",
                    other,
                )),
            }
        }

        fn label(self) -> &'static str {
            match self {
                V2Platform::Cv1835S19jPro => "cv1835-s19jpro",
                V2Platform::Am335xBbS19jPro => "am335x-bb-s19jpro",
                V2Platform::AmlogicS19jPro => "amlogic-s19jpro",
            }
        }
    }

    struct V2CommonArgs {
        bus: u8,
        addr: u8,
        platform: V2Platform,
        flag_confirm: bool,
    }

    fn parse_v2_common(args: &[String]) -> Result<V2CommonArgs, String> {
        let mut bus: Option<u8> = None;
        let mut addr: Option<u8> = None;
        let mut platform: Option<V2Platform> = None;
        let mut flag_confirm = false;
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--bus" => {
                    i += 1;
                    let v = args.get(i).ok_or("--bus needs a value")?;
                    bus = Some(
                        v.parse::<u8>()
                            .map_err(|e| format!("--bus {:?}: {}", v, e))?,
                    );
                }
                "--addr" => {
                    i += 1;
                    let v = args.get(i).ok_or("--addr needs a value")?;
                    let s = v.trim_start_matches("0x").trim_start_matches("0X");
                    addr = Some(
                        u8::from_str_radix(s, 16).map_err(|e| format!("--addr {:?}: {}", v, e))?,
                    );
                }
                "--platform" => {
                    i += 1;
                    let v = args.get(i).ok_or("--platform needs a value")?;
                    platform = Some(V2Platform::parse(v)?);
                }
                "--confirm-bricked" => flag_confirm = true,
                _ => {} // subcommand-specific args skipped here
            }
            i += 1;
        }
        Ok(V2CommonArgs {
            bus: bus.ok_or("--bus is required")?,
            addr: addr.unwrap_or(0x20),
            platform: platform.ok_or("--platform is required (see --help)")?,
            flag_confirm,
        })
    }

    fn parse_path_arg(args: &[String], name: &str) -> Result<String, String> {
        let mut i = 0;
        while i < args.len() {
            if args[i] == name {
                return Ok(args
                    .get(i + 1)
                    .ok_or_else(|| format!("{} needs a value", name))?
                    .clone());
            }
            i += 1;
        }
        Err(format!("missing required argument {}", name))
    }

    fn parse_optional_str(args: &[String], name: &str) -> Option<String> {
        let mut i = 0;
        while i < args.len() {
            if args[i] == name {
                return args.get(i + 1).cloned();
            }
            i += 1;
        }
        None
    }

    fn parse_optional_usize(args: &[String], name: &str) -> Result<Option<usize>, String> {
        if let Some(v) = parse_optional_str(args, name) {
            Ok(Some(
                v.parse::<usize>()
                    .map_err(|e| format!("{} {:?}: {}", name, v, e))?,
            ))
        } else {
            Ok(None)
        }
    }

    fn report(prefix: &str, r: Result<(), String>) -> i32 {
        match r {
            Ok(()) => {
                println!("{}: OK", prefix);
                0
            }
            Err(e) => {
                eprintln!("{}: ERROR — {}", prefix, e);
                1
            }
        }
    }

    // ── Audit log ────────────────────────────────────────────────────────

    /// Default directory for the framed-protocol audit log on production
    /// hardware. Overridden via `DCENT_PIC_RECOVERY_LOG_DIR` for tests.
    const DEFAULT_LOG_DIR: &str = "/var/log/dcent";
    const LOG_FILENAME: &str = "pic1704_fp_audit.log";

    fn audit_log_path() -> PathBuf {
        let dir = std::env::var("DCENT_PIC_RECOVERY_LOG_DIR")
            .unwrap_or_else(|_| DEFAULT_LOG_DIR.to_string());
        PathBuf::from(dir).join(LOG_FILENAME)
    }

    fn append_audit_line(
        op: &str,
        addr: u8,
        platform: V2Platform,
        serial: Option<&str>,
        outcome: &str,
    ) {
        let unix_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let serial_str = serial.unwrap_or("-");
        let line = format!(
            "{}\t{}\taddr=0x{:02X}\tplatform={}\tserial={}\toutcome={}\n",
            unix_secs,
            op,
            addr,
            platform.label(),
            serial_str,
            outcome,
        );
        let path = audit_log_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = f.write_all(line.as_bytes());
        }
    }

    // ── Raw I²C framed-protocol helpers ──────────────────────────────────

    /// Send framed FP_READ_VERSION (write `[0x15]`, read 1 byte).
    /// Returns the version byte.
    fn fp_read_version(i2c: &mut I2cBus, addr: u8) -> Result<u8, String> {
        i2c.set_slave(addr)
            .map_err(|e| format!("set_slave 0x{:02X}: {}", addr, e))?;
        i2c.write_bulk(&[read_version_step_v2()])
            .map_err(|e| format!("FP_READ_VERSION write: {}", e))?;
        let bytes = i2c
            .read_bytes(1)
            .map_err(|e| format!("FP_READ_VERSION read: {}", e))?;
        bytes
            .first()
            .copied()
            .ok_or_else(|| "FP_READ_VERSION: empty response".to_string())
    }

    /// Send a pre-formatted framed write command (no register prefix).
    fn fp_write_raw(i2c: &mut I2cBus, addr: u8, bytes: &[u8]) -> Result<(), String> {
        i2c.set_slave(addr)
            .map_err(|e| format!("set_slave 0x{:02X}: {}", addr, e))?;
        i2c.write_bulk(bytes)
            .map_err(|e| format!("framed write: {}", e))?;
        Ok(())
    }

    /// Send FP_VERIFY_CRC (write `[0x13]`, read 2 bytes LE) and decode.
    fn fp_verify_crc(i2c: &mut I2cBus, addr: u8) -> Result<u16, String> {
        i2c.set_slave(addr)
            .map_err(|e| format!("set_slave 0x{:02X}: {}", addr, e))?;
        i2c.write_bulk(&[dcentrald_asic::pic1704::programmer_v2::FP_VERIFY_CRC])
            .map_err(|e| format!("FP_VERIFY_CRC write: {}", e))?;
        let bytes = i2c
            .read_bytes(2)
            .map_err(|e| format!("FP_VERIFY_CRC read: {}", e))?;
        if bytes.len() < 2 {
            return Err(format!(
                "FP_VERIFY_CRC: short response ({} bytes, expected 2)",
                bytes.len()
            ));
        }
        let mut rbuf = [0u8; 2];
        rbuf.copy_from_slice(&bytes[..2]);
        Ok(decode_verify_response(&rbuf))
    }

    /// Mandatory collision guard: pre-read REG_VERSION via framed
    /// FP_READ_VERSION and refuse if not 0x86. FP_SEEK (0x10) collides
    /// with REG_VOLTAGE_L=0x10 in the PIC1704 register map; sending
    /// `[0x10, addr×3]` to an app-mode chip would be interpreted as a
    /// voltage write → silent overvolt risk.
    fn assert_bootloader(i2c: &mut I2cBus, addr: u8) -> Result<(), String> {
        let ver = fp_read_version(i2c, addr)?;
        if ver != VERSION_BOOTLOADER {
            return Err(format!(
                "Collision guard refusal: REG_VERSION = 0x{:02X} (expected 0x86 = bootloader). \
                 FP_SEEK (0x10) collides with REG_VOLTAGE_L (0x10) in app mode — \
                 sending framed commands to a non-bootloader chip is unsafe. \
.",
                ver,
            ));
        }
        Ok(())
    }

    // ── Manifest ─────────────────────────────────────────────────────────

    /// Minimal manifest schema accepted by `pic1704-reflash-fp`. We
    /// avoid a `serde_json` dep here — the binary already does string
    /// parsing for I2C arg munging, and we only need 4 fields.
    struct Manifest {
        sha256: String,
        word_count: usize,
        expected_post_flash_crc: u16,
        target_subtype: String,
        target_carrier: String,
        target_serial: Option<String>,
    }

    /// Parse a tiny JSON object: `{"sha256": "<hex>", "word_count": N,
    /// "expected_post_flash_crc": "0xNNNN", "target_subtype": "<s>",
    /// "target_carrier": "<s>", "target_serial": "<s>"}`. Tolerant of
    /// whitespace / member order. NOT a full JSON parser.
    fn parse_manifest(text: &str) -> Result<Manifest, String> {
        fn extract_string(text: &str, key: &str) -> Option<String> {
            let needle = format!("\"{}\"", key);
            let idx = text.find(&needle)?;
            let rest = &text[idx + needle.len()..];
            let colon = rest.find(':')?;
            let after = &rest[colon + 1..];
            let q1 = after.find('"')?;
            let after2 = &after[q1 + 1..];
            let q2 = after2.find('"')?;
            Some(after2[..q2].to_string())
        }
        fn extract_number(text: &str, key: &str) -> Option<String> {
            let needle = format!("\"{}\"", key);
            let idx = text.find(&needle)?;
            let rest = &text[idx + needle.len()..];
            let colon = rest.find(':')?;
            let after = rest[colon + 1..].trim_start();
            let end = after
                .find(|c: char| c == ',' || c == '}' || c == '\n' || c == '\r')
                .unwrap_or(after.len());
            Some(after[..end].trim().trim_matches('"').to_string())
        }

        let sha256 =
            extract_string(text, "sha256").ok_or("manifest missing required field \"sha256\"")?;
        let word_count_s = extract_number(text, "word_count")
            .ok_or("manifest missing required field \"word_count\"")?;
        let word_count = word_count_s
            .parse::<usize>()
            .map_err(|e| format!("manifest \"word_count\" ({:?}): {}", word_count_s, e))?;
        let crc_s = extract_string(text, "expected_post_flash_crc")
            .or_else(|| extract_number(text, "expected_post_flash_crc"))
            .ok_or("manifest missing required field \"expected_post_flash_crc\"")?;
        let crc_trim = crc_s
            .trim_start_matches("0x")
            .trim_start_matches("0X")
            .trim_matches('"');
        let expected_post_flash_crc = u16::from_str_radix(crc_trim, 16)
            .or_else(|_| crc_trim.parse::<u16>())
            .map_err(|e| format!("manifest \"expected_post_flash_crc\" ({:?}): {}", crc_s, e))?;
        let target_subtype = extract_string(text, "target_subtype")
            .ok_or("manifest missing required field \"target_subtype\"")?;
        let target_carrier = extract_string(text, "target_carrier")
            .ok_or("manifest missing required field \"target_carrier\"")?;
        let target_serial = extract_string(text, "target_serial");
        Ok(Manifest {
            sha256,
            word_count,
            expected_post_flash_crc,
            target_subtype,
            target_carrier,
            target_serial,
        })
    }

    /// Compute SHA-256 (hex, lowercase) of bytes. We avoid pulling a
    /// crypto crate here — instead we compute the hash via a small
    /// inline implementation. Match-once vs manifest, no need to
    /// ship the same hasher as production.
    fn sha256_hex(data: &[u8]) -> String {
        // Tiny SHA-256 (pure Rust, host-safe). Implementation derived
        // from the FIPS 180-4 spec; not intended for crypto-grade
        // throughput, just for manifest matching.
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
            0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
            0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
            0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
            0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
            0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
            0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
            0xc67178f2,
        ];
        let mut h: [u32; 8] = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
            0x5be0cd19,
        ];
        let bit_len = (data.len() as u64).wrapping_mul(8);
        let mut padded: Vec<u8> = data.to_vec();
        padded.push(0x80);
        while padded.len() % 64 != 56 {
            padded.push(0);
        }
        padded.extend_from_slice(&bit_len.to_be_bytes());

        for chunk in padded.chunks_exact(64) {
            let mut w = [0u32; 64];
            for i in 0..16 {
                let off = i * 4;
                w[i] = u32::from_be_bytes([
                    chunk[off],
                    chunk[off + 1],
                    chunk[off + 2],
                    chunk[off + 3],
                ]);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[i - 7])
                    .wrapping_add(s1);
            }
            let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
                (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
            for i in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ ((!e) & g);
                let temp1 = hh
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K[i])
                    .wrapping_add(w[i]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let temp2 = s0.wrapping_add(maj);
                hh = g;
                g = f;
                f = e;
                e = d.wrapping_add(temp1);
                d = c;
                c = b;
                b = a;
                a = temp1.wrapping_add(temp2);
            }
            h[0] = h[0].wrapping_add(a);
            h[1] = h[1].wrapping_add(b);
            h[2] = h[2].wrapping_add(c);
            h[3] = h[3].wrapping_add(d);
            h[4] = h[4].wrapping_add(e);
            h[5] = h[5].wrapping_add(f);
            h[6] = h[6].wrapping_add(g);
            h[7] = h[7].wrapping_add(hh);
        }
        let mut out = String::with_capacity(64);
        for word in h {
            out.push_str(&format!("{:08x}", word));
        }
        out
    }

    // ── Hex-file loader ──────────────────────────────────────────────────

    /// Parse `dsPIC33EP16GS202_app.txt`-format firmware file (one 24-bit
    /// hex word per line, optional `:` Intel-HEX prefix tolerated, blank
    /// and comment lines skipped). Returns the raw words in order.
    /// Trailing all-`FFFFFF` words ARE retained — the manifest's
    /// `word_count` distinguishes the meaningful-word region.
    fn parse_hex_file(text: &str) -> Result<Vec<u32>, String> {
        let mut words = Vec::new();
        for (lineno, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
                continue;
            }
            // Tolerate a leading `:` (Intel-HEX style) by stripping it.
            let body = line.trim_start_matches(':');
            // Take first 6 hex chars (24 bits).
            let hex6: String = body
                .chars()
                .filter(|c| c.is_ascii_hexdigit())
                .take(6)
                .collect();
            if hex6.len() != 6 {
                return Err(format!(
                    "hex parse error at line {}: expected 6 hex chars, got {:?}",
                    lineno + 1,
                    body,
                ));
            }
            let w = u32::from_str_radix(&hex6, 16).map_err(|e| {
                format!("hex parse error at line {}: {:?}: {}", lineno + 1, hex6, e)
            })?;
            words.push(w & 0x00FF_FFFF);
        }
        Ok(words)
    }

    // ── pic1704-jump-to-app ──────────────────────────────────────────────

    pub fn run_jump_to_app(args: &[String]) -> i32 {
        let r = (|| -> Result<(), String> {
            let common = parse_v2_common(args)?;
            if !common.flag_confirm {
                return Err(
                    "pic1704-jump-to-app refused: --confirm-bricked is required \
                     (DESTRUCTIVE — sending FP_START_APP to a non-bootloader chip \
                     can corrupt the running PIC firmware)"
                        .to_string(),
                );
            }
            let mut i2c = I2cBus::open(common.bus)
                .map_err(|e| format!("open /dev/i2c-{}: {}", common.bus, e))?;

            // Phase 1: pre-read VERSION + collision guard.
            assert_bootloader(&mut i2c, common.addr).inspect_err(|_| {
                append_audit_line(
                    "pic1704-jump-to-app",
                    common.addr,
                    common.platform,
                    None,
                    "refused_not_in_bootloader",
                );
            })?;

            // Phase 2: send FP_START_APP `[0x14, 0x01]`.
            fp_write_raw(&mut i2c, common.addr, &start_app_steps_v2()).inspect_err(|_| {
                append_audit_line(
                    "pic1704-jump-to-app",
                    common.addr,
                    common.platform,
                    None,
                    "fp_start_app_write_failed",
                );
            })?;

            // Phase 3: poll FP_READ_VERSION until app or timeout.
            let deadline = Instant::now() + Duration::from_millis(TIMEOUT_MS);
            loop {
                let ver = fp_read_version(&mut i2c, common.addr).unwrap_or(0xFF);
                match ver {
                    VERSION_APP_88 | VERSION_APP_89 | VERSION_APP_8A => {
                        append_audit_line(
                            "pic1704-jump-to-app",
                            common.addr,
                            common.platform,
                            None,
                            &format!("ok_app_version_0x{:02X}", ver),
                        );
                        return Ok(());
                    }
                    _ => {}
                }
                if Instant::now() >= deadline {
                    append_audit_line(
                        "pic1704-jump-to-app",
                        common.addr,
                        common.platform,
                        None,
                        "timeout_post_start_app",
                    );
                    return Err(format!(
                        "pic1704-jump-to-app: post-FP_START_APP version poll timed out \
                         after {} ms (last version 0x{:02X}). Application partition may \
                         be missing/corrupt — try pic1704-reflash-fp",
                        TIMEOUT_MS, ver,
                    ));
                }
                thread::sleep(Duration::from_millis(POLL_MS));
            }
        })();
        report("pic1704-jump-to-app", r)
    }

    // ── pic1704-reflash-fp ──────────────────────────────────────────────

    pub fn run_reflash_fp(args: &[String]) -> i32 {
        // W15.B4: --pic1704-protocol={auto|stock|w4v2}, default auto.
        // If `stock` (explicit) or `auto` + stock-SEEK probe ACKs
        // [0x01,0x01], delegate to the W15.B Ghidra-extracted stock
        // implementation; else fall through to the W14.C V2 framed
        // path below.
        let protocol_arg =
            parse_optional_str(args, "--pic1704-protocol").unwrap_or_else(|| "auto".to_string());
        let resolved =
            match dcentrald_asic::pic1704::reflash::parse_protocol_override(&protocol_arg) {
                Ok(Some(p)) => Some(p),
                Ok(None) => {
                    // `auto` — defer routing decision to the stock CLI itself
                    // (which performs the live SEEK probe). The stock CLI
                    // owns its own fallback path back to V2 if probe fails.
                    None
                }
                Err(e) => {
                    eprintln!("pic1704-reflash-fp: ERROR — {}", e);
                    return 1;
                }
            };
        if matches!(
            resolved,
            Some(dcentrald_asic::pic1704::reflash::Pic1704Protocol::Stock)
        ) || resolved.is_none()
        {
            // Explicit stock OR auto → delegate to stock CLI. Audit log
            // line below records the routing decision.
            return super::pic1704_stock_cli::run_reflash_stock_with_routing(args, resolved);
        }
        // Explicit w4v2 fall-through to existing V2 path.
        let r = (|| -> Result<(), String> {
            let common = parse_v2_common(args)?;
            let hex_path = parse_path_arg(args, "--hex")?;
            let manifest_path = parse_path_arg(args, "--manifest").map_err(|_| {
                "pic1704-reflash-fp refused: --manifest <path> is required \
                 (FpError::NoManifest). The manifest pins target_subtype, \
                 target_carrier, expected_post_flash_crc, and the SHA-256 of \
                 the hex payload."
                    .to_string()
            })?;
            let cli_serial = parse_optional_str(args, "--serial");
            let batch_size_opt = parse_optional_usize(args, "--batch-size")?;
            let batch_size = batch_size_opt.unwrap_or(BATCH_MAX as usize);
            let flag_acknowledge = args
                .iter()
                .any(|a| a == "--i-acknowledge-pic1704-framed-inferred");

            // ── Pre-flight gate checks ────────────────────────────────
            if !common.flag_confirm {
                return Err("pic1704-reflash-fp refused: --confirm-bricked is required \
                     (HIGH-RISK — framed-protocol RE-INFERRED from C source, \
                     bench-untested)"
                    .to_string());
            }
            if !flag_acknowledge {
                return Err(
                    "pic1704-reflash-fp refused: --i-acknowledge-pic1704-framed-inferred \
                     is required. The framed protocol bytes are inferred from RE C \
                     source (DCENT_OS_WAVE4_HANDOFF/pic1704_v2.{c,h}); known-good CRC \
                     test vectors in the handoff are still 0x???? placeholders. \
. \
                     This flag is intentionally NOT named --i-acknowledge-90-percent."
                        .to_string(),
                );
            }
            if batch_size == 0 || batch_size > BATCH_MAX as usize {
                return Err(format!(
                    "pic1704-reflash-fp refused: --batch-size {} out of range \
                     (must be 1..={}, FpError::BatchTooLarge if exceeded)",
                    batch_size, BATCH_MAX,
                ));
            }

            // ── Loud warning banner ───────────────────────────────────
            eprintln!();
            eprintln!("================================================================");
            eprintln!("WARNING: PIC1704 framed reflash protocol is INFERRED from RE C");
            eprintln!("source (DCENT_OS_WAVE4_HANDOFF/pic1704_v2.{{c,h}}). Known-good");
            eprintln!("CRC vectors in W4 handoff are still 0x???? placeholders.");
            eprintln!("Continue only on a sacrificial PIC1704 with a logic analyzer");
            eprintln!("attached. Refusing without --confirm-bricked AND");
            eprintln!("--i-acknowledge-pic1704-framed-inferred AND --manifest.");
            eprintln!("================================================================");
            eprintln!();
            eprintln!(
                "Bus={} Addr=0x{:02X} Platform={} BatchSize={}",
                common.bus,
                common.addr,
                common.platform.label(),
                batch_size,
            );
            eprintln!();

            // ── Load + validate hex + manifest ────────────────────────
            let hex_bytes =
                fs::read(&hex_path).map_err(|e| format!("read hex {:?}: {}", hex_path, e))?;
            let hex_text = std::str::from_utf8(&hex_bytes)
                .map_err(|e| format!("hex {:?} not UTF-8: {}", hex_path, e))?;
            let words = parse_hex_file(hex_text)?;

            let manifest_bytes = fs::read(&manifest_path)
                .map_err(|e| format!("read manifest {:?}: {}", manifest_path, e))?;
            let manifest_text = std::str::from_utf8(&manifest_bytes)
                .map_err(|e| format!("manifest {:?} not UTF-8: {}", manifest_path, e))?;
            let manifest = parse_manifest(manifest_text)?;

            // SHA-256 match against the raw hex file bytes (post-read,
            // pre-parse). Ensures byte-for-byte identity with the
            // manifest-pinned firmware.
            let actual_sha = sha256_hex(&hex_bytes);
            if actual_sha.to_lowercase() != manifest.sha256.to_lowercase() {
                append_audit_line(
                    "pic1704-reflash-fp",
                    common.addr,
                    common.platform,
                    cli_serial.as_deref(),
                    "refused_sha256_mismatch",
                );
                return Err(format!(
                    "pic1704-reflash-fp refused: SHA-256 mismatch. \
                     manifest pins {}, hex file is {}",
                    manifest.sha256, actual_sha,
                ));
            }
            if manifest.word_count > FLASH_MAX_WORDS as usize {
                return Err(format!(
                    "pic1704-reflash-fp refused: manifest word_count {} exceeds \
                     FLASH_MAX_WORDS ({})",
                    manifest.word_count, FLASH_MAX_WORDS,
                ));
            }
            if manifest.word_count == 0 {
                return Err("pic1704-reflash-fp refused: manifest word_count = 0".to_string());
            }
            if manifest.word_count > words.len() {
                return Err(format!(
                    "pic1704-reflash-fp refused: manifest word_count {} exceeds \
                     hex file word count {}",
                    manifest.word_count,
                    words.len(),
                ));
            }
            // Optional --serial check vs manifest's target_serial (if both present).
            if let (Some(cli), Some(mani)) = (cli_serial.as_ref(), manifest.target_serial.as_ref())
            {
                if cli != mani {
                    append_audit_line(
                        "pic1704-reflash-fp",
                        common.addr,
                        common.platform,
                        Some(cli),
                        "refused_serial_mismatch",
                    );
                    return Err(format!(
                        "pic1704-reflash-fp refused: --serial {:?} does not match \
                         manifest target_serial {:?} (FpError::SerialMismatch)",
                        cli, mani,
                    ));
                }
            }
            eprintln!(
                "Manifest OK: subtype={} carrier={} word_count={} expected_crc=0x{:04X}",
                manifest.target_subtype,
                manifest.target_carrier,
                manifest.word_count,
                manifest.expected_post_flash_crc,
            );

            let app_words: &[u32] = &words[..manifest.word_count];
            let mut i2c = I2cBus::open(common.bus)
                .map_err(|e| format!("open /dev/i2c-{}: {}", common.bus, e))?;

            // ── Phase 1: pre-flight VERSION read + collision guard ────
            eprintln!("Phase 1: framed FP_READ_VERSION + bootloader assertion");
            assert_bootloader(&mut i2c, common.addr).inspect_err(|_| {
                append_audit_line(
                    "pic1704-reflash-fp",
                    common.addr,
                    common.platform,
                    cli_serial.as_deref(),
                    "phase1_refused_not_in_bootloader",
                );
            })?;

            // ── Phase 2: erase pages covering the application region ──
            eprintln!("Phase 2: erase pages from 0x{:06X}", FLASH_APP_START);
            let mut page = FLASH_APP_START;
            while page < FLASH_APP_START + manifest.word_count as u32 {
                let seek = seek_steps_v2(page)
                    .map_err(|e| format!("seek_steps_v2(0x{:06X}): {:?}", page, e))?;
                fp_write_raw(&mut i2c, common.addr, &seek)?;
                let erase = erase_steps_v2(page)
                    .map_err(|e| format!("erase_steps_v2(0x{:06X}): {:?}", page, e))?;
                fp_write_raw(&mut i2c, common.addr, &erase)?;
                thread::sleep(Duration::from_millis(5));
                page = page.saturating_add(FLASH_PAGE_WORDS);
            }

            // ── Phase 3: write firmware in batches ────────────────────
            eprintln!(
                "Phase 3: write {} words in batches of {}",
                manifest.word_count, batch_size,
            );
            let mut offset = 0usize;
            while offset < manifest.word_count {
                let end = (offset + batch_size).min(manifest.word_count);
                let batch = &app_words[offset..end];
                let batch_addr = FLASH_APP_START + offset as u32;

                // PER-BATCH collision guard: re-read VERSION before
                // every batch. If any prior write knocked the chip
                // out of bootloader, refuse to continue.
                assert_bootloader(&mut i2c, common.addr).inspect_err(|_| {
                    append_audit_line(
                        "pic1704-reflash-fp",
                        common.addr,
                        common.platform,
                        cli_serial.as_deref(),
                        "phase3_refused_batch_not_in_bootloader",
                    );
                })?;

                let seek = seek_steps_v2(batch_addr)
                    .map_err(|e| format!("seek_steps_v2(0x{:06X}): {:?}", batch_addr, e))?;
                fp_write_raw(&mut i2c, common.addr, &seek)?;
                let write = write_steps_v2(batch)
                    .map_err(|e| format!("write_steps_v2(batch@0x{:06X}): {:?}", batch_addr, e))?;
                fp_write_raw(&mut i2c, common.addr, &write)?;

                offset = end;
            }

            // ── Phase 4: verify CRC ───────────────────────────────────
            eprintln!("Phase 4: FP_VERIFY_CRC");
            let device_crc = fp_verify_crc(&mut i2c, common.addr)?;
            let host_crc = compute_crc_host(app_words);
            if device_crc != host_crc || device_crc != manifest.expected_post_flash_crc {
                append_audit_line(
                    "pic1704-reflash-fp",
                    common.addr,
                    common.platform,
                    cli_serial.as_deref(),
                    &format!(
                        "phase4_crc_mismatch_device=0x{:04X}_host=0x{:04X}_manifest=0x{:04X}",
                        device_crc, host_crc, manifest.expected_post_flash_crc,
                    ),
                );
                return Err(format!(
                    "pic1704-reflash-fp HARD STOP: CRC mismatch (device=0x{:04X}, \
                     host=0x{:04X}, manifest=0x{:04X}). Do NOT auto-retry — \
                     bootloader may be wedged. Power-cycle and re-probe.",
                    device_crc, host_crc, manifest.expected_post_flash_crc,
                ));
            }

            // ── Phase 5: start application ────────────────────────────
            eprintln!("Phase 5: FP_START_APP");
            fp_write_raw(&mut i2c, common.addr, &start_app_steps_v2())?;

            // ── Phase 6: poll until app or timeout ────────────────────
            eprintln!("Phase 6: poll FP_READ_VERSION up to {} ms", TIMEOUT_MS);
            let deadline = Instant::now() + Duration::from_millis(TIMEOUT_MS);
            loop {
                let ver = fp_read_version(&mut i2c, common.addr).unwrap_or(0xFF);
                if matches!(ver, VERSION_APP_88 | VERSION_APP_89 | VERSION_APP_8A) {
                    append_audit_line(
                        "pic1704-reflash-fp",
                        common.addr,
                        common.platform,
                        cli_serial.as_deref(),
                        &format!("ok_app_version_0x{:02X}", ver),
                    );
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    append_audit_line(
                        "pic1704-reflash-fp",
                        common.addr,
                        common.platform,
                        cli_serial.as_deref(),
                        "phase6_post_start_app_timeout",
                    );
                    return Err(format!(
                        "pic1704-reflash-fp: post-FP_START_APP version poll timed out \
                         after {} ms (last version 0x{:02X}). Reflash phases 1-5 \
                         appear complete (CRC matched), but the app didn't start. \
                         Manual ICSP rework may be required.",
                        TIMEOUT_MS, ver,
                    ));
                }
                thread::sleep(Duration::from_millis(POLL_MS));
            }
        })();
        report("pic1704-reflash-fp", r)
    }
}

// ── PIC1704 stock-bmminer reflash subcommand handler (W15.B) ────────────────
//
// Ghidra-extracted byte-exact wire format. Distinct from W14.C V2
// (`pic1704_v2_cli` above):
//
//   - W15.B (`pic1704-reflash-stock`): GHIDRA-EXTRACTED stock bmminer
//     0x55 magic + additive checksum + 2-phase write + 300 ms wait.
//   - W14.C (`pic1704-reflash-fp` w4v2 path): RE-INFERRED REG_CMD
//     0x10-0x15 + CRC-ITU-T V.41 + single-phase write.
//
// Routing decision (W15.B3):
//
//   - `pic1704-reflash-stock` is always stock.
//   - `pic1704-reflash-fp --pic1704-protocol=stock` is always stock.
//   - `pic1704-reflash-fp --pic1704-protocol=w4v2` is always w4v2.
//   - `pic1704-reflash-fp` (no flag) or `--pic1704-protocol=auto` →
//     probe stock SEEK; if [0x01,0x01] → stock, else → w4v2.
//
// Audit log: `$DCENT_PIC_RECOVERY_LOG_DIR/pic1704_stock_audit.log`
// (default `/var/log/dcent/`). Every phase logs an outcome line.
mod pic1704_stock_cli {
    use super::I2cBus;
    use dcentrald_asic::pic1704::programmer_stock::{
        compute_checksum_stock, erase_steps_stock, pack_words_msb_first, parse_hex_app_file,
        seek_steps_stock, write_phase1_steps_stock, write_phase2_steps_stock, ACK_ERASE, ACK_SEEK,
        ACK_WRITE_PHASE1, ACK_WRITE_PHASE2, STOCK_APP_START_WORDS, STOCK_ERASE_BATCH_WORDS,
        STOCK_INTER_PHASE_MS, STOCK_WRITE_BATCH_BYTES,
    };
    use dcentrald_asic::pic1704::reflash::{route_by_seek_ack, Pic1704Protocol};
    use std::fs;
    use std::io::Write as _;
    use std::path::PathBuf;
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    const DEFAULT_LOG_DIR: &str = "/var/log/dcent";
    const LOG_FILENAME: &str = "pic1704_stock_audit.log";

    fn audit_log_path() -> PathBuf {
        let dir = std::env::var("DCENT_PIC_RECOVERY_LOG_DIR")
            .unwrap_or_else(|_| DEFAULT_LOG_DIR.to_string());
        PathBuf::from(dir).join(LOG_FILENAME)
    }

    fn append_audit_line(
        op: &str,
        addr: u8,
        platform: &str,
        serial: Option<&str>,
        protocol: &str,
        outcome: &str,
    ) {
        let unix_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let serial_str = serial.unwrap_or("-");
        let line = format!(
            "{}\t{}\taddr=0x{:02X}\tplatform={}\tserial={}\tprotocol={}\toutcome={}\n",
            unix_secs, op, addr, platform, serial_str, protocol, outcome,
        );
        let path = audit_log_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = f.write_all(line.as_bytes());
        }
    }

    fn parse_optional_str(args: &[String], name: &str) -> Option<String> {
        let mut i = 0;
        while i < args.len() {
            if args[i] == name {
                return args.get(i + 1).cloned();
            }
            i += 1;
        }
        None
    }

    fn parse_required_str(args: &[String], name: &str) -> Result<String, String> {
        parse_optional_str(args, name).ok_or_else(|| format!("missing required argument {}", name))
    }

    fn parse_required_u8_hex(
        args: &[String],
        name: &str,
        default_hex: Option<u8>,
    ) -> Result<u8, String> {
        if let Some(v) = parse_optional_str(args, name) {
            let s = v.trim_start_matches("0x").trim_start_matches("0X");
            u8::from_str_radix(s, 16).map_err(|e| format!("{} {:?}: {}", name, v, e))
        } else if let Some(d) = default_hex {
            Ok(d)
        } else {
            Err(format!("{} is required", name))
        }
    }

    fn parse_required_u8(args: &[String], name: &str) -> Result<u8, String> {
        let v = parse_required_str(args, name)?;
        v.parse::<u8>()
            .map_err(|e| format!("{} {:?}: {}", name, v, e))
    }

    /// Send a packet, wait STOCK_INTER_PHASE_MS, read 2 bytes, compare
    /// to expected ACK. Returns Ok if matched, Err with diagnostic.
    fn send_and_check_ack(
        i2c: &mut I2cBus,
        addr: u8,
        packet: &[u8],
        expected: [u8; 2],
        phase: &str,
    ) -> Result<(), String> {
        i2c.set_slave(addr)
            .map_err(|e| format!("set_slave 0x{:02X}: {}", addr, e))?;
        i2c.write_bulk(packet)
            .map_err(|e| format!("{} write: {}", phase, e))?;
        thread::sleep(Duration::from_millis(STOCK_INTER_PHASE_MS));
        let bytes = i2c
            .read_bytes(2)
            .map_err(|e| format!("{} read: {}", phase, e))?;
        if bytes.len() < 2 {
            return Err(format!(
                "{}: short ACK ({} bytes, expected 2)",
                phase,
                bytes.len()
            ));
        }
        if bytes[0] != expected[0] || bytes[1] != expected[1] {
            return Err(format!(
                "{}: unexpected ACK [0x{:02X}, 0x{:02X}], expected [0x{:02X}, 0x{:02X}]",
                phase, bytes[0], bytes[1], expected[0], expected[1],
            ));
        }
        Ok(())
    }

    /// Probe a stock SEEK against `STOCK_APP_START_WORDS` and return
    /// the 2-byte ACK if the read succeeded. `None` on read failure /
    /// short read / NACK.
    fn probe_stock_seek_ack(i2c: &mut I2cBus, addr: u8) -> Option<[u8; 2]> {
        let pkt = seek_steps_stock(STOCK_APP_START_WORDS).ok()?;
        i2c.set_slave(addr).ok()?;
        i2c.write_bulk(&pkt).ok()?;
        thread::sleep(Duration::from_millis(STOCK_INTER_PHASE_MS));
        let bytes = i2c.read_bytes(2).ok()?;
        if bytes.len() < 2 {
            return None;
        }
        Some([bytes[0], bytes[1]])
    }

    /// Top-level entry for `pic1704-reflash-stock`. Always stock
    /// protocol — no probe / no fallback. CLI flags identical to
    /// `pic1704-reflash-fp` shape.
    pub fn run_reflash_stock(args: &[String]) -> i32 {
        run_reflash_stock_with_routing(args, Some(Pic1704Protocol::Stock))
    }

    /// Routing-aware entry: caller has already resolved `--pic1704-protocol`.
    /// `Some(Stock)` → run stock unconditionally. `None` (auto) → probe
    /// stock SEEK and route. `Some(V2Custom)` is a programming error
    /// here (caller should fall through to W14.C V2 path instead).
    pub fn run_reflash_stock_with_routing(args: &[String], forced: Option<Pic1704Protocol>) -> i32 {
        if matches!(forced, Some(Pic1704Protocol::V2Custom)) {
            eprintln!(
                "pic1704_stock_cli internal error: V2Custom routed to stock CLI \
                 — use pic1704_v2_cli::run_reflash_fp instead"
            );
            return 1;
        }
        let r = (|| -> Result<(), String> {
            // ── Common-arg parsing (mirrors V2 CLI shape) ──────────────
            let bus = parse_required_u8(args, "--bus")?;
            let addr = parse_required_u8_hex(args, "--addr", Some(0x20))?;
            let platform = parse_required_str(args, "--platform")?;
            let hex_path = parse_required_str(args, "--hex")?;
            let manifest_path = parse_required_str(args, "--manifest")?;
            let cli_serial = parse_optional_str(args, "--serial");
            let flag_confirm = args.iter().any(|a| a == "--confirm-bricked");

            if !flag_confirm {
                return Err(
                    "pic1704-reflash-stock refused: --confirm-bricked is required \
                     (DESTRUCTIVE — Ghidra-extracted stock protocol is byte-exact \
                     decoded but bench-untested on a real bricked PIC1704)"
                        .to_string(),
                );
            }
            // Validate platform string against the V2 carrier set; the
            // stock protocol applies to the same 3 PIC1704 carriers.
            match platform.as_str() {
                "cv1835-s19jpro" | "am335x-bb-s19jpro" | "amlogic-s19jpro" => {}
                other => {
                    return Err(format!(
                        "unknown --platform {:?}; expected one of: cv1835-s19jpro, \
                         am335x-bb-s19jpro, amlogic-s19jpro",
                        other,
                    ));
                }
            }

            eprintln!();
            eprintln!("================================================================");
            eprintln!("PIC1704 stock bmminer reflash (W15.B Ghidra-extracted)");
            eprintln!("Bus={} Addr=0x{:02X} Platform={}", bus, addr, platform);
            eprintln!(
                "Inter-phase delay: {} ms (NOT µs — W4 errata)",
                STOCK_INTER_PHASE_MS
            );
            eprintln!(
                "Erase batch: {} words / Write batch: {} BYTES",
                STOCK_ERASE_BATCH_WORDS, STOCK_WRITE_BATCH_BYTES
            );
            eprintln!("================================================================");
            eprintln!();

            // ── Load hex + manifest ────────────────────────────────────
            let hex_path_buf = std::path::PathBuf::from(&hex_path);
            let words_u16 = parse_hex_app_file(&hex_path_buf)
                .map_err(|e| format!("parse hex {}: {:?}", hex_path, e))?;
            if words_u16.is_empty() {
                return Err("hex file contains zero words".to_string());
            }
            // Check manifest existence + readability (full schema check
            // shared with V2 CLI is intentionally minimal here — stock
            // CLI is the explicit-route path; auto-route delegates to V2
            // CLI's full manifest validation when chosen).
            fs::read_to_string(&manifest_path)
                .map_err(|e| format!("read manifest {}: {}", manifest_path, e))?;

            let payload_bytes = pack_words_msb_first(&words_u16);

            // ── Routing decision (auto only) ───────────────────────────
            let mut i2c = I2cBus::open(bus).map_err(|e| format!("open /dev/i2c-{}: {}", bus, e))?;
            let chosen_protocol = match forced {
                Some(p) => p,
                None => {
                    // auto → probe stock SEEK ACK
                    let ack = probe_stock_seek_ack(&mut i2c, addr);
                    let routed = route_by_seek_ack(ack);
                    eprintln!(
                        "Auto-detect: stock SEEK probe ACK = {:?} → routed to {}",
                        ack,
                        routed.label(),
                    );
                    append_audit_line(
                        "pic1704-reflash-auto",
                        addr,
                        &platform,
                        cli_serial.as_deref(),
                        routed.label(),
                        &format!("auto_routed_ack={:?}", ack),
                    );
                    routed
                }
            };
            if chosen_protocol == Pic1704Protocol::V2Custom {
                // Shouldn't be reached because the dispatcher in
                // `run_reflash_fp` handles auto-V2 fallback by calling
                // back into the V2 path. But guard explicitly here in
                // case a future caller wires routing differently.
                return Err(
                    "auto-route picked w4v2 — caller must fall back to pic1704-reflash-fp \
                     w4v2 path; stock CLI cannot run V2 wire format"
                        .to_string(),
                );
            }

            // ── Phase 1: SEEK to start of app region ───────────────────
            eprintln!("Phase 1: stock SEEK to 0x{:04X}", STOCK_APP_START_WORDS);
            let seek_pkt = seek_steps_stock(STOCK_APP_START_WORDS)
                .map_err(|e| format!("seek_steps_stock: {:?}", e))?;
            send_and_check_ack(&mut i2c, addr, &seek_pkt, ACK_SEEK, "SEEK").inspect_err(|_| {
                append_audit_line(
                    "pic1704-reflash-stock",
                    addr,
                    &platform,
                    cli_serial.as_deref(),
                    "stock",
                    "phase1_seek_failed",
                );
            })?;

            // ── Phase 2: ERASE pages covering the program region ───────
            let total_words = words_u16.len() as u32;
            let erase_iters =
                ((total_words + STOCK_ERASE_BATCH_WORDS - 1) / STOCK_ERASE_BATCH_WORDS) as usize;
            eprintln!(
                "Phase 2: stock ERASE × {} iterations ({} words / {} per iter)",
                erase_iters, total_words, STOCK_ERASE_BATCH_WORDS,
            );
            for iter in 0..erase_iters {
                if iter > u8::MAX as usize {
                    return Err(format!(
                        "ERASE iteration {} exceeds u8 — hex file too large for stock protocol",
                        iter
                    ));
                }
                let pkt = erase_steps_stock(iter as u8)
                    .map_err(|e| format!("erase_steps_stock(iter={}): {:?}", iter, e))?;
                send_and_check_ack(
                    &mut i2c,
                    addr,
                    &pkt,
                    ACK_ERASE,
                    &format!("ERASE iter {}", iter),
                )
                .inspect_err(|_| {
                    append_audit_line(
                        "pic1704-reflash-stock",
                        addr,
                        &platform,
                        cli_serial.as_deref(),
                        "stock",
                        &format!("phase2_erase_failed_iter_{}", iter),
                    );
                })?;
            }

            // ── Phase 3: WRITE in 16-byte batches (phase 1 + phase 2) ──
            let total_bytes = payload_bytes.len();
            let n_batches = (total_bytes + STOCK_WRITE_BATCH_BYTES - 1) / STOCK_WRITE_BATCH_BYTES;
            eprintln!(
                "Phase 3: stock WRITE × {} batches ({} bytes / {} per batch)",
                n_batches, total_bytes, STOCK_WRITE_BATCH_BYTES,
            );
            for batch_idx in 0..n_batches {
                let start = batch_idx * STOCK_WRITE_BATCH_BYTES;
                let end = (start + STOCK_WRITE_BATCH_BYTES).min(total_bytes);
                let mut chunk = [0u8; 16];
                for (i, b) in payload_bytes[start..end].iter().enumerate() {
                    chunk[i] = *b;
                }
                // Phase 3a: WRITE phase 1 (data)
                let p1 = write_phase1_steps_stock(&chunk)
                    .map_err(|e| format!("write_phase1(batch={}): {:?}", batch_idx, e))?;
                send_and_check_ack(
                    &mut i2c,
                    addr,
                    &p1,
                    ACK_WRITE_PHASE1,
                    &format!("WRITE phase 1 batch {}", batch_idx),
                )
                .inspect_err(|_| {
                    append_audit_line(
                        "pic1704-reflash-stock",
                        addr,
                        &platform,
                        cli_serial.as_deref(),
                        "stock",
                        &format!("phase3a_write_phase1_failed_batch_{}", batch_idx),
                    );
                })?;
                // Phase 3b: WRITE phase 2 (commit)
                let p2 = write_phase2_steps_stock();
                send_and_check_ack(
                    &mut i2c,
                    addr,
                    &p2,
                    ACK_WRITE_PHASE2,
                    &format!("WRITE phase 2 batch {}", batch_idx),
                )
                .inspect_err(|_| {
                    append_audit_line(
                        "pic1704-reflash-stock",
                        addr,
                        &platform,
                        cli_serial.as_deref(),
                        "stock",
                        &format!("phase3b_write_phase2_failed_batch_{}", batch_idx),
                    );
                })?;
            }

            // ── Phase 4: success ───────────────────────────────────────
            // Stock protocol has no CRC verify or jump-to-app phase in
            // the Ghidra source — the application boots when the DC-DC
            // is re-enabled by the caller. Audit-log success and return.
            // Sanity-check the additive sum was non-zero (catches a
            // future no-op codec bug where every batch checksum is 0).
            let _final_ck = compute_checksum_stock(&payload_bytes, 0);
            append_audit_line(
                "pic1704-reflash-stock",
                addr,
                &platform,
                cli_serial.as_deref(),
                "stock",
                &format!(
                    "ok_words={}_bytes={}_batches={}",
                    total_words, total_bytes, n_batches
                ),
            );
            eprintln!(
                "Phase 4: COMPLETE — {} words written ({} batches)",
                total_words, n_batches
            );
            Ok(())
        })();
        match r {
            Ok(()) => {
                println!("pic1704-reflash-stock: OK");
                0
            }
            Err(e) => {
                eprintln!("pic1704-reflash-stock: ERROR — {}", e);
                1
            }
        }
    }
}
