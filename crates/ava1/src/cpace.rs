//! Pairing PAKE (SPEC.md §4.6, §5.5): CPace-style, on top of the Noise session.
//!
//! The console draws a random six-digit code and shows it on its own screen only. Both sides
//! derive a generator `G = Elligator2(BLAKE2b-256("AVA1 CPace" ‖ h ‖ code))` (`h` the Noise
//! handshake hash, the code as six ASCII digits), exchange `Y = x·G` for fresh secret scalars
//! `x`, and derive `K = BLAKE2b-256("AVA1 CPace K" ‖ h ‖ X25519(x, Y_peer) ‖ Y_a ‖ Y_b)`.
//! Key confirmation is `MAC_K("client" ‖ h)` then `MAC_K("server" ‖ h)`, keyed BLAKE2b-256.
//! Someone who does not know the code cannot compute `G`, hence cannot compute `K`: one online
//! guess per attempt, and nothing on the wire lets a guess be tested offline.
//!
//! The Elligator2 map is a port of Monocypher's `crypto_elligator_map` (the C payload
//! calls it), so both sides agree byte for byte; `vectors/cpace.txt` pins the whole chain.
use blake2::digest::consts::U32;
use blake2::digest::{KeyInit, Mac};
use blake2::{Blake2b, Blake2bMac, Digest};

const P: [u64; 4] = [
    0xffff_ffff_ffff_ffed,
    0xffff_ffff_ffff_ffff,
    0xffff_ffff_ffff_ffff,
    0x7fff_ffff_ffff_ffff,
];
/// p - 2 and (p - 1) / 2.
const P_MINUS_2: [u64; 4] = [
    0xffff_ffff_ffff_ffeb,
    0xffff_ffff_ffff_ffff,
    0xffff_ffff_ffff_ffff,
    0x7fff_ffff_ffff_ffff,
];
const P_HALF: [u64; 4] = [
    0xffff_ffff_ffff_fff6,
    0xffff_ffff_ffff_ffff,
    0xffff_ffff_ffff_ffff,
    0x3fff_ffff_ffff_ffff,
];
/// The Montgomery curve constant A = 486662.
const A: u64 = 486_662;

/// A field element mod 2^255 - 19, as four little-endian limbs, kept below 2^256 (not
/// necessarily below p); `canon` reduces fully.
#[derive(Clone, Copy)]
struct Fe([u64; 4]);

impl Fe {
    #[cfg(test)]
    const ZERO: Fe = Fe([0; 4]);
    const ONE: Fe = Fe([1, 0, 0, 0]);

    fn small(v: u64) -> Fe {
        Fe([v, 0, 0, 0])
    }

    /// 32 bytes little endian with the top two bits cleared (a 254-bit value, below p).
    fn from_hidden(b: &[u8; 32]) -> Fe {
        let mut l = [0u64; 4];
        for (i, c) in b.chunks(8).enumerate() {
            l[i] = u64::from_le_bytes(c.try_into().unwrap());
        }
        l[3] &= 0x3fff_ffff_ffff_ffff;
        Fe(l)
    }

    fn add(self, o: Fe) -> Fe {
        let mut r = [0u64; 4];
        let mut c = 0u128;
        for (i, ri) in r.iter_mut().enumerate() {
            let cur = u128::from(self.0[i]) + u128::from(o.0[i]) + c;
            *ri = cur as u64;
            c = cur >> 64;
        }
        // 2^256 = 38 (mod p). The carry is 0 or 1, and after it the value is tiny.
        let mut c = c * 38;
        for ri in r.iter_mut() {
            let cur = u128::from(*ri) + c;
            *ri = cur as u64;
            c = cur >> 64;
        }
        r[0] = r[0].wrapping_add((c * 38) as u64);
        Fe(r)
    }

    /// The fully reduced value, in [0, p).
    fn canon(self) -> Fe {
        let mut v = self.0;
        for _ in 0..2 {
            let mut d = [0u64; 4];
            let mut borrow = 0u64;
            for i in 0..4 {
                let (x, b1) = v[i].overflowing_sub(P[i]);
                let (x, b2) = x.overflowing_sub(borrow);
                d[i] = x;
                borrow = u64::from(b1 | b2);
            }
            // borrow == 1: v < p, keep v; else take the difference.
            let keep = 0u64.wrapping_sub(borrow);
            for i in 0..4 {
                v[i] = (v[i] & keep) | (d[i] & !keep);
            }
        }
        Fe(v)
    }

    fn neg(self) -> Fe {
        let c = self.canon();
        let mut r = [0u64; 4];
        let mut borrow = 0u64;
        for i in 0..4 {
            let (x, b1) = P[i].overflowing_sub(c.0[i]);
            let (x, b2) = x.overflowing_sub(borrow);
            r[i] = x;
            borrow = u64::from(b1 | b2);
        }
        Fe(r)
    }

    fn sub(self, o: Fe) -> Fe {
        self.add(o.neg())
    }

    fn mul(self, o: Fe) -> Fe {
        let (a, b) = (self.0, o.0);
        let mut t = [0u64; 8];
        for i in 0..4 {
            let mut carry = 0u128;
            for j in 0..4 {
                let cur = u128::from(t[i + j]) + u128::from(a[i]) * u128::from(b[j]) + carry;
                t[i + j] = cur as u64;
                carry = cur >> 64;
            }
            t[i + 4] = carry as u64;
        }
        let mut r = [0u64; 5];
        let mut carry = 0u128;
        for i in 0..4 {
            let cur = u128::from(t[i]) + u128::from(t[i + 4]) * 38 + carry;
            r[i] = cur as u64;
            carry = cur >> 64;
        }
        r[4] = carry as u64;
        let mut c = u128::from(r[4]) * 38;
        for ri in r.iter_mut().take(4) {
            let cur = u128::from(*ri) + c;
            *ri = cur as u64;
            c = cur >> 64;
        }
        r[0] = r[0].wrapping_add((c * 38) as u64);
        Fe([r[0], r[1], r[2], r[3]])
    }

    fn sq(self) -> Fe {
        self.mul(self)
    }

    /// `self ^ e`, `e` a public constant.
    fn pow(self, e: &[u64; 4]) -> Fe {
        let mut r = Fe::ONE;
        for limb in e.iter().rev() {
            for bit in (0..64).rev() {
                r = r.sq();
                if (limb >> bit) & 1 == 1 {
                    r = r.mul(self);
                }
            }
        }
        r
    }

    fn invert(self) -> Fe {
        self.pow(&P_MINUS_2)
    }

    /// 1 when `self` is a non-zero square, else 0.
    fn is_square(self) -> u64 {
        u64::from(self.pow(&P_HALF).canon().0 == Fe::ONE.0)
    }

    fn select(self, other: Fe, take_other: u64) -> Fe {
        let m = 0u64.wrapping_sub(take_other);
        let mut r = [0u64; 4];
        for (i, ri) in r.iter_mut().enumerate() {
            *ri = (self.0[i] & !m) | (other.0[i] & m);
        }
        Fe(r)
    }

    fn to_bytes(self) -> [u8; 32] {
        let c = self.canon();
        let mut o = [0u8; 32];
        for (i, l) in c.0.iter().enumerate() {
            o[i * 8..i * 8 + 8].copy_from_slice(&l.to_le_bytes());
        }
        o
    }
}

/// Elligator2 for Curve25519 (non-square 2), the u coordinate only: the same function as
/// Monocypher's `crypto_elligator_map` and RFC 9380's `map_to_curve_elligator2_curve25519`.
/// The top two bits of `hidden` are ignored.
pub fn elligator_map(hidden: &[u8; 32]) -> [u8; 32] {
    let r = Fe::from_hidden(hidden);
    let a = Fe::small(A);
    // w = -A / (1 + 2 r^2)
    let t = r.sq().add(r.sq());
    let w = a.neg().mul(Fe::ONE.add(t).invert());
    // chi(w^3 + A w^2 + w): e = 1 -> u = w, else u = -w - A.
    let gw = w.mul(w.sq().add(a.mul(w)).add(Fe::ONE));
    let alt = w.neg().sub(a);
    w.select(alt, 1 - gw.is_square()).to_bytes()
}

/// The generator for this handshake and code.
pub fn generator(h: &[u8; 64], code: u32) -> [u8; 32] {
    let d: [u8; 32] = Blake2b::<U32>::new()
        .chain_update(b"AVA1 CPace")
        .chain_update(h)
        .chain_update(format!("{code:06}").as_bytes())
        .finalize()
        .into();
    elligator_map(&d)
}

/// `x · G`; None when the result is the all-zero point (a degenerate input).
pub fn public(x: &[u8; 32], g: &[u8; 32]) -> Option<[u8; 32]> {
    nonzero(x25519_dalek::x25519(*x, *g))
}

fn nonzero(v: [u8; 32]) -> Option<[u8; 32]> {
    (!ct_eq32(&v, &[0; 32])).then_some(v)
}

/// The session key from our scalar and the peer's public value, `ya`/`yb` being the
/// client's and the server's public values. None when the shared secret is all zero (a
/// low-order peer value).
pub fn key(
    h: &[u8; 64],
    x: &[u8; 32],
    y_peer: &[u8; 32],
    ya: &[u8; 32],
    yb: &[u8; 32],
) -> Option<[u8; 32]> {
    let mut shared = nonzero(x25519_dalek::x25519(*x, *y_peer))?;
    let k = Blake2b::<U32>::new()
        .chain_update(b"AVA1 CPace K")
        .chain_update(h)
        .chain_update(shared)
        .chain_update(ya)
        .chain_update(yb)
        .finalize()
        .into();
    zeroize::Zeroize::zeroize(&mut shared);
    Some(k)
}

/// The key confirmation of `role` (`b"client"` or `b"server"`).
pub fn mac(k: &[u8; 32], role: &[u8], h: &[u8; 64]) -> [u8; 32] {
    let mut m = <Blake2bMac<U32> as KeyInit>::new_from_slice(k).expect("32-byte key");
    Mac::update(&mut m, role);
    Mac::update(&mut m, h);
    m.finalize().into_bytes().into()
}

/// Constant-time equality.
pub fn ct_eq32(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A uniformly random six-digit code from the system CSPRNG (rejection sampling, no
/// modulo bias).
pub fn random_code() -> std::io::Result<u32> {
    const LIMIT: u32 = 4_294_000_000; // the largest multiple of 10^6 that fits
    loop {
        let v = u32::from_le_bytes(crate::keys::random_bytes::<4>()?);
        if v < LIMIT {
            return Ok(v % 1_000_000);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arr<const N: usize>(h: &str) -> [u8; N] {
        crate::hex::decode(h).unwrap().try_into().unwrap()
    }

    /// Prints fresh vector lines (`cargo test -p ava1 --lib print_cpace_vectors -- --ignored --nocapture`).
    #[test]
    #[ignore]
    fn print_cpace_vectors() {
        for (i, code) in [0u32, 123456, 999999, 1, 482913].into_iter().enumerate() {
            let mut h = [0u8; 64];
            for (j, b) in h.iter_mut().enumerate() {
                *b = (j as u8).wrapping_mul(7).wrapping_add(i as u8 * 31);
            }
            let (xa, xb) = ([0x11 + i as u8; 32], [0x71 + i as u8; 32]);
            let g = generator(&h, code);
            let (ya, yb) = (public(&xa, &g).unwrap(), public(&xb, &g).unwrap());
            let k = key(&h, &xa, &yb, &ya, &yb).unwrap();
            assert_eq!(k, key(&h, &xb, &ya, &ya, &yb).unwrap());
            let hx = |b: &[u8]| crate::hex::encode(b);
            println!(
                "cpace {} {code:06} {} {} {} {} {} {} {} {}",
                hx(&h),
                hx(&xa),
                hx(&xb),
                hx(&g),
                hx(&ya),
                hx(&yb),
                hx(&k),
                hx(&mac(&k, b"client", &h)),
                hx(&mac(&k, b"server", &h)),
            );
        }
    }

    #[test]
    fn the_vectors_reproduce() {
        let mut n = 0;
        for l in include_str!("../../../vectors/cpace.txt").lines() {
            if l.starts_with('#') || l.trim().is_empty() {
                continue;
            }
            let f: Vec<&str> = l.split_whitespace().collect();
            let h: [u8; 64] = arr(f[1]);
            let code: u32 = f[2].parse().unwrap();
            let (xa, xb): ([u8; 32], [u8; 32]) = (arr(f[3]), arr(f[4]));
            let g = generator(&h, code);
            assert_eq!(crate::hex::encode(&g), f[5], "G {l}");
            let (ya, yb) = (public(&xa, &g).unwrap(), public(&xb, &g).unwrap());
            assert_eq!(crate::hex::encode(&ya), f[6], "Ya {l}");
            assert_eq!(crate::hex::encode(&yb), f[7], "Yb {l}");
            let k = key(&h, &xa, &yb, &ya, &yb).unwrap();
            assert_eq!(crate::hex::encode(&k), f[8], "K {l}");
            assert_eq!(
                crate::hex::encode(&mac(&k, b"client", &h)),
                f[9],
                "client {l}"
            );
            assert_eq!(
                crate::hex::encode(&mac(&k, b"server", &h)),
                f[10],
                "server {l}"
            );
            n += 1;
        }
        assert!(n >= 5);
    }

    #[test]
    fn the_field_arithmetic_is_right() {
        // (p - 1)^2 = 1, 2^255 = 19, a * a^-1 = 1, and -(-a) = a.
        let pm1 = Fe(P).sub(Fe::ONE);
        assert_eq!(pm1.sq().canon().0, [1, 0, 0, 0]);
        let mut x = Fe::ONE;
        for _ in 0..255 {
            x = x.add(x);
        }
        assert_eq!(x.canon().0, [19, 0, 0, 0]);
        let a = Fe([
            0x1234_5678_9abc_def0,
            0x0fed_cba9_8765_4321,
            77,
            0x1234_0000_0000_0000,
        ]);
        assert_eq!(a.mul(a.invert()).canon().0, [1, 0, 0, 0]);
        assert_eq!(a.neg().neg().canon().0, a.canon().0);
        assert_eq!(Fe::ZERO.neg().canon().0, [0; 4]);
        // 2 is not a square mod p (p = 5 mod 8), -1 is.
        assert_eq!(Fe::small(2).is_square(), 0);
        assert_eq!(Fe::ONE.neg().is_square(), 1);
    }

    #[test]
    fn the_map_lands_on_the_curve() {
        for i in 1..64u8 {
            let mut hidden = [i.wrapping_mul(37); 32];
            hidden[0] = i;
            let mut ub = elligator_map(&hidden);
            ub[31] &= 0x7f; // canonical values already have bit 255 clear
            let mut l = [0u64; 4];
            for (i, c) in ub.chunks(8).enumerate() {
                l[i] = u64::from_le_bytes(c.try_into().unwrap());
            }
            let u = Fe(l);
            // v^2 = u^3 + A u^2 + u must have a root.
            let rhs = u.mul(u.sq().add(Fe::small(A).mul(u)).add(Fe::ONE));
            assert_eq!(rhs.is_square(), 1, "{i}");
        }
    }

    #[test]
    fn a_wrong_code_gives_a_different_generator_and_key() {
        let h = [7u8; 64];
        assert_ne!(generator(&h, 123456), generator(&h, 123457));
        assert_ne!(generator(&h, 123456), generator(&[8u8; 64], 123456));
        let (xa, xb) = ([3u8; 32], [4u8; 32]);
        let g = generator(&h, 123456);
        let (ya, yb) = (public(&xa, &g).unwrap(), public(&xb, &g).unwrap());
        let ka = key(&h, &xa, &yb, &ya, &yb).unwrap();
        let kb = key(&h, &xb, &ya, &ya, &yb).unwrap();
        assert_eq!(ka, kb, "the same code agrees");
        let g2 = generator(&h, 654321);
        let yb2 = public(&xb, &g2).unwrap();
        let kb2 = key(&h, &xb, &ya, &ya, &yb2).unwrap();
        assert_ne!(ka, kb2, "another code does not");
        assert_ne!(mac(&ka, b"client", &h), mac(&ka, b"server", &h));
    }

    #[test]
    fn a_low_order_peer_value_is_refused() {
        let h = [1u8; 64];
        let g = generator(&h, 1);
        let x = [5u8; 32];
        let y = public(&x, &g).unwrap();
        // The identity (u = 0) and a point of order 2 (u = 0, u = 1 are small-order points).
        for bad in [[0u8; 32], {
            let mut b = [0u8; 32];
            b[0] = 1;
            b
        }] {
            assert!(key(&h, &x, &bad, &y, &y).is_none());
        }
    }

    #[test]
    fn random_codes_are_six_digits() {
        for _ in 0..200 {
            assert!(random_code().unwrap() < 1_000_000);
        }
        // Not constant.
        let a: Vec<u32> = (0..20).map(|_| random_code().unwrap()).collect();
        assert!(a.iter().any(|c| *c != a[0]));
    }
}
