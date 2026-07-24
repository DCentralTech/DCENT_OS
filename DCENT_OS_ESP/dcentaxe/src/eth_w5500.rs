// DCENT_axe W5500 SPI-Ethernet bring-up ("DCENT LAN Mod") — PLAN-E Phase 1
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0
//
// ESP-IDF native `esp_eth` W5500 MAC/PHY + lwIP netif via the esp-idf-svc safe
// wrappers (`EthDriver::new_spi_with_event_source` → `EspEth`) — deliberately
// NOT a bespoke SPI register poller. This mirrors the substrate TNA's
// ESP-Miner-LAN itself uses (`esp_eth_mac_new_w5500`/`esp_eth_phy_new_w5500`,
// polling mode, DHCP-on) — approach reuse, independently written Rust.
//
// Hardware contract (see `dcentaxe_hal::eth` for the locked pin map + tests):
// - SPI2/FSPI host, mode 0, clone-safe 2 MHz (`W5500_SPI_HZ` — bench-gated TODO
//   to raise), on the 4 BAP/J4 lines GPIO 41/40/39/42 (SCLK/MOSI/MISO/CS).
// - W5500 RST = on-board RC power-on-reset, INT = unwired → the ESP-IDF MAC
//   runs in POLLING mode (`SpiEventSource::polling`, TNA-parity 1 ms) and the
//   firmware consumes ZERO host GPIO beyond the 4 SPI pins.
// - MAC = efuse base MAC with the locally-administered bit set and the last
//   byte XOR 0x01 (`derive_eth_mac`, host-tested TNA parity).
//
// Failover mechanism: the Ethernet netif is created with `route_priority`
// ABOVE the Wi-Fi STA default (100) whenever LAN is preferred, so lwIP itself
// steers outbound traffic to Ethernet while its link+IP are up and falls back
// to Wi-Fi when the cable drops. The pure FSM in `net.rs` observes and reports;
// inbound services bind all netifs.
//
// ⚠️ Integration seam — NEEDS-VERIFY on silicon (same posture as
// `lora_pins::open_lora_bus`): host tests cover the pure layers only; live
// link + accepted-share-over-Ethernet proof is bench-gated (PLAN-E Phase 3).
// Callers must treat every error as fail-soft: log, leave Ethernet dark,
// mining unaffected.

// The esp-idf-svc W5500 types exist only when the ESP-IDF sdkconfig compiles
// the W5500 driver in. The `eth-w5500` Cargo feature therefore REQUIRES the
// sdkconfig overlay — fail the build loudly (instead of 40 confusing
// missing-type errors) when it is absent.
#[cfg(not(esp_idf_eth_spi_ethernet_w5500))]
compile_error!(
    "feature `eth-w5500` needs the ESP-IDF W5500 driver compiled in: build with \
     ESP_IDF_SDKCONFIG_DEFAULTS=\"sdkconfig.defaults;sdkconfig.defaults.eth-w5500\" \
     (repo root, see that overlay file), then wipe the target dir (e.g. C:/bt) so \
     the generated sdkconfig regenerates."
);

#[cfg(esp_idf_eth_spi_ethernet_w5500)]
mod imp {
    use std::time::Duration;

    use esp_idf_svc::eth::{EspEth, EthDriver, SpiEth, SpiEthChipset, SpiEventSource};
    use esp_idf_svc::eventloop::EspSystemEventLoop;
    use esp_idf_svc::hal::gpio::{AnyInputPin, AnyOutputPin};
    use esp_idf_svc::hal::spi::{config::DriverConfig, SpiAnyPins, SpiDriver};
    use esp_idf_svc::hal::units::Hertz;
    use esp_idf_svc::netif::{EspNetif, NetifConfiguration};
    use esp_idf_svc::sys;
    use log::{info, warn};

    use dcentaxe_hal::eth::{derive_eth_mac, W5500_POLL_PERIOD_MS, W5500_SPI_HZ};

    use crate::config::NetworkMode;
    use crate::net::LinkSnapshot;

    /// Ethernet netif route priority when LAN is preferred (EthPreferred /
    /// EthOnly): above the Wi-Fi STA default of 100 so lwIP selects Ethernet
    /// as the default route whenever its link + address are up, and falls
    /// back to the STA netif automatically when they are not.
    const ETH_ROUTE_PRIORITY_PREFERRED: u32 = 120;

    /// TNA fallback MAC when the efuse read fails (`ethernet_w5500.c`
    /// parity): locally administered, unicast, non-zero.
    const FALLBACK_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];

    /// A live W5500 Ethernet link: the `esp_eth` driver + its lwIP netif.
    /// Keep this alive for as long as the link should exist (dropping it
    /// stops the driver and detaches the netif).
    pub struct EthLink {
        eth: EspEth<'static, SpiEth<SpiDriver<'static>>>,
    }

    impl EthLink {
        /// Bring up the W5500 on SPI2 with DHCP. Blocking work is bounded to
        /// driver install/start — DHCP proceeds asynchronously; poll
        /// [`EthLink::snapshot`] from the main loop.
        ///
        /// Only ever called after `DcentAxeConfig::eth_lan_activation()`
        /// returned `Ok(true)` — i.e. the operator opted in AND the
        /// `AccessoryMode::W5500Lan` guard passed (never on a BAP-Touch
        /// board). `mode` is EthPreferred or EthOnly here; both prefer the
        /// wired route (WifiOnly never reaches bring-up).
        pub fn bring_up<SPI: SpiAnyPins + 'static>(
            spi2: SPI,
            sclk: AnyOutputPin<'static>,
            mosi: AnyOutputPin<'static>,
            miso: AnyInputPin<'static>,
            cs: AnyOutputPin<'static>,
            sysloop: EspSystemEventLoop,
            mode: NetworkMode,
        ) -> Result<Self, String> {
            // SPI2 bus with the three shared lines; esp_eth adds the W5500 as
            // an NSS-framed device (CS handed to the driver below). `sdo` =
            // MOSI, `sdi` = MISO — same convention as `open_lora_bus`.
            let spi = SpiDriver::new(spi2, sclk, mosi, Some(miso), &DriverConfig::new())
                .map_err(|e| format!("SPI2 bus init failed: {e:?}"))?;

            // Unique per-unit MAC — TNA-parity derivation from the efuse base
            // MAC, with the TNA fallback if the efuse read fails.
            let mut base = [0u8; 6];
            let mac = if unsafe { sys::esp_efuse_mac_get_default(base.as_mut_ptr()) } == sys::ESP_OK
            {
                derive_eth_mac(base)
            } else {
                warn!("ETH: efuse base-MAC read failed — using fallback MAC");
                FALLBACK_MAC
            };

            // Polling event source: the add-on does not wire W5500 INT (zero
            // extra host GPIO), so the MAC polls at the TNA-parity period.
            let poll =
                SpiEventSource::polling(Duration::from_millis(u64::from(W5500_POLL_PERIOD_MS)))
                    .map_err(|e| format!("invalid W5500 poll period: {e:?}"))?;

            let driver = EthDriver::new_spi_with_event_source(
                spi,
                poll,
                Some(cs),
                // RST is an on-board RC power-on-reset — no host GPIO.
                Option::<AnyOutputPin>::None,
                SpiEthChipset::W5500,
                Hertz(W5500_SPI_HZ),
                Some(&mac),
                None, // PHY address: W5500 default
                sysloop,
            )
            .map_err(|e| format!("esp_eth W5500 driver install failed: {e:?}"))?;

            // DHCP-client netif (eth_default_client) with the failover route
            // priority. lwIP's default-netif selection by route_prio IS the
            // outbound failover mechanism (see module doc).
            let mut conf = NetifConfiguration::eth_default_client();
            conf.route_priority = match mode {
                // WifiOnly never reaches bring-up (activation gate); keep the
                // arm anyway so a future caller cannot silently prefer eth
                // under a wifi-only config.
                NetworkMode::WifiOnly => NetifConfiguration::eth_default_client().route_priority,
                NetworkMode::EthOnly | NetworkMode::EthPreferred => ETH_ROUTE_PRIORITY_PREFERRED,
            };
            let netif = EspNetif::new_with_conf(&conf)
                .map_err(|e| format!("eth netif create failed: {e:?}"))?;

            let mut eth = EspEth::wrap_all(driver, netif)
                .map_err(|e| format!("eth netif attach failed: {e:?}"))?;
            eth.start()
                .map_err(|e| format!("esp_eth start failed: {e:?}"))?;

            info!(
                "ETH: W5500 up on SPI2 (SCLK41/MOSI40/MISO39/CS42) @ {} MHz, poll {} ms, \
                 mac {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, route_prio {}, DHCP pending",
                W5500_SPI_HZ / 1_000_000,
                W5500_POLL_PERIOD_MS,
                mac[0],
                mac[1],
                mac[2],
                mac[3],
                mac[4],
                mac[5],
                conf.route_priority,
            );
            Ok(Self { eth })
        }

        /// PHY carrier status (esp_eth link state).
        pub fn is_link_up(&self) -> bool {
            self.eth.is_connected().unwrap_or(false)
        }

        /// The netif's current IPv4 address, if DHCP has assigned one.
        pub fn ip(&self) -> Option<std::net::Ipv4Addr> {
            self.eth
                .netif()
                .get_ip_info()
                .ok()
                .filter(|info| !info.ip.is_unspecified())
                .map(|info| info.ip)
        }

        /// Sample link state for the pure failover FSM (`net::FailoverState`).
        pub fn snapshot(&self, wifi_up: bool) -> LinkSnapshot {
            LinkSnapshot {
                wifi_up,
                eth_link_up: self.is_link_up(),
                eth_has_ip: self.ip().is_some(),
            }
        }
    }
}

#[cfg(esp_idf_eth_spi_ethernet_w5500)]
pub use imp::EthLink;
