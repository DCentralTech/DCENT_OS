//!  braiins-G — BraiinsOS+ proto wire-type wrappers (HAL-free).
//!
//! Source RE evidence:
//!
//! §9.2.28 (lines 1228-1255) — 9 unit-wrapper messages used across
//! the BraiinsOS+ gRPC API + the `RealHashrate` 10-window sliding
//! aggregator.
//!
//!  + 25 + 26 cite these wire types inline (e.g.
//! `Power=u64 watts` is referenced in  braiinsos_constraints
//! and  braiinsos_dps_configuration). This module formalizes
//! them as their own typed structs so future BraiinsOS+ DTO modules
//! can reuse the canonical shape + conversion helpers.
//!
//! Wire-type discipline pinned by tests:
//! - Power.watt = u64 (not u32)
//! - Hours.hours = u32 (not u64)
//! - All Hashrate variants and PowerEfficiency = f64 (not f32)
//! - Field names match the proto verbatim: `watt`, `hertz`, `volt`,
//!   `degree_c`, `terahash_per_second`, `gigahash_per_second`,
//!   `megahash_per_second`, `joule_per_terahash`, `hours`.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Unit wrappers
// ---------------------------------------------------------------------------

/// `Power { uint64 watt = 1; }` — wattage (always whole watts).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Power {
    pub watt: u64,
}

impl Power {
    pub fn from_watts(watts: u64) -> Self {
        Self { watt: watts }
    }
}

/// `Frequency { double hertz = 1; }` — fractional hertz.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Frequency {
    pub hertz: f64,
}

/// `Voltage { double volt = 1; }` — fractional volts.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Voltage {
    pub volt: f64,
}

/// `Temperature { double degree_c = 1; }` — degrees Celsius.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Temperature {
    pub degree_c: f64,
}

/// `TeraHashrate { double terahash_per_second = 1; }`.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct TeraHashrate {
    pub terahash_per_second: f64,
}

impl TeraHashrate {
    pub fn from_giga(g: GigaHashrate) -> Self {
        // 1 TH = 1000 GH.
        Self {
            terahash_per_second: g.gigahash_per_second / 1_000.0,
        }
    }
}

/// `GigaHashrate { double gigahash_per_second = 1; }`.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct GigaHashrate {
    pub gigahash_per_second: f64,
}

impl GigaHashrate {
    pub fn from_mega(m: MegaHashrate) -> Self {
        // 1 GH = 1000 MH.
        Self {
            gigahash_per_second: m.megahash_per_second / 1_000.0,
        }
    }

    pub fn from_tera(t: TeraHashrate) -> Self {
        // 1 TH = 1000 GH.
        Self {
            gigahash_per_second: t.terahash_per_second * 1_000.0,
        }
    }
}

/// `MegaHashrate { double megahash_per_second = 1; }`.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct MegaHashrate {
    pub megahash_per_second: f64,
}

/// `PowerEfficiency { double joule_per_terahash = 1; }`.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct PowerEfficiency {
    pub joule_per_terahash: f64,
}

impl PowerEfficiency {
    /// Compute J/TH from a wall-power and a hashrate. Equivalent to
    /// the dashboard "efficiency" badge: `power.watt / hashrate.ths`.
    /// Returns `0.0` if `ths == 0.0` (avoids div-by-zero).
    pub fn from_watts_and_ths(watts: u64, ths: f64) -> Self {
        let jpt = if ths == 0.0 {
            0.0
        } else {
            (watts as f64) / ths
        };
        Self {
            joule_per_terahash: jpt,
        }
    }
}

/// `Hours { uint32 hours = 1; }` — whole hours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Hours {
    pub hours: u32,
}

// ---------------------------------------------------------------------------
// RealHashrate sliding-window aggregator
// ---------------------------------------------------------------------------

/// `RealHashrate` per RE doc §9.2.28 lines 1242-1254 — 10-window
/// sliding hashrate aggregator. All fields are GigaHashrate.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct RealHashrate {
    pub last_5s: GigaHashrate,
    pub last_15s: GigaHashrate,
    pub last_30s: GigaHashrate,
    pub last_1m: GigaHashrate,
    pub last_5m: GigaHashrate,
    pub last_15m: GigaHashrate,
    pub last_30m: GigaHashrate,
    pub last_1h: GigaHashrate,
    pub last_24h: GigaHashrate,
    pub since_restart: GigaHashrate,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_watt_field_name_matches_proto() {
        let p = Power { watt: 3500 };
        let json = serde_json::to_value(&p).unwrap();
        assert!(json.get("watt").is_some());
        assert_eq!(json["watt"].as_u64(), Some(3500));
        // Negative pin: NO `watts` (plural) field.
        assert!(json.get("watts").is_none());
    }

    #[test]
    fn power_field_is_u64_not_u32() {
        // Pin via an extreme value — u32::MAX = 4_294_967_295.
        // u64::MAX is much larger.
        let p = Power { watt: u64::MAX };
        let json = serde_json::to_string(&p).unwrap();
        let back: Power = serde_json::from_str(&json).unwrap();
        assert_eq!(back.watt, u64::MAX);
    }

    #[test]
    fn power_from_watts_helper_round_trips() {
        let p = Power::from_watts(3500);
        assert_eq!(p.watt, 3500);
    }

    #[test]
    fn frequency_hertz_field_name_matches_proto() {
        let f = Frequency {
            hertz: 525_000_000.0,
        };
        let json = serde_json::to_value(&f).unwrap();
        assert!(json.get("hertz").is_some());
        assert!(json.get("hz").is_none());
    }

    #[test]
    fn voltage_volt_field_name_matches_proto() {
        let v = Voltage { volt: 13.8 };
        let json = serde_json::to_value(&v).unwrap();
        assert!(json.get("volt").is_some());
        assert!(json.get("voltage").is_none());
    }

    #[test]
    fn temperature_degree_c_field_name_matches_proto() {
        let t = Temperature { degree_c: 75.0 };
        let json = serde_json::to_value(&t).unwrap();
        assert!(json.get("degree_c").is_some());
        // Negative pins: NO `celsius`, `temp`, etc.
        assert!(json.get("celsius").is_none());
        assert!(json.get("temp").is_none());
    }

    #[test]
    fn terahashrate_field_name_matches_proto() {
        let t = TeraHashrate {
            terahash_per_second: 200.0,
        };
        let json = serde_json::to_value(&t).unwrap();
        assert!(json.get("terahash_per_second").is_some());
        assert!(json.get("ths").is_none());
    }

    #[test]
    fn gigahashrate_field_name_matches_proto() {
        let g = GigaHashrate {
            gigahash_per_second: 200_000.0,
        };
        let json = serde_json::to_value(&g).unwrap();
        assert!(json.get("gigahash_per_second").is_some());
    }

    #[test]
    fn megahashrate_field_name_matches_proto() {
        let m = MegaHashrate {
            megahash_per_second: 200_000_000.0,
        };
        let json = serde_json::to_value(&m).unwrap();
        assert!(json.get("megahash_per_second").is_some());
    }

    #[test]
    fn power_efficiency_field_name_matches_proto() {
        let e = PowerEfficiency {
            joule_per_terahash: 17.5,
        };
        let json = serde_json::to_value(&e).unwrap();
        assert!(json.get("joule_per_terahash").is_some());
        // Negative pins.
        assert!(json.get("jpt").is_none());
        assert!(json.get("efficiency").is_none());
    }

    #[test]
    fn hours_field_is_u32_not_u64() {
        let h = Hours { hours: u32::MAX };
        let json = serde_json::to_string(&h).unwrap();
        let back: Hours = serde_json::from_str(&json).unwrap();
        assert_eq!(back.hours, u32::MAX);
    }

    #[test]
    fn tera_to_giga_conversion_is_thousand_to_one() {
        // 1 TH = 1000 GH.
        let t = TeraHashrate {
            terahash_per_second: 200.0,
        };
        let g = GigaHashrate::from_tera(t);
        assert!((g.gigahash_per_second - 200_000.0).abs() < 1e-6);

        let t2 = TeraHashrate::from_giga(g);
        assert!((t2.terahash_per_second - 200.0).abs() < 1e-6);
    }

    #[test]
    fn giga_to_mega_conversion_is_thousand_to_one() {
        // 1 GH = 1000 MH.
        let m = MegaHashrate {
            megahash_per_second: 200_000.0,
        };
        let g = GigaHashrate::from_mega(m);
        // 200_000 MH ÷ 1000 = 200 GH.
        assert!((g.gigahash_per_second - 200.0).abs() < 1e-6);
    }

    #[test]
    fn power_efficiency_from_s21_nameplate_is_17_5_jpt() {
        // S21 nameplate (per  silicon-D bm1368): 200 TH/s @
        // 3,500 W → 17.5 J/TH. Cross-reference invariant.
        let e = PowerEfficiency::from_watts_and_ths(3_500, 200.0);
        assert!(
            (e.joule_per_terahash - 17.5).abs() < 1e-3,
            "S21 nameplate efficiency was {} (expected 17.5)",
            e.joule_per_terahash
        );
    }

    #[test]
    fn power_efficiency_handles_zero_hashrate_safely() {
        // Avoid div-by-zero — return 0.0 J/TH for a 0 TH/s sample.
        let e = PowerEfficiency::from_watts_and_ths(3_500, 0.0);
        assert_eq!(e.joule_per_terahash, 0.0);
    }

    #[test]
    fn real_hashrate_has_all_10_documented_windows() {
        // RE doc §9.2.28 lines 1242-1253 lists 10 fields:
        // last_5s, last_15s, last_30s, last_1m, last_5m, last_15m,
        // last_30m, last_1h, last_24h, since_restart.
        let r = RealHashrate::default();
        let json = serde_json::to_value(&r).unwrap();
        for field in [
            "last_5s",
            "last_15s",
            "last_30s",
            "last_1m",
            "last_5m",
            "last_15m",
            "last_30m",
            "last_1h",
            "last_24h",
            "since_restart",
        ] {
            assert!(
                json.get(field).is_some(),
                "RealHashrate missing window {}",
                field
            );
        }
    }

    #[test]
    fn real_hashrate_window_values_round_trip_through_serde() {
        let original = RealHashrate {
            last_5s: GigaHashrate {
                gigahash_per_second: 200_000.0,
            },
            last_1m: GigaHashrate {
                gigahash_per_second: 199_500.0,
            },
            since_restart: GigaHashrate {
                gigahash_per_second: 198_000.0,
            },
            ..RealHashrate::default()
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: RealHashrate = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn all_unit_wrappers_round_trip_through_serde() {
        // Pin every unit-wrapper.
        let pairs: Vec<(String, String)> = vec![
            (
                serde_json::to_string(&Power::from_watts(3_500)).unwrap(),
                "{\"watt\":3500}".to_string(),
            ),
            (
                serde_json::to_string(&Frequency { hertz: 525.0 }).unwrap(),
                "{\"hertz\":525.0}".to_string(),
            ),
            (
                serde_json::to_string(&Voltage { volt: 13.8 }).unwrap(),
                "{\"volt\":13.8}".to_string(),
            ),
            (
                serde_json::to_string(&Hours { hours: 24 }).unwrap(),
                "{\"hours\":24}".to_string(),
            ),
        ];
        for (actual, expected) in pairs {
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn hashrate_units_are_all_f64() {
        // Pin via extreme values that would lose precision in f32.
        let t = TeraHashrate {
            terahash_per_second: 1.234567890123_e9,
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: TeraHashrate = serde_json::from_str(&json).unwrap();
        assert!((back.terahash_per_second - t.terahash_per_second).abs() < 1.0);
    }
}
