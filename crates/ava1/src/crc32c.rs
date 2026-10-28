//! CRC-32C (Castagnoli, reflected), used only for the frame header (SPEC.md §2).

const POLY: u32 = 0x82F6_3B78;

const fn table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { (c >> 1) ^ POLY } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

static TABLE: [u32; 256] = table();

pub fn crc32c(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in data {
        c = TABLE[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

#[cfg(test)]
mod tests {
    #[test]
    fn crc32c_check_value() {
        assert_eq!(super::crc32c(b"123456789"), 0xE306_9283);
        assert_eq!(super::crc32c(b""), 0);
    }
}
