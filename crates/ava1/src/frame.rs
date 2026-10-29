//! The 16-byte AVA1 frame header (SPEC.md §2).

use crate::crc32c::crc32c;

/// The two bytes every frame header starts with (SPEC.md §2).
pub const MAGIC: [u8; 2] = *b"A1";
/// Length of a frame header in bytes (SPEC.md §2).
pub const HEADER_LEN: usize = 16;
/// Largest body any AVA1 frame may carry.
pub const MAX_BODY: u32 = 16 * 1024 * 1024;
/// Largest body accepted on a control connection or before authentication.
pub const CONTROL_MAX_BODY: u32 = 64 * 1024;
/// SPEC.md §7.4: the largest RPC request body.
pub const RPC_REQUEST_MAX: usize = 56 * 1024;
/// SPEC.md §7.4: the largest RPC reply body. Replies travel on the control connection, whose
/// reader accepts `RPC_REPLY_MAX` plus framing (`RPC_FRAME_SLACK`) once the handshake is done.
pub const RPC_REPLY_MAX: usize = 256 * 1024;
/// Room for an `RpcResponse`'s status, length, extension count and the AEAD tag.
pub const RPC_FRAME_SLACK: usize = 1024;
/// The body is ChaCha20-Poly1305 ciphertext followed by its 16-byte MAC (every frame
/// after the handshake).
pub const FLAG_SEALED: u8 = 0x01;
/// A receiver that does not know this frame type skips it instead of closing.
pub const FLAG_IGNORABLE: u8 = 0x02;

/// A frame header (SPEC.md §2). Its 16 encoded bytes end with a CRC-32C of the first 12.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// Frame type.
    pub ty: u8,
    /// Flags: [`FLAG_SEALED`], [`FLAG_IGNORABLE`]; other bits are passed through unchecked.
    pub flags: u8,
    /// Channel; its meaning depends on the frame type (an RPC's request id, for example).
    pub channel: u32,
    /// Body length in bytes, MAC included when sealed. [`Header::decode`] refuses more than
    /// [`MAX_BODY`].
    pub body_len: u32,
}

/// Why a frame header was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HeaderError {
    /// The first two bytes are not [`MAGIC`].
    #[error("not an AVA1 frame (bad magic)")]
    BadMagic,
    /// The CRC-32C of bytes 0..12 does not match bytes 12..16.
    #[error("frame header checksum mismatch")]
    BadCrc,
    /// The body length is over [`MAX_BODY`] or the reader's lower cap; also returned for a
    /// body too large to send.
    #[error("frame body of {0} bytes is over the limit")]
    TooLong(u32),
}

impl Header {
    /// The 16 header bytes, CRC included. Does not check `body_len` against `MAX_BODY`.
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[0..2].copy_from_slice(&MAGIC);
        b[2] = self.ty;
        b[3] = self.flags;
        b[4..8].copy_from_slice(&self.channel.to_le_bytes());
        b[8..12].copy_from_slice(&self.body_len.to_le_bytes());
        let crc = crc32c(&b[0..12]);
        b[12..16].copy_from_slice(&crc.to_le_bytes());
        b
    }

    /// Parses a header, checking the magic, the CRC and `body_len <= MAX_BODY`, in that order.
    pub fn decode(b: &[u8; HEADER_LEN]) -> Result<Self, HeaderError> {
        if b[0..2] != MAGIC {
            return Err(HeaderError::BadMagic);
        }
        if u32::from_le_bytes([b[12], b[13], b[14], b[15]]) != crc32c(&b[0..12]) {
            return Err(HeaderError::BadCrc);
        }
        let body_len = u32::from_le_bytes([b[8], b[9], b[10], b[11]]);
        if body_len > MAX_BODY {
            return Err(HeaderError::TooLong(body_len));
        }
        Ok(Header {
            ty: b[2],
            flags: b[3],
            channel: u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
            body_len,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vectors() -> Vec<(Header, [u8; HEADER_LEN])> {
        include_str!("../../../vectors/frame_header.txt")
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .map(|l| {
                let f: Vec<&str> = l.split_whitespace().collect();
                let h = Header {
                    ty: u8::from_str_radix(f[0], 16).unwrap(),
                    flags: u8::from_str_radix(f[1], 16).unwrap(),
                    channel: f[2].parse().unwrap(),
                    body_len: f[3].parse().unwrap(),
                };
                let b: [u8; HEADER_LEN] = crate::hex::decode(f[4]).unwrap().try_into().unwrap();
                (h, b)
            })
            .collect()
    }

    #[test]
    fn golden_headers_encode_and_decode() {
        let v = vectors();
        assert_eq!(v.len(), 2);
        for (h, b) in v {
            assert_eq!(h.encode(), b);
            assert_eq!(Header::decode(&b), Ok(h));
        }
    }

    #[test]
    fn a_flipped_bit_is_caught() {
        let (_, mut b) = vectors()[0];
        b[5] ^= 1;
        assert_eq!(Header::decode(&b), Err(HeaderError::BadCrc));
    }

    #[test]
    fn wrong_magic_is_not_a_frame() {
        let (_, mut b) = vectors()[0];
        b[0] = b'B';
        assert_eq!(Header::decode(&b), Err(HeaderError::BadMagic));
    }

    #[test]
    fn a_body_over_16_mib_is_refused() {
        let b = Header {
            ty: 1,
            flags: 0,
            channel: 0,
            body_len: MAX_BODY + 1,
        }
        .encode();
        assert_eq!(Header::decode(&b), Err(HeaderError::TooLong(MAX_BODY + 1)));
    }
}
