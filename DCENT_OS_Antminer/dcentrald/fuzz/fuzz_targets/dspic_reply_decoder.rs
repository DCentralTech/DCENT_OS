#![no_main]
//! Fuzz the dsPIC/APW voltage-reply decoders.
//!
//! These decoders (`dcentrald-common`, no-HAL) parse bytes off known-degraded
//! hardware — the `a lab unit` dsPIC (0x22) emits documented "framed noise", fw=0x86
//! chips echo garbage — and they run inside the teardown / safe-off sequence. A
//! slice-index or length-arithmetic panic there would abort safe-off mid-flight.
//! The invariant this target enforces is therefore simply: **none of the three
//! decoders may panic on ANY input.** Return values are intentionally discarded.
//!
//! The same no-panic + fail-closed properties are also pinned deterministically
//! by the proptests in `dcentrald-common/src/dspic_decode.rs`; this target adds
//! coverage-guided exploration for CI / long-run fuzzing.

use libfuzzer_sys::fuzz_target;

use dcentrald_common::dspic_decode::{
    decode_bare_voltage_reply, decode_framed_measure_voltage_i2c0_capture,
    decode_framed_measure_voltage_reply,
};

/// dsPIC physical max (mirrors `DSPIC_MAX_VOLTAGE_MV` in `dcentrald-asic`).
const MAX_MV: u16 = 15_140;

fuzz_target!(|data: &[u8]| {
    // Split the input: the first bytes drive the bare decoder's scalar args and
    // a varying max bound; the remainder feeds the two framed decoders.
    if let [cmd, exp, hi, lo, max_hi, max_lo, rest @ ..] = data {
        let max = u16::from_be_bytes([*max_hi, *max_lo]);
        let _ = decode_bare_voltage_reply(*cmd, *exp, *hi, *lo, max);
        let _ = decode_framed_measure_voltage_reply(rest, max);
        let _ = decode_framed_measure_voltage_i2c0_capture(rest, max);
    }
    // Always exercise the framed decoders against the full buffer at the fixed
    // physical max as well, so short inputs are covered too.
    let _ = decode_framed_measure_voltage_reply(data, MAX_MV);
    let _ = decode_framed_measure_voltage_i2c0_capture(data, MAX_MV);
});
