//! Canonical AM2 Zynq PL-UART to hashboard-slot/controller mapping.
//!
//! This leaf owns the four-slot topology shared by planning and HAL discovery.
//! It does not grant hardware authority; it only prevents duplicate tables from
//! drifting as lifecycle evidence is layered above it.

pub const AM2_SLOT_COUNT: usize = 4;

pub const AM2_SLOT_UARTS: [&str; AM2_SLOT_COUNT] =
    ["/dev/ttyS1", "/dev/ttyS2", "/dev/ttyS3", "/dev/ttyS4"];

pub const AM2_SLOT_DSPIC_ADDRS: [u8; AM2_SLOT_COUNT] = [0x20, 0x21, 0x22, 0x23];

pub fn slot_for_uart(serial_device: &str) -> Option<u8> {
    AM2_SLOT_UARTS
        .iter()
        .position(|candidate| *candidate == serial_device)
        .and_then(|slot| u8::try_from(slot).ok())
}

pub fn dspic_address_for_slot(slot: u8) -> Option<u8> {
    AM2_SLOT_DSPIC_ADDRS.get(usize::from(slot)).copied()
}

pub fn uart_for_slot(slot: u8) -> Option<&'static str> {
    AM2_SLOT_UARTS.get(usize::from(slot)).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_four_slots_round_trip() {
        for slot in 0..AM2_SLOT_COUNT as u8 {
            let uart = uart_for_slot(slot).unwrap();
            assert_eq!(slot_for_uart(uart), Some(slot));
            assert_eq!(dspic_address_for_slot(slot), Some(0x20 + slot));
        }
        assert_eq!(slot_for_uart("/dev/ttyS5"), None);
        assert_eq!(dspic_address_for_slot(4), None);
    }
}
