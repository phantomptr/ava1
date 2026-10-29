#![cfg(unix)]
use ava1_ctest::ffi;

/// The slot is one global: tests that write it take turns.
static SLOT: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn the_c_slot_is_what_the_rust_stamper_looks_for() {
    let _turn = SLOT.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let slot = std::ptr::addr_of_mut!(ffi::ava1_trust_slot);
        let mut bytes = std::ptr::read_volatile(slot);
        assert_eq!(ava1::trust::read(&bytes), None);
        let mut out = [0u8; 32];
        assert_eq!(ffi::ava1_trust_slot_key(out.as_mut_ptr()), -1, "unstamped");
        ava1::trust::stamp(&mut bytes, &[0x5a; 32]).unwrap();
        std::ptr::write_volatile(slot, bytes);
        assert_eq!(ffi::ava1_trust_slot_key(out.as_mut_ptr()), 0);
        assert_eq!(out, [0x5a; 32]);
        bytes[9] = 0;
        std::ptr::write_volatile(slot, bytes);
    }
}

#[test]
fn a_launch_stamp_reaches_the_c_reader_and_a_key_stamp_carries_no_token() {
    let _turn = SLOT.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let slot = std::ptr::addr_of_mut!(ffi::ava1_trust_slot);
        let orig = std::ptr::read_volatile(slot);
        let mut bytes = orig;
        let (mut key, mut tok) = ([0u8; 32], [0u8; 16]);
        assert_eq!(
            ffi::ava1_trust_slot_token(tok.as_mut_ptr()),
            -1,
            "unstamped"
        );
        ava1::trust::stamp_launch(&mut bytes, &[0x5a; 32], &[0xc3; 16]).unwrap();
        std::ptr::write_volatile(slot, bytes);
        assert_eq!(ffi::ava1_trust_slot_key(key.as_mut_ptr()), 0);
        assert_eq!(ffi::ava1_trust_slot_token(tok.as_mut_ptr()), 0);
        assert_eq!((key, tok), ([0x5a; 32], [0xc3; 16]));
        ava1::trust::stamp(&mut bytes, &[0x5b; 32]).unwrap();
        std::ptr::write_volatile(slot, bytes);
        assert_eq!(ffi::ava1_trust_slot_key(key.as_mut_ptr()), 0);
        assert_eq!(key, [0x5b; 32]);
        assert_eq!(ffi::ava1_trust_slot_token(tok.as_mut_ptr()), -1);
        std::ptr::write_volatile(slot, orig);
    }
}

#[test]
fn the_c_launch_proof_matches_rust_and_the_vectors() {
    for l in include_str!("../../../vectors/launch.txt").lines() {
        if l.starts_with('#') || l.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = l.split_whitespace().collect();
        let t: [u8; 16] = ava1::hex::decode(f[0]).unwrap().try_into().unwrap();
        let h: [u8; 64] = ava1::hex::decode(f[1]).unwrap().try_into().unwrap();
        let mut out = [0u8; 16];
        unsafe { ffi::ava1_launch_proof(t.as_ptr(), h.as_ptr(), out.as_mut_ptr()) };
        assert_eq!(ava1::hex::encode(&out), f[2]);
        assert_eq!(out, ava1::launch::proof(&t, &h));
    }
}
