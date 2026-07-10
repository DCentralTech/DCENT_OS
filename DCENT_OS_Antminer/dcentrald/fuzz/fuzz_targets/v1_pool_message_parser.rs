#![no_main]

use dcentrald_stratum::v1::messages::parse_pool_message;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let line = String::from_utf8_lossy(data);
    let _ = parse_pool_message(&line);

    for part in line.split('\n').take(16) {
        let _ = parse_pool_message(part);
    }
});
