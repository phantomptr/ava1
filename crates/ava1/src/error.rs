use crate::frame::HeaderError;
use crate::wire::{DecodeError, EncodeError};

/// Every error this crate's protocol operations return.
#[derive(Debug, thiserror::Error)]
pub enum Ava1Error {
    /// An I/O error: the socket, or a local file such as the peer store.
    #[error("network: {0}")]
    Io(#[from] std::io::Error),
    /// A frame header that is not valid (bad magic, bad CRC, a body over the limit), or a
    /// frame too large to send.
    #[error("{0}")]
    Header(#[from] HeaderError),
    /// A message body that does not decode (SPEC.md §3).
    #[error("malformed message: {0}")]
    Decode(#[from] DecodeError),
    /// A message that cannot be encoded: a field is over its length limit.
    #[error("message too large to send: {0}")]
    Encode(#[from] EncodeError),
    /// The Noise library failed during the handshake (SPEC.md §4.2), for example on a message
    /// that does not authenticate; carries its error text.
    #[error("handshake failed: {0}")]
    Noise(String),
    /// A sealed frame failed to open, a keyed connection received an unsealed frame, or a
    /// JoinAck's proof did not verify or named another lane (SPEC.md §4.4, §4.5).
    #[error("a frame failed authentication")]
    BadTag,
    /// A frame of another type than the one expected, or a sealed frame on a connection not
    /// yet keyed; carries the frame type.
    #[error("unexpected frame type {0:#04x}")]
    Unexpected(u8),
    /// A refusal with an AVA1 error or status code. Client side: the other device sent an
    /// `Error` frame or a non-OK RPC status, or a pairing check failed (`ERR_PAIRING_CODE`) or
    /// all lanes are open (`ERR_BUSY`). Server side: this node refused the client, which was sent
    /// the same code. Its text names no side, since either may have refused.
    #[error("refused ({code}): {message}")]
    Refused {
        /// The `ERR_*` code or RPC status (see [`gen`](crate::gen)).
        code: u16,
        /// The reason given with it.
        message: String,
    },
    /// No protocol version in common (SPEC.md §5 step 2).
    #[error("no protocol version in common (they speak {min}..={max}, we speak {ours})")]
    Version {
        /// The lowest version the other device speaks.
        min: u16,
        /// The highest version the other device speaks.
        max: u16,
        /// The version this side speaks (`PROTOCOL_VERSION`).
        ours: u16,
    },
    /// The devices are not paired: an RPC, lane or trusted confirmation was attempted before
    /// the pairing was accepted, or (server side) an unknown client met a closed pairing
    /// window (SPEC.md §5).
    #[error("the devices are not paired yet")]
    NotPaired,
    /// The server's static key is not the one the caller expected; found on message 2,
    /// before this side revealed its key or name (SPEC.md §5 step 3).
    #[error("a different device answered at this address (its key is not the expected one)")]
    WrongPeer,
    /// The Welcome's `nonce_s` does not hash to the `pair_commit` of message 2 (SPEC.md §4.6,
    /// §5 step 4).
    #[error("the pairing commitment does not match the reveal: the handshake is not with the device it appears to be")]
    PairingCommitMismatch,
    /// The other device sent a low-order ephemeral or static key, or no static key
    /// (SPEC.md §4.2).
    #[error("the other device sent no usable key")]
    WeakKey,
    /// A bounded operation ran out of time: connecting and the handshake, an RPC, a lane
    /// join, or a paced write.
    #[error("timed out")]
    Timeout,
    /// The other side closed the stream: end of stream mid-frame, or a write that moved no
    /// bytes.
    #[error("connection closed")]
    Closed,
    /// The connection broke or can no longer be used; carries why (an earlier write failed
    /// part-way, the nonce space is spent, a frame arrived too slowly, or the link ended).
    #[error("connection lost: {0}")]
    Lost(String),
}

impl From<snow::Error> for Ava1Error {
    fn from(e: snow::Error) -> Self {
        Ava1Error::Noise(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_does_not_blame_the_other_device() {
        // Also returned for this side's own refusals (a pairing code over 6 digits, all
        // lanes open) and, on a server, for the client it refused.
        let e = Ava1Error::Refused {
            code: crate::gen::ERR_BUSY,
            message: "all 8 lanes are open".into(),
        };
        assert_eq!(
            e.to_string(),
            format!("refused ({}): all 8 lanes are open", crate::gen::ERR_BUSY)
        );
    }
}
