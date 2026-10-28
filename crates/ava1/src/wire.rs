//! Encoding rules shared by every generated message (SPEC.md §3).

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    #[error("message ends early")]
    Short,
    #[error("{0} unexpected bytes after the message")]
    Trailing(usize),
    #[error("text field is not valid UTF-8")]
    Utf8,
    #[error("extension tag {0} appears twice")]
    DupExt(u16),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EncodeError {
    #[error("text field of {0} bytes is over 65535")]
    StrTooLong(usize),
    #[error("byte field of {0} bytes is over 4 GiB")]
    BytesTooLong(usize),
}

#[derive(Default)]
pub struct Writer {
    pub buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn fixed(&mut self, v: &[u8]) {
        self.buf.extend_from_slice(v);
    }
    pub fn bytes(&mut self, v: &[u8]) -> Result<(), EncodeError> {
        let n = u32::try_from(v.len()).map_err(|_| EncodeError::BytesTooLong(v.len()))?;
        self.u32(n);
        self.fixed(v);
        Ok(())
    }
    pub fn str(&mut self, v: &str) -> Result<(), EncodeError> {
        let n = u16::try_from(v.len()).map_err(|_| EncodeError::StrTooLong(v.len()))?;
        self.u16(n);
        self.fixed(v.as_bytes());
        Ok(())
    }
    /// An extension field: tag, u32 value length, then the value `f` writes.
    pub fn ext(
        &mut self,
        tag: u16,
        f: impl FnOnce(&mut Writer) -> Result<(), EncodeError>,
    ) -> Result<(), EncodeError> {
        self.u16(tag);
        let at = self.buf.len();
        self.u32(0);
        f(self)?;
        let n = (self.buf.len() - at - 4) as u32;
        self.buf[at..at + 4].copy_from_slice(&n.to_le_bytes());
        Ok(())
    }
    /// A list of messages: u32 total length, then `u32 len ‖ item` per item (SPEC.md §3).
    /// An error leaves the buffer undefined — completed items, a zeroed length and part
    /// of the item that failed — so drop the writer, as with `ext`.
    pub fn records<M: Message>(&mut self, items: &[M]) -> Result<(), EncodeError> {
        let at = self.buf.len();
        self.u32(0);
        for m in items {
            let item_at = self.buf.len();
            self.u32(0);
            m.encode_into(self)?;
            self.patch_len(item_at)?;
        }
        self.patch_len(at)
    }

    fn patch_len(&mut self, at: usize) -> Result<(), EncodeError> {
        let n = self.buf.len() - at - 4;
        let n32 = u32::try_from(n).map_err(|_| EncodeError::BytesTooLong(n))?;
        self.buf[at..at + 4].copy_from_slice(&n32.to_le_bytes());
        Ok(())
    }
}

pub struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Self { b, pos: 0 }
    }
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.pos.checked_add(n).ok_or(DecodeError::Short)?;
        let s = self.b.get(self.pos..end).ok_or(DecodeError::Short)?;
        self.pos = end;
        Ok(s)
    }
    pub fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_le_bytes(self.fixed::<2>()?))
    }
    pub fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(self.fixed::<4>()?))
    }
    pub fn u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_le_bytes(self.fixed::<8>()?))
    }
    pub fn fixed<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let mut a = [0u8; N];
        a.copy_from_slice(self.take(N)?);
        Ok(a)
    }
    pub fn bytes(&mut self) -> Result<Vec<u8>, DecodeError> {
        let n = self.u32()? as usize;
        Ok(self.take(n)?.to_vec())
    }
    pub fn str(&mut self) -> Result<String, DecodeError> {
        let n = self.u16()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| DecodeError::Utf8)
    }
    /// A list of messages: u32 total length, then `u32 len ‖ item` per item (SPEC.md §3).
    pub fn records<M: Message>(&mut self) -> Result<Vec<M>, DecodeError> {
        let n = self.u32()? as usize;
        let mut r = Reader::new(self.take(n)?);
        let mut out = Vec::new();
        while r.pos < r.b.len() {
            let len = r.u32()? as usize;
            out.push(M::decode(r.take(len)?)?);
        }
        Ok(out)
    }
    pub fn finish(&self) -> Result<(), DecodeError> {
        match self.b.len() - self.pos {
            0 => Ok(()),
            left => Err(DecodeError::Trailing(left)),
        }
    }
}

pub trait Message: Sized {
    const NAME: &'static str;
    fn encode_into(&self, w: &mut Writer) -> Result<(), EncodeError>;
    fn decode(b: &[u8]) -> Result<Self, DecodeError>;
    fn to_bytes(&self) -> Result<Vec<u8>, EncodeError> {
        let mut w = Writer::new();
        self.encode_into(&mut w)?;
        Ok(w.buf)
    }
}

/// A message that travels as a frame of its own type.
pub trait FrameMessage: Message {
    const TYPE: u8;
}

/// Deterministic generator for conformance samples (not for keys).
pub struct SplitMix(pub u64);

impl SplitMix {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
    pub fn fill(&mut self, b: &mut [u8]) {
        for x in b {
            *x = self.next_u64() as u8;
        }
    }
    pub fn ascii(&mut self, max: usize) -> String {
        let n = self.below(max as u64 + 1) as usize;
        (0..n)
            .map(|_| (b'a' + self.below(26) as u8) as char)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::{self, Error, NodeInfo, PairConfirm, Ping, RpcRequest};

    fn hex(s: &str) -> Vec<u8> {
        crate::hex::decode(s).unwrap()
    }

    #[test]
    fn golden_messages() {
        let ping = Ping {
            seq: 5,
            t_us: 0x0102_0304_0506_0708,
        };
        assert_eq!(
            ping.to_bytes().unwrap(),
            hex("0500000008070605040302010000")
        );
        let e = Error {
            code: 0x0102,
            message: "no".into(),
        };
        assert_eq!(e.to_bytes().unwrap(), hex("020102006e6f0000"));
        let r = RpcRequest {
            method: 1,
            body: vec![0xaa, 0xbb],
        };
        assert_eq!(r.to_bytes().unwrap(), hex("010002000000aabb0000"));
        assert_eq!(
            PairConfirm { mac: [0xab; 32] }.to_bytes().unwrap(),
            hex(&format!("{}0000", "ab".repeat(32)))
        );
        let n = NodeInfo {
            version: "5".into(),
            platform: "ps5".into(),
            name: "Pro".into(),
            firmware: Some("13.60".into()),
        };
        let b = hex("0100350300707335030050726f0100010007000000050031332e3630");
        assert_eq!(n.to_bytes().unwrap(), b);
        assert_eq!(NodeInfo::decode(&b).unwrap(), n);
    }

    #[test]
    fn every_vector_round_trips() {
        for l in include_str!("../../../vectors/messages.txt").lines() {
            if l.starts_with('#') || l.trim().is_empty() {
                continue;
            }
            let (name, h) = l.split_once(' ').unwrap();
            let b = hex(h);
            assert_eq!(gen::roundtrip(name, &b), Some(Ok(b.clone())), "{name}");
        }
    }

    #[test]
    fn unknown_extensions_are_skipped() {
        let b = hex("0500000008070605040302010100090002000000abcd");
        assert_eq!(
            Ping::decode(&b).unwrap(),
            Ping {
                seq: 5,
                t_us: 0x0102_0304_0506_0708
            }
        );
    }

    #[test]
    fn malformed_messages_are_refused() {
        assert_eq!(Ping::decode(&hex("05000000")), Err(DecodeError::Short));
        assert_eq!(
            Ping::decode(&hex("050000000807060504030201000000")),
            Err(DecodeError::Trailing(1))
        );
        assert_eq!(
            Error::decode(&hex("01000100ff0000")),
            Err(DecodeError::Utf8)
        );
        let mut w = Writer::new();
        w.str("5").unwrap();
        w.str("ps5").unwrap();
        w.str("Pro").unwrap();
        w.u16(2);
        w.ext(1, |w| w.str("a")).unwrap();
        w.ext(1, |w| w.str("b")).unwrap();
        assert_eq!(NodeInfo::decode(&w.buf), Err(DecodeError::DupExt(1)));
    }

    #[test]
    fn samples_round_trip() {
        let mut rng = SplitMix(7);
        for _ in 0..200 {
            for name in gen::ALL {
                let b = gen::sample(name, &mut rng).unwrap();
                assert_eq!(gen::roundtrip(name, &b), Some(Ok(b.clone())), "{name}");
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    struct Two {
        a: u8,
        s: String,
    }
    impl Message for Two {
        const NAME: &'static str = "Two";
        fn encode_into(&self, w: &mut Writer) -> Result<(), EncodeError> {
            w.u8(self.a);
            w.str(&self.s)?;
            w.u16(0);
            Ok(())
        }
        fn decode(b: &[u8]) -> Result<Self, DecodeError> {
            let mut r = Reader::new(b);
            let m = Two {
                a: r.u8()?,
                s: r.str()?,
            };
            let _ = r.u16()?;
            r.finish()?;
            Ok(m)
        }
    }

    #[test]
    fn records_encode_as_length_prefixed_items() {
        let items = vec![
            Two {
                a: 1,
                s: "x".into(),
            },
            Two {
                a: 2,
                s: String::new(),
            },
        ];
        let mut w = Writer::new();
        w.records(&items).unwrap();
        // blob = 06000000 ‖ 01 0100 78 0000 ‖ 05000000 ‖ 02 0000 0000 = 19 bytes
        assert_eq!(w.buf, hex("1300000006000000010100780000050000000200000000"));
        let mut r = Reader::new(&w.buf);
        assert_eq!(r.records::<Two>().unwrap(), items);
        r.finish().unwrap();
        let mut w = Writer::new();
        w.records::<Two>(&[]).unwrap();
        assert_eq!(w.buf, hex("00000000"));
    }

    #[test]
    fn records_with_a_bad_item_are_refused() {
        // One item claims 9 bytes but the blob holds 5.
        let b = hex("09000000090000000101007800");
        assert_eq!(Reader::new(&b).records::<Two>(), Err(DecodeError::Short));
        // An item with a trailing byte inside its own length.
        let b = hex("0b0000000700000001010078000000");
        assert!(Reader::new(&b).records::<Two>().is_err());
        // The blob claims more than the buffer holds.
        assert_eq!(
            Reader::new(&hex("0900000001000000")).records::<Two>(),
            Err(DecodeError::Short)
        );
        // A zero-length item: no struct encodes to nothing (each carries its ext count).
        assert!(Reader::new(&hex("0400000000000000"))
            .records::<Two>()
            .is_err());
        // A byte after the last item that does not start one.
        let b = hex("0b00000006000000010100780000ff");
        assert!(Reader::new(&b).records::<Two>().is_err());
    }

    /// A struct that lists `Two`s, so a records field can hold records inside its items.
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    struct Wrap {
        items: Vec<Two>,
    }
    impl Message for Wrap {
        const NAME: &'static str = "Wrap";
        fn encode_into(&self, w: &mut Writer) -> Result<(), EncodeError> {
            w.records(&self.items)?;
            w.u16(0);
            Ok(())
        }
        fn decode(b: &[u8]) -> Result<Self, DecodeError> {
            let mut r = Reader::new(b);
            let m = Wrap {
                items: r.records::<Two>()?,
            };
            let _ = r.u16()?;
            r.finish()?;
            Ok(m)
        }
    }

    #[test]
    fn records_nest_and_the_outer_length_follows_the_inner_ones() {
        let outer = vec![
            Wrap {
                items: vec![
                    Two {
                        a: 1,
                        s: "x".into(),
                    },
                    Two {
                        a: 2,
                        s: String::new(),
                    },
                ],
            },
            Wrap::default(),
        ];
        let mut w = Writer::new();
        w.records(&outer).unwrap();
        // Each Wrap is its records field (the inner blob: the 19 bytes the flat test pins)
        // plus its own u16 ext count, and every item carries its own u32 length.
        assert_eq!(
            w.buf,
            hex("27000000190000001300000006000000010100780000050000000200000000000006000000000000000000")
        );
        let mut r = Reader::new(&w.buf);
        assert_eq!(r.records::<Wrap>().unwrap(), outer);
        r.finish().unwrap();
    }
}
