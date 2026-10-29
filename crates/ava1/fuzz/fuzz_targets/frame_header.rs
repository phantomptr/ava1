#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(h) = <[u8; 16]>::try_from(data.get(..16).unwrap_or(&[])) {
        if let Ok(x) = ava1::frame::Header::decode(&h) {
            assert_eq!(x.encode(), h);
        }
    }
});
