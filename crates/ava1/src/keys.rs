//! Identities, the Noise XX handshake, and sealed frames (SPEC.md §4).

use std::io;
use std::path::Path;

use blake2::digest::consts::{U16, U32};
use blake2::digest::{KeyInit, Mac};
use blake2::{Blake2b, Blake2bMac, Digest};
use ring::aead::{Aad, LessSafeKey, Nonce, Tag, UnboundKey, CHACHA20_POLY1305};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroize;

use crate::Ava1Error;

/// The Noise protocol name (SPEC.md §4.2).
pub const NOISE: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2b";
/// The Noise prologue (SPEC.md §4.2).
pub const PROLOGUE: &[u8] = b"AVA1 v1";
/// Length of the Poly1305 MAC after every sealed body (SPEC.md §4.4).
pub const MAC_LEN: usize = 16;

/// A node's long-lived X25519 key pair (project 2 adds an Ed25519 key for tickets).
pub struct Identity {
    secret: [u8; 32],
    public: [u8; 32],
}

impl Drop for Identity {
    fn drop(&mut self) {
        self.secret.zeroize();
    }
}

impl Identity {
    /// The identity whose X25519 secret is `secret`; the public key is derived from it.
    /// `[u8; 32]` is `Copy`: this wipes only its own copy, and the caller's array stays as it
    /// was. A caller that holds the secret should keep it in `zeroize::Zeroizing` (or wipe it
    /// after this call).
    pub fn from_secret(mut secret: [u8; 32]) -> Self {
        let public = PublicKey::from(&StaticSecret::from(secret)).to_bytes();
        let id = Self { secret, public };
        secret.zeroize();
        id
    }

    /// A new identity from the OS's secure random source; fails if there is none.
    pub fn generate() -> io::Result<Self> {
        Ok(Self::from_secret(random_bytes::<32>()?))
    }

    /// The X25519 public key: this node as other devices know it.
    pub fn public(&self) -> [u8; 32] {
        self.public
    }

    /// Reads a 32-byte secret, creating it (mode 0600) when the file is missing. A file of
    /// the wrong size is an error: replacing it would silently unpair every device.
    /// Creation holds an advisory lock on `<path>.lock` and re-reads under it, so two
    /// processes starting together end up with the one identity the first of them made.
    pub fn load_or_create(path: &Path) -> io::Result<Self> {
        if let Some(id) = Self::read(path)? {
            return Ok(id);
        }
        let _lock = crate::fslock::lock(path)?;
        if let Some(id) = Self::read(path)? {
            return Ok(id);
        }
        let bytes = zeroize::Zeroizing::new(random_bytes::<32>()?);
        let tmp = crate::fslock::TmpGuard::new(crate::fslock::unique_tmp(path));
        write_private(tmp.path(), &bytes[..])?;
        std::fs::rename(tmp.path(), path)?;
        tmp.disarm();
        Ok(Self::from_secret(*bytes))
    }

    /// The identity in `path`; None when the file does not exist.
    fn read(path: &Path) -> io::Result<Option<Self>> {
        match std::fs::read(path) {
            Ok(b) => {
                let b = zeroize::Zeroizing::new(b);
                let a: zeroize::Zeroizing<[u8; 32]> =
                    zeroize::Zeroizing::new(b.as_slice().try_into().map_err(|_| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("{} holds {} bytes, expected 32", path.display(), b.len()),
                        )
                    })?);
                Ok(Some(Self::from_secret(*a)))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    // `create_new`: the name is unique to this call, and an existing file is never
    // truncated or adopted.
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// `N` bytes from the OS's secure random source; fails if there is none.
pub fn random_bytes<const N: usize>() -> io::Result<[u8; N]> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b)
        .map_err(|e| io::Error::other(format!("no secure random source: {e}")))?;
    Ok(b)
}

/// Keys of an established session. The client is always the Noise initiator.
#[derive(Clone)]
pub struct SessionKeys {
    /// Client → server (initiator → responder) key from Split() (SPEC.md §4.2); lane keys
    /// derive from it (§4.3).
    pub c2s: [u8; 32],
    /// Server → client key from Split().
    pub s2c: [u8; 32],
    /// Noise handshake hash: binds the session (the pairing PAKE, joins).
    pub hash: [u8; 64],
}

impl Drop for SessionKeys {
    fn drop(&mut self) {
        self.c2s.zeroize();
        self.s2c.zeroize();
        self.hash.zeroize();
    }
}

/// Whether `p` is a low-order X25519 point: one whose shared secret with every private
/// key is all zero, so a peer sending it would fix the "secret" in advance. X25519
/// clamps scalars to multiples of 8, which maps exactly these points (and no others) to
/// zero, so one multiplication by any scalar decides it. Same policy as the C side,
/// which refuses an all-zero DH output (SPEC.md §4.2).
pub fn is_low_order(p: &[u8; 32]) -> bool {
    const PROBE: [u8; 32] = [0x42; 32];
    x25519_dalek::x25519(PROBE, *p) == [0u8; 32]
}

/// One side of a `Noise_XX_25519_ChaChaPoly_BLAKE2b` handshake.
pub struct Handshake {
    hs: snow::HandshakeState,
    /// Handshake messages read so far: the first one read starts with the peer's
    /// ephemeral key in the clear.
    reads: u8,
}

impl Handshake {
    /// The initiator (client) side, with [`PROLOGUE`].
    pub fn initiator(me: &Identity) -> Result<Self, snow::Error> {
        Self::build(me, true, None, PROLOGUE)
    }

    /// The responder (server) side, with [`PROLOGUE`].
    pub fn responder(me: &Identity) -> Result<Self, snow::Error> {
        Self::build(me, false, None, PROLOGUE)
    }

    /// `eph` fixes the ephemeral key: for the published test vector only.
    #[doc(hidden)]
    pub fn build(
        me: &Identity,
        initiator: bool,
        eph: Option<&[u8; 32]>,
        prologue: &[u8],
    ) -> Result<Self, snow::Error> {
        let mut b = snow::Builder::new(NOISE.parse()?)
            .local_private_key(&me.secret)?
            .prologue(prologue)?;
        if let Some(e) = eph {
            b = b.fixed_ephemeral_key_for_testing_only(e);
        }
        let hs = if initiator {
            b.build_initiator()?
        } else {
            b.build_responder()?
        };
        Ok(Self { hs, reads: 0 })
    }

    /// Writes the next handshake message, carrying `payload`.
    pub fn write(&mut self, payload: &[u8]) -> Result<Vec<u8>, snow::Error> {
        let mut out = vec![0u8; payload.len() + 128];
        let n = self.hs.write_message(payload, &mut out)?;
        out.truncate(n);
        Ok(out)
    }

    /// Reads the peer's next handshake message. Refuses (`WeakKey`) a low-order
    /// ephemeral or static key: snow would carry on with an all-zero DH result.
    pub fn read(&mut self, msg: &[u8]) -> Result<Vec<u8>, Ava1Error> {
        if self.reads == 0 {
            // Message 1 (to the responder) and message 2 (to the initiator) both open
            // with the sender's ephemeral key.
            let e: Option<&[u8; 32]> = msg.get(..32).and_then(|b| b.try_into().ok());
            if e.is_some_and(is_low_order) {
                return Err(Ava1Error::WeakKey);
            }
        }
        self.reads += 1;
        let mut out = vec![0u8; msg.len()];
        let n = self.hs.read_message(msg, &mut out)?;
        out.truncate(n);
        if self.remote_static().is_some_and(|s| is_low_order(&s)) {
            out.zeroize();
            return Err(Ava1Error::WeakKey);
        }
        Ok(out)
    }

    /// The peer's static key, once a handshake message has revealed it (message 2 for the
    /// initiator, message 3 for the responder).
    pub fn remote_static(&self) -> Option<[u8; 32]> {
        self.hs.get_remote_static()?.try_into().ok()
    }

    /// After message 3: the split keys (initiator → responder first) and the handshake hash.
    pub fn finish(mut self) -> SessionKeys {
        let mut hash = [0u8; 64];
        hash.copy_from_slice(self.hs.get_handshake_hash());
        let (c2s, s2c) = self.hs.dangerously_get_raw_split();
        SessionKeys { c2s, s2c, hash }
    }
}

fn mac32(key: &[u8; 32], parts: &[&[u8]]) -> [u8; 32] {
    let mut m = <Blake2bMac<U32> as KeyInit>::new_from_slice(key).expect("32-byte key");
    for p in parts {
        Mac::update(&mut m, p);
    }
    let mut o = [0u8; 32];
    o.copy_from_slice(&m.finalize().into_bytes());
    o
}

fn mac16(key: &[u8; 32], parts: &[&[u8]]) -> [u8; 16] {
    let mut m = <Blake2bMac<U16> as KeyInit>::new_from_slice(key).expect("32-byte key");
    for p in parts {
        Mac::update(&mut m, p);
    }
    let mut o = [0u8; 16];
    o.copy_from_slice(&m.finalize().into_bytes());
    o
}

/// The key a connection uses in one direction (SPEC.md §4.3): keyed BLAKE2b-256(dir,
/// "AVA1 lane" ‖ u16le(lane) ‖ client_nonce ‖ server_nonce). Both nonces are fresh
/// random per lane join, so a re-join (or a replayed Join) of the same lane id never
/// reuses a key with its counters restarted at 0. The control connection (lane 0) is
/// keyed once per handshake; `control_key` feeds all-zero client/server nonces into this
/// *key derivation* (the Noise session already makes that key unique per handshake). That
/// is about the derivation inputs only: every sealed frame, control frames included, still
/// uses the AEAD nonce `0^4 ‖ u64le(counter)` and the counter advances on every frame.
pub fn lane_key(
    dir: &[u8; 32],
    lane: u16,
    client_nonce: &[u8; 16],
    server_nonce: &[u8; 16],
) -> [u8; 32] {
    mac32(
        dir,
        &[
            b"AVA1 lane",
            &lane.to_le_bytes(),
            client_nonce,
            server_nonce,
        ],
    )
}

/// The control connection's key in one direction: `lane_key(dir, 0, 0¹⁶, 0¹⁶)`.
pub fn control_key(dir: &[u8; 32]) -> [u8; 32] {
    lane_key(dir, 0, &[0; 16], &[0; 16])
}

fn join_mac(dir: &[u8; 32], parts: &[&[u8]]) -> [u8; 16] {
    let mut jk = mac32(dir, &[b"AVA1 join"]);
    let t = mac16(&jk, parts);
    jk.zeroize();
    t
}

/// A Join's proof (SPEC.md §4.5): keyed BLAKE2b-128(BLAKE2b-256(c2s, "AVA1 join"),
/// "join" ‖ sid ‖ u16le(lane) ‖ client_nonce).
pub fn join_tag(c2s: &[u8; 32], sid: &[u8; 16], lane: u16, client_nonce: &[u8; 16]) -> [u8; 16] {
    join_mac(c2s, &[b"join", sid, &lane.to_le_bytes(), client_nonce])
}

/// A JoinAck's proof: keyed BLAKE2b-128(BLAKE2b-256(s2c, "AVA1 join"),
/// "join-ack" ‖ sid ‖ u16le(lane) ‖ client_nonce ‖ server_nonce).
pub fn join_ack_tag(
    s2c: &[u8; 32],
    sid: &[u8; 16],
    lane: u16,
    client_nonce: &[u8; 16],
    server_nonce: &[u8; 16],
) -> [u8; 16] {
    join_mac(
        s2c,
        &[
            b"join-ack",
            sid,
            &lane.to_le_bytes(),
            client_nonce,
            server_nonce,
        ],
    )
}

/// The server's commitment to its pairing nonce, sent in `ServerInfo` before the client
/// has revealed anything (SPEC.md §4.6).
pub fn pair_commit(nonce_s: &[u8; 16]) -> [u8; 32] {
    Blake2b::<U32>::new()
        .chain_update(nonce_s)
        .finalize()
        .into()
}

fn nonce(n: u64) -> [u8; 12] {
    let mut b = [0u8; 12];
    b[4..].copy_from_slice(&n.to_le_bytes());
    b
}

/// ChaCha20-Poly1305 in place; appends the 16-byte MAC.
pub fn seal(key: &[u8; 32], n: u64, ad: &[u8], buf: &mut Vec<u8>) {
    seal_nonce(key, &nonce(n), ad, buf)
}

/// `seal` for a buffer the caller already laid out: encrypts `buf` in place and returns the
/// MAC, so a frame can be assembled (header, body) in one allocation and sealed where it
/// lies, with no intermediate copy.
pub fn seal_slice(key: &[u8; 32], n: u64, ad: &[u8], buf: &mut [u8]) -> [u8; MAC_LEN] {
    let tag = aead_key(key)
        .seal_in_place_separate_tag(Nonce::assume_unique_for_key(nonce(n)), Aad::from(ad), buf)
        .expect("frames are far below ChaCha20's length limit");
    tag.as_ref().try_into().expect("a 16-byte MAC")
}

/// Verifies and decrypts in place, stripping the MAC. `false` = forged, reordered or damaged.
pub fn open(key: &[u8; 32], n: u64, ad: &[u8], buf: &mut Vec<u8>) -> bool {
    open_nonce(key, &nonce(n), ad, buf)
}

/// RFC 8439 AEAD with an explicit 96-bit nonce (`seal` uses 0⁴ ‖ u64le(n)). For the
/// published test vectors.
#[doc(hidden)]
pub fn seal_nonce(key: &[u8; 32], nonce: &[u8; 12], ad: &[u8], buf: &mut Vec<u8>) {
    let tag = aead_key(key)
        .seal_in_place_separate_tag(Nonce::assume_unique_for_key(*nonce), Aad::from(ad), buf)
        .expect("frames are far below ChaCha20's length limit");
    buf.extend_from_slice(tag.as_ref());
}

#[doc(hidden)]
pub fn open_nonce(key: &[u8; 32], nonce: &[u8; 12], ad: &[u8], buf: &mut Vec<u8>) -> bool {
    if buf.len() < MAC_LEN {
        return false;
    }
    let at = buf.len() - MAC_LEN;
    let tag: [u8; MAC_LEN] = buf[at..].try_into().expect("16 bytes");
    buf.truncate(at);
    // ring compares the tag in constant time and zeroes `buf` when it does not match,
    // so unauthenticated plaintext never reaches a caller.
    aead_key(key)
        .open_in_place_separate_tag(
            Nonce::assume_unique_for_key(*nonce),
            Aad::from(ad),
            Tag::from(tag),
            buf,
            0..,
        )
        .is_ok()
}

/// ring's key schedule is a copy of the 32 bytes, so a key per frame costs nothing.
fn aead_key(key: &[u8; 32]) -> LessSafeKey {
    LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, key).expect("32-byte key"))
}

/// Whether two 16-byte values are equal, compared in constant time.
pub fn ct_eq16(a: &[u8; 16], b: &[u8; 16]) -> bool {
    a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hex;

    fn v() -> serde_json::Value {
        serde_json::from_str(include_str!("../../../vectors/noise_xx.json")).unwrap()
    }
    fn b(v: &serde_json::Value, k: &str) -> Vec<u8> {
        hex::decode(v[k].as_str().unwrap()).unwrap()
    }
    fn k32(x: Vec<u8>) -> [u8; 32] {
        x.try_into().unwrap()
    }

    #[test]
    fn the_published_noise_vector_reproduces() {
        let v = v();
        let pro = b(&v, "init_prologue");
        let (is, ie) = (
            Identity::from_secret(k32(b(&v, "init_static"))),
            k32(b(&v, "init_ephemeral")),
        );
        let (rs, re) = (
            Identity::from_secret(k32(b(&v, "resp_static"))),
            k32(b(&v, "resp_ephemeral")),
        );
        let mut i = Handshake::build(&is, true, Some(&ie), &pro).unwrap();
        let mut r = Handshake::build(&rs, false, Some(&re), &pro).unwrap();
        let msgs = v["messages"].as_array().unwrap();
        for (n, m) in msgs.iter().take(3).enumerate() {
            let payload = hex::decode(m["payload"].as_str().unwrap()).unwrap();
            let (w, rd) = if n % 2 == 0 {
                (&mut i, &mut r)
            } else {
                (&mut r, &mut i)
            };
            let msg = w.write(&payload).unwrap();
            assert_eq!(
                hex::encode(&msg),
                m["ciphertext"].as_str().unwrap(),
                "message {n}"
            );
            assert_eq!(rd.read(&msg).unwrap(), payload);
        }
        assert_eq!(i.remote_static(), Some(rs.public()));
        assert_eq!(r.remote_static(), Some(is.public()));
        let (ki, kr) = (i.finish(), r.finish());
        assert_eq!(hex::encode(&ki.hash), v["handshake_hash"].as_str().unwrap());
        assert_eq!((ki.c2s, ki.s2c), (kr.c2s, kr.s2c));
        // Transport messages 3..5 alternate responder, initiator, responder, each direction from n = 0.
        let (mut n_i, mut n_r) = (0u64, 0u64);
        for (n, m) in msgs.iter().enumerate().skip(3) {
            let mut buf = hex::decode(m["payload"].as_str().unwrap()).unwrap();
            let from_initiator = n % 2 == 0;
            let (key, ctr) = if from_initiator {
                (&ki.c2s, &mut n_i)
            } else {
                (&ki.s2c, &mut n_r)
            };
            seal(key, *ctr, &[], &mut buf);
            *ctr += 1;
            assert_eq!(
                hex::encode(&buf),
                m["ciphertext"].as_str().unwrap(),
                "transport {n}"
            );
        }
    }

    #[test]
    fn a_fresh_handshake_agrees_on_keys_and_peers() {
        let (c, s) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let (mut i, mut r) = (
            Handshake::initiator(&c).unwrap(),
            Handshake::responder(&s).unwrap(),
        );
        r.read(&i.write(b"hello").unwrap()).unwrap();
        assert_eq!(i.read(&r.write(b"server").unwrap()).unwrap(), b"server");
        assert_eq!(r.read(&i.write(b"client").unwrap()).unwrap(), b"client");
        assert_eq!(i.remote_static(), Some(s.public()));
        assert_eq!(r.remote_static(), Some(c.public()));
        let (a, b) = (i.finish(), r.finish());
        assert_eq!((a.c2s, a.s2c, a.hash), (b.c2s, b.s2c, b.hash));
    }

    fn arr<const N: usize>(h: &str) -> [u8; N] {
        hex::decode(h).unwrap().try_into().unwrap()
    }

    #[test]
    fn the_pairing_commitment_reproduces_the_vectors() {
        let mut n = 0;
        for l in include_str!("../../../vectors/pairing.txt").lines() {
            if l.starts_with('#') || l.trim().is_empty() {
                continue;
            }
            let f: Vec<&str> = l.split_whitespace().collect();
            let ns: [u8; 16] = arr(f[1]);
            assert_eq!(hex::encode(&pair_commit(&ns)), f[2], "{l}");
            n += 1;
        }
        assert!(n >= 3);
    }

    #[test]
    fn a_tampered_handshake_message_is_refused() {
        let (c, s) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let (mut i, mut r) = (
            Handshake::initiator(&c).unwrap(),
            Handshake::responder(&s).unwrap(),
        );
        r.read(&i.write(&[]).unwrap()).unwrap();
        let mut m2 = r.write(&[]).unwrap();
        m2[40] ^= 1; // inside the encrypted static key
        assert!(i.read(&m2).is_err());
    }

    #[test]
    fn low_order_keys_are_refused() {
        // The canonical small-order points (RFC 7748 §6.1 / Curve25519 "contributory" list).
        let low: [&str; 5] = [
            "0000000000000000000000000000000000000000000000000000000000000000",
            "0100000000000000000000000000000000000000000000000000000000000000",
            "e0eb7a7c3b41b8ae1656e3faf19fc46ada098deb9c32b1fd866205165f49b800",
            "5f9c95bca3508c24b1d0b1559c83ef5b04445cc4581c8e86d8224eddd09f1157",
            "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
        ];
        for h in low {
            let p: [u8; 32] = hex::decode(h).unwrap().try_into().unwrap();
            assert!(is_low_order(&p), "{h}");
            // As the peer's ephemeral key in message 1.
            let s = Identity::generate().unwrap();
            let mut r = Handshake::responder(&s).unwrap();
            let mut m1 = p.to_vec();
            m1.extend_from_slice(b"hello");
            assert!(matches!(r.read(&m1), Err(Ava1Error::WeakKey)), "{h}");
        }
        assert!(!is_low_order(&Identity::generate().unwrap().public()));
        assert!(!is_low_order(&[9u8; 32]));
    }

    #[test]
    fn seal_slice_matches_seal_without_the_copy() {
        let k = [8u8; 32];
        let mut a = vec![0x5a; 4099];
        let mut b = a.clone();
        seal(&k, 9, b"hdr", &mut a);
        let tag = seal_slice(&k, 9, b"hdr", &mut b);
        b.extend_from_slice(&tag);
        assert_eq!(a, b);
    }

    #[test]
    fn sealing_binds_key_counter_and_header() {
        let k = [3u8; 32];
        let mut a = b"frame body".to_vec();
        seal(&k, 5, b"hdr", &mut a);
        assert_eq!(a.len(), 10 + MAC_LEN);
        for (key, n, ad) in [
            ([4u8; 32], 5u64, &b"hdr"[..]),
            (k, 6, b"hdr"),
            (k, 5, b"hdx"),
        ] {
            let mut x = a.clone();
            assert!(!open(&key, n, ad, &mut x));
        }
        let mut ok = a.clone();
        assert!(open(&k, 5, b"hdr", &mut ok));
        assert_eq!(ok, b"frame body");
        let mut short = vec![0u8; 3];
        assert!(!open(&k, 0, b"", &mut short));
    }

    #[test]
    fn derived_keys_differ_by_lane_label_and_nonce() {
        let d = [9u8; 32];
        let (cn, sn) = ([1u8; 16], [2u8; 16]);
        assert_ne!(lane_key(&d, 0, &cn, &sn), lane_key(&d, 1, &cn, &sn));
        assert_eq!(control_key(&d), lane_key(&d, 0, &[0; 16], &[0; 16]));
        // Two joins of the same lane id never share a key: either nonce changes it.
        assert_ne!(lane_key(&d, 1, &cn, &sn), lane_key(&d, 1, &cn, &[3; 16]));
        assert_ne!(lane_key(&d, 1, &cn, &sn), lane_key(&d, 1, &[3; 16], &sn));
        let sid = [1u8; 16];
        assert_ne!(
            join_tag(&d, &sid, 1, &cn),
            join_ack_tag(&d, &sid, 1, &cn, &sn)
        );
        assert_ne!(
            join_ack_tag(&d, &sid, 1, &cn, &sn),
            join_ack_tag(&d, &sid, 1, &cn, &[3; 16])
        );
        assert!(ct_eq16(&[7; 16], &[7; 16]) && !ct_eq16(&[7; 16], &[8; 16]));
    }

    #[test]
    fn the_key_vectors_reproduce() {
        let mut n = 0;
        for l in include_str!("../../../vectors/keys.txt").lines() {
            if l.starts_with('#') || l.trim().is_empty() {
                continue;
            }
            let f: Vec<&str> = l.split_whitespace().collect();
            let h16 = |s: &str| -> [u8; 16] { hex::decode(s).unwrap().try_into().unwrap() };
            let h32 = |s: &str| -> [u8; 32] { hex::decode(s).unwrap().try_into().unwrap() };
            let lane: u16 = f[2].parse().unwrap();
            let got = match f[0] {
                "lane_key" => hex::encode(&lane_key(&h32(f[1]), lane, &h16(f[3]), &h16(f[4]))),
                "join_tag" => hex::encode(&join_tag(&h32(f[1]), &h16(f[5]), lane, &h16(f[3]))),
                "join_ack_tag" => hex::encode(&join_ack_tag(
                    &h32(f[1]),
                    &h16(f[5]),
                    lane,
                    &h16(f[3]),
                    &h16(f[4]),
                )),
                k => panic!("unknown vector kind {k}"),
            };
            assert_eq!(got, *f.last().unwrap(), "{l}");
            n += 1;
        }
        assert!(n >= 5);
    }

    #[test]
    fn identity_persists_and_a_bad_file_is_an_error() {
        let dir = std::env::temp_dir().join(format!("ava1-id-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let p = dir.join("ava").join("identity");
        let a = Identity::load_or_create(&p).unwrap();
        assert_eq!(Identity::load_or_create(&p).unwrap().public(), a.public());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::write(&p, b"short").unwrap();
        assert_eq!(
            Identity::load_or_create(&p).err().unwrap().kind(),
            std::io::ErrorKind::InvalidData
        );
        assert_eq!(
            std::fs::read(&p).unwrap(),
            b"short",
            "a bad identity is never replaced"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
