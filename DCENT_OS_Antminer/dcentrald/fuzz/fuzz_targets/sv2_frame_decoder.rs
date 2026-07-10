#![no_main]

use dcentrald_stratum::v2::framing::FrameDecoder;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut decoder = FrameDecoder::new();
    let chunk_len = data.first().map(|b| (*b as usize % 64) + 1).unwrap_or(1);

    for chunk in data.chunks(chunk_len) {
        decoder.feed(chunk);
        for _ in 0..8 {
            match decoder.next_frame() {
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break,
            }
        }
    }

    for _ in 0..8 {
        match decoder.next_frame() {
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
});
