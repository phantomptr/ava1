#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&pick, rest)) = data.split_first() else {
        return;
    };
    let name = ava1::gen::ALL[pick as usize % ava1::gen::ALL.len()];
    if let Some(Ok(bytes)) = ava1::gen::roundtrip(name, rest) {
        // Whatever decodes re-encodes to a canonical form that is stable.
        assert_eq!(ava1::gen::roundtrip(name, &bytes), Some(Ok(bytes.clone())));
    }
});
