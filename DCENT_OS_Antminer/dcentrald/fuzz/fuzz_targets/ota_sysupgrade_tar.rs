#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = dcentrald_api::ota_signature::fuzz_read_sysupgrade_tar_bytes(data);
});
