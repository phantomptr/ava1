#![cfg(unix)]
use ava1::frame::Header;
use ava1::gen;
use ava1::wire::SplitMix;
use ava1_ctest::*;

#[test]
fn crc_and_headers_match_rust() {
    assert_eq!(c_crc32c(b"123456789"), 0xE306_9283);
    let mut rng = SplitMix(1);
    for _ in 0..1000 {
        let h = Header {
            ty: rng.next_u64() as u8,
            flags: rng.next_u64() as u8,
            channel: rng.next_u64() as u32,
            body_len: rng.below(ava1::frame::MAX_BODY as u64 + 1) as u32,
        };
        let b = h.encode();
        assert_eq!(c_header_encode(h), b);
        assert_eq!(c_header_decode(&b), Ok(h));
    }
    let mut bad = Header {
        ty: 1,
        flags: 0,
        channel: 0,
        body_len: 0,
    }
    .encode();
    bad[3] ^= 0x40;
    assert!(c_header_decode(&bad).is_err());
}

#[test]
fn golden_vectors_round_trip_in_c() {
    for l in include_str!("../../../vectors/messages.txt").lines() {
        if l.starts_with('#') || l.trim().is_empty() {
            continue;
        }
        let (name, h) = l.split_once(' ').unwrap();
        let b = ava1::hex::decode(h).unwrap();
        assert_eq!(c_roundtrip(name, &b), Ok(b.clone()), "{name}");
    }
}

#[test]
fn rust_samples_round_trip_in_c() {
    let mut rng = SplitMix(42);
    for _ in 0..500 {
        for name in gen::ALL {
            let b = gen::sample(name, &mut rng).unwrap();
            assert_eq!(c_roundtrip(name, &b), Ok(b.clone()), "{name}");
        }
    }
}

#[test]
fn c_and_rust_agree_on_garbage() {
    // Random bytes: both sides must accept exactly the same inputs. Re-encodings are
    // compared on canonical bytes (SPEC.md §3): a re-encoder may drop what it accepted but
    // would not have written (an unknown extension inside a records item), so C's
    // verbatim item bytes and Rust's re-encoded ones are both correct.
    let mut rng = SplitMix(9);
    for _ in 0..20_000 {
        let name = gen::ALL[rng.below(gen::ALL.len() as u64) as usize];
        let n = rng.below(120) as usize;
        let mut b = vec![0u8; n];
        rng.fill(&mut b);
        let rust = gen::roundtrip(name, &b).unwrap();
        let c = c_roundtrip(name, &b);
        assert_eq!(rust.is_ok(), c.is_ok(), "{name} {}", ava1::hex::encode(&b));
        if let Ok(r) = rust {
            assert_eq!(
                c_roundtrip(name, &r),
                Ok(r.clone()),
                "C must reproduce Rust's canonical bytes: {name} {}",
                ava1::hex::encode(&r)
            );
        }
        if let Ok(c) = c {
            assert_eq!(
                c_roundtrip(name, &c),
                Ok(c.clone()),
                "re-encoding twice changes nothing: {name}"
            );
        }
    }
}

#[test]
fn c_counts_and_rebuilds_a_records_blob_with_the_generated_helpers() {
    // The per-struct helpers the payload uses to build and walk records blobs: nothing
    // else on the host calls them, so a defect in the emitted C would otherwise ship
    // unnoticed. The items are Rust's canonical encodings; C must count them and
    // re-append them to exactly those bytes.
    use ava1::wire::Message;
    let items = vec![
        gen::NodeInfo {
            version: "1.0".into(),
            platform: "ps5".into(),
            name: "console".into(),
            firmware: Some("13.60".into()),
        },
        gen::NodeInfo {
            version: String::new(),
            platform: "host".into(),
            name: String::new(),
            firmware: None,
        },
    ];
    let mut blob = Vec::new();
    for it in &items {
        let b = it.to_bytes().unwrap();
        blob.extend_from_slice(&(b.len() as u32).to_le_bytes());
        blob.extend_from_slice(&b);
    }
    let (out, count) = c_records_helpers(&blob).unwrap();
    assert_eq!(count, 2);
    assert_eq!(out, blob, "C re-appends the items it read");
    // An empty list, and a truncated item: the second must be refused, not counted.
    assert_eq!(c_records_helpers(&[]).unwrap(), (Vec::new(), 0));
    let mut bad = blob.clone();
    bad.truncate(bad.len() - 1);
    assert!(c_records_helpers(&bad).is_err());
}

#[test]
fn c_utf8_matches_rust_exactly() {
    const B: [u8; 19] = [
        0x00, 0x7F, 0x80, 0x8F, 0x90, 0x9F, 0xA0, 0xBF, 0xC0, 0xC1, 0xC2, 0xDF, 0xE0, 0xED, 0xEF,
        0xF0, 0xF4, 0xF5, 0xFF,
    ];
    let check = |b: &[u8]| assert_eq!(c_utf8_valid(b), std::str::from_utf8(b).is_ok(), "{b:02x?}");
    for a in 0..=255u8 {
        check(&[a]);
        for &b in &B {
            check(&[a, b]);
            for &c in &B {
                check(&[a, b, c]);
            }
        }
    }
    for &a in &B {
        for &b in &B {
            for &c in &B {
                for &d in &B {
                    check(&[a, b, c, d]);
                }
            }
        }
    }
}

/// Start of the ext count (u16) such that the ext blocks after it tile the buffer exactly.
fn ext_start(b: &[u8]) -> Option<usize> {
    (0..b.len().saturating_sub(1)).rev().find(|&p| {
        let n = u16::from_le_bytes([b[p], b[p + 1]]);
        let mut at = p + 2;
        for _ in 0..n {
            if at + 6 > b.len() {
                return false;
            }
            let l = u32::from_le_bytes(b[at + 2..at + 6].try_into().unwrap()) as usize;
            at += 6;
            if l > b.len() - at {
                return false;
            }
            at += l;
        }
        at == b.len()
    })
}

fn ext_blocks(b: &[u8], p: usize) -> Vec<(usize, usize)> {
    let n = u16::from_le_bytes([b[p], b[p + 1]]);
    let mut at = p + 2;
    let mut v = Vec::new();
    for _ in 0..n {
        let l = u32::from_le_bytes(b[at + 2..at + 6].try_into().unwrap()) as usize;
        v.push((at, at + 6 + l));
        at += 6 + l;
    }
    v
}

fn mutate(b: &[u8], rng: &mut SplitMix) -> Vec<u8> {
    let mut m = b.to_vec();
    match rng.below(7) {
        0 if !m.is_empty() => {
            let i = rng.below(m.len() as u64) as usize;
            m[i] ^= 1 << rng.below(8);
        }
        1 => m.truncate(rng.below(m.len() as u64 + 1) as usize),
        2 => {
            let mut x = vec![0u8; 1 + rng.below(8) as usize];
            rng.fill(&mut x);
            m.extend(x);
        }
        3 => {
            if let Some(p) = ext_start(&m) {
                let n = u16::from_le_bytes([m[p], m[p + 1]]).wrapping_add(1);
                m[p..p + 2].copy_from_slice(&n.to_le_bytes());
                let mut val = vec![0u8; rng.below(6) as usize];
                rng.fill(&mut val);
                m.extend(0x7f00u16.to_le_bytes());
                m.extend((val.len() as u32).to_le_bytes());
                m.extend(val);
            }
        }
        4 => {
            if let Some(p) = ext_start(&m) {
                let blocks = ext_blocks(&m, p);
                if !blocks.is_empty() {
                    let (s, e) = blocks[rng.below(blocks.len() as u64) as usize];
                    let n = u16::from_le_bytes([m[p], m[p + 1]]).wrapping_add(1);
                    m[p..p + 2].copy_from_slice(&n.to_le_bytes());
                    let dup = m[s..e].to_vec();
                    m.extend(dup);
                }
            }
        }
        5 if !m.is_empty() => {
            const BAD: [&[u8]; 5] = [
                &[0xC0, 0x80],
                &[0xFF],
                &[0xED, 0xA0, 0x80],
                &[0xF4, 0x90, 0x80, 0x80],
                &[0xE0, 0x80, 0x80],
            ];
            let bad = BAD[rng.below(BAD.len() as u64) as usize];
            let i = rng.below(m.len() as u64) as usize;
            for (k, &x) in bad.iter().enumerate() {
                if i + k < m.len() {
                    m[i + k] = x;
                }
            }
        }
        _ => {
            // Grow an ext value's declared length so the value has trailing bytes inside.
            if let Some(p) = ext_start(&m) {
                let blocks = ext_blocks(&m, p);
                if let Some(&(s, e)) = blocks.first() {
                    m.push(rng.next_u64() as u8);
                    let l = (e - s - 6 + 1) as u32;
                    m[s + 2..s + 6].copy_from_slice(&l.to_le_bytes());
                    // Move the extra byte to the end of that value.
                    let extra = m.pop().unwrap();
                    m.insert(e, extra);
                }
            }
        }
    }
    m
}

#[test]
fn c_and_rust_agree_on_mutated_messages() {
    let mut seeds: Vec<(String, Vec<u8>)> = Vec::new();
    let mut rng = SplitMix(77);
    for l in include_str!("../../../vectors/messages.txt").lines() {
        if l.starts_with('#') || l.trim().is_empty() {
            continue;
        }
        let (name, h) = l.split_once(' ').unwrap();
        seeds.push((name.to_string(), ava1::hex::decode(h).unwrap()));
    }
    let mut total = 0usize;
    let mut accepted = 0usize;
    for _ in 0..800 {
        let mut round: Vec<(String, Vec<u8>)> = gen::ALL
            .iter()
            .map(|n| (n.to_string(), gen::sample(n, &mut rng).unwrap()))
            .collect();
        round.extend(seeds.iter().cloned());
        for (name, b) in &round {
            let m = mutate(b, &mut rng);
            let rust = gen::roundtrip(name, &m).unwrap();
            let c = c_roundtrip(name, &m);
            assert_eq!(rust.is_ok(), c.is_ok(), "{name} {}", ava1::hex::encode(&m));
            if let Ok(r) = rust {
                // Canonical bytes only (SPEC.md §3): a mutation can leave an accepted
                // message the encoder would not have written — an extension inside a
                // records item that Rust drops and C reproduces — and that difference is
                // not a divergence. Canonical output must still match exactly.
                assert_eq!(
                    c_roundtrip(name, &r),
                    Ok(r.clone()),
                    "{name} {}",
                    ava1::hex::encode(&m)
                );
                accepted += 1;
            }
            if let Ok(c) = c {
                assert_eq!(
                    c_roundtrip(name, &c),
                    Ok(c.clone()),
                    "re-encoding twice changes nothing: {name}"
                );
            }
            total += 1;
        }
    }
    assert!(total >= 20_000, "{total}");
    assert!(
        accepted > total / 20,
        "mutations too destructive: {accepted}/{total}"
    );
}
