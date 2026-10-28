//! Lowercase hex, for peer files and logs.

pub fn encode(b: &[u8]) -> String {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for &x in b {
        s.push(D[(x >> 4) as usize] as char);
        s.push(D[(x & 15) as usize] as char);
    }
    s
}

pub fn decode(s: &str) -> Option<Vec<u8>> {
    let s = s.as_bytes();
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let v = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    s.chunks(2)
        .map(|p| Some(v(p[0])? << 4 | v(p[1])?))
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn hex_round_trips_and_rejects_junk() {
        assert_eq!(super::encode(&[0x00, 0xab, 0xff]), "00abff");
        assert_eq!(super::decode("00ABff"), Some(vec![0x00, 0xab, 0xff]));
        assert_eq!(super::decode("abc"), None);
        assert_eq!(super::decode("zz"), None);
    }
}
