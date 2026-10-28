use crate::frame::HeaderError;
use crate::wire::{DecodeError, EncodeError};

#[derive(Debug, thiserror::Error)]
pub enum Ava1Error {
    #[error("network: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Header(#[from] HeaderError),
    #[error("malformed message: {0}")]
    Decode(#[from] DecodeError),
    #[error("message too large to send: {0}")]
    Encode(#[from] EncodeError),
    #[error("handshake failed: {0}")]
    Noise(String),
    #[error("a frame failed authentication")]
    BadTag,
    #[error("unexpected frame type {0:#04x}")]
    Unexpected(u8),
    #[error("refused by the other device ({code}): {message}")]
    Refused { code: u16, message: String },
    #[error("no protocol version in common (they speak {min}..={max}, we speak {ours})")]
    Version { min: u16, max: u16, ours: u16 },
    #[error("the devices are not paired yet")]
    NotPaired,
    #[error("a different device answered at this address (its key is not the expected one)")]
    WrongPeer,
    #[error("the pairing commitment does not match the reveal: the handshake is not with the device it appears to be")]
    PairingCommitMismatch,
    #[error("the other device sent no usable key")]
    WeakKey,
    #[error("timed out")]
    Timeout,
    #[error("connection closed")]
    Closed,
    #[error("connection lost: {0}")]
    Lost(String),
}

impl From<snow::Error> for Ava1Error {
    fn from(e: snow::Error) -> Self {
        Ava1Error::Noise(e.to_string())
    }
}
