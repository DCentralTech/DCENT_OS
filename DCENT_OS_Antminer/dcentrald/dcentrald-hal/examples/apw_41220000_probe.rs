//! One-off `a lab unit` APW probe via the PL GPIO bank at `0x41220000`.
//!
//! This is a lab helper, not production firmware code. It exists to test the
//! hypothesis that the S19j Pro `a lab unit` APW121215a path is bit-banged through
//! `gpio895/896` (`PWR_I2C2_SDA/SCL`) with `gpio907` asserted.

use std::error::Error;
use std::fs;
use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

use dcentrald_hal::psu_gpio_i2c::GpioBitBangI2c;

const GPIO_907: u32 = 907;
const SDA_GPIO: u32 = 895;
const SCL_GPIO: u32 = 896;
const APW_ADDR: u8 = 0x10;

type DynResult<T> = Result<T, Box<dyn Error>>;

fn gpio_dir(gpio: u32) -> String {
    format!("/sys/class/gpio/gpio{}", gpio)
}

fn ensure_exported(gpio: u32) -> DynResult<()> {
    let dir = gpio_dir(gpio);
    if !Path::new(&dir).exists() {
        fs::write("/sys/class/gpio/export", format!("{}", gpio))?;
        sleep(Duration::from_millis(50));
    }
    Ok(())
}

fn read_trimmed(path: &str) -> DynResult<String> {
    Ok(fs::read_to_string(path)?.trim().to_string())
}

fn read_gpio_value(gpio: u32) -> DynResult<String> {
    read_trimmed(&format!("{}/value", gpio_dir(gpio)))
}

fn read_gpio_direction(gpio: u32) -> DynResult<String> {
    read_trimmed(&format!("{}/direction", gpio_dir(gpio)))
}

fn write_gpio(gpio: u32, dir: &str, value: &str) -> DynResult<()> {
    ensure_exported(gpio)?;
    fs::write(format!("{}/direction", gpio_dir(gpio)), dir)?;
    fs::write(format!("{}/value", gpio_dir(gpio)), value)?;
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<Vec<_>>()
        .join(" ")
}

fn main() -> DynResult<()> {
    let old_907_dir = read_gpio_direction(GPIO_907).ok();
    let old_907_val = read_gpio_value(GPIO_907).ok();

    println!("== APW 0x41220000 probe ==");
    println!(
        "SDA gpio={}, SCL gpio={}, APW addr=0x{:02x}",
        SDA_GPIO, SCL_GPIO, APW_ADDR
    );

    println!(
        "gpio907 before: dir={:?} value={:?}",
        old_907_dir, old_907_val
    );
    write_gpio(GPIO_907, "out", "1")?;
    println!(
        "gpio907 asserted high: dir={} value={}",
        read_gpio_direction(GPIO_907)?,
        read_gpio_value(GPIO_907)?
    );

    let i2c = GpioBitBangI2c::new_am2()?;
    i2c.bus_recovery()?;
    println!("bus recovery complete");

    println!("-- address-only probe (empty write) --");
    match i2c.write_to(APW_ADDR, &[]) {
        Ok(()) => println!("address ACK via bit-bang: YES"),
        Err(e) => println!("address ACK via bit-bang: NO ({})", e),
    }

    let fw_frame = [0x55, 0xAA, 0x03, 0x01, 0x04];
    println!("-- GET_FW_VERSION write {} --", hex(&fw_frame));
    match i2c.write_to(APW_ADDR, &fw_frame) {
        Ok(()) => println!("GET_FW_VERSION write: ACK"),
        Err(e) => println!("GET_FW_VERSION write: FAIL ({})", e),
    }

    sleep(Duration::from_millis(50));
    let mut resp = [0u8; 8];
    println!("-- read 8 bytes after GET_FW_VERSION --");
    match i2c.read_from(APW_ADDR, &mut resp) {
        Ok(n) => println!("read ok: {} bytes [{}]", n, hex(&resp[..n.min(resp.len())])),
        Err(e) => println!("read fail: {}", e),
    }

    let wd_disable = [0x55, 0xAA, 0x04, 0x81, 0x00, 0x85];
    println!("-- watchdog disable write {} --", hex(&wd_disable));
    match i2c.write_to(APW_ADDR, &wd_disable) {
        Ok(()) => println!("watchdog disable write: ACK"),
        Err(e) => println!("watchdog disable write: FAIL ({})", e),
    }

    if let (Some(dir), Some(val)) = (old_907_dir.as_deref(), old_907_val.as_deref()) {
        fs::write(format!("{}/direction", gpio_dir(GPIO_907)), dir)?;
        if dir == "out" {
            fs::write(format!("{}/value", gpio_dir(GPIO_907)), val)?;
        }
        println!("gpio907 restored to dir={} value={}", dir, val);
    }

    Ok(())
}
