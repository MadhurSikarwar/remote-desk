//! The encrypted channel between two devices.
//!
//! **Handshake.** Noise `XX`: each side sends a fresh ephemeral key and its
//! long-term key, the latter encrypted. When it completes, each side knows
//! the other's long-term public key and that the other holds the matching
//! private key, and both share keys that no eavesdropper can derive. The
//! session keys are not recoverable later even if a long-term key leaks.
//!
//! **What the handshake does not tell you** is whether the key at the other
//! end belongs to the device you meant to reach. That is settled outside:
//! by a key approved before ([`crate::TrustStore`]), or by two people
//! comparing the [`Established::auth_code`], which differs on the two sides
//! if someone sits in the middle.
//!
//! **Records.** After the handshake, data travels as records of
//! `u16 length | ciphertext`, each at most 65535 bytes and each
//! authenticated. Records must arrive in the order they were sent: a
//! changed, dropped, repeated or reordered record fails to decrypt, and the
//! channel is then dead.

use std::io::{Read, Write};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use snow::{HandshakeState, StatelessTransportState};

use crate::identity::{Identity, PublicKey};
use crate::CryptoError;

pub(crate) const NOISE_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";

/// Largest record Noise allows.
const MAX_RECORD: usize = 65_535;
/// Authentication tag appended to every record.
const TAG_LEN: usize = 16;
const MAX_PLAINTEXT: usize = MAX_RECORD - TAG_LEN;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The side that opened the connection (the viewer).
    Initiator,
    /// The side that accepted it (the host).
    Responder,
}

fn failure(reason: &str) -> CryptoError {
    CryptoError::Channel(reason.to_owned())
}

/// Writes one length-prefixed record.
pub fn write_record<W: Write>(writer: &mut W, record: &[u8]) -> Result<(), CryptoError> {
    let len = u16::try_from(record.len()).map_err(|_| failure("record too large"))?;
    let mut framed = Vec::with_capacity(2 + record.len());
    framed.extend_from_slice(&len.to_le_bytes());
    framed.extend_from_slice(record);
    writer.write_all(&framed)?;
    Ok(())
}

/// Reads one length-prefixed record.
pub fn read_record<R: Read>(reader: &mut R) -> Result<Vec<u8>, CryptoError> {
    let mut len = [0u8; 2];
    reader.read_exact(&mut len)?;
    let mut record = vec![0u8; usize::from(u16::from_le_bytes(len))];
    reader.read_exact(&mut record)?;
    Ok(record)
}

/// A handshake in progress.
pub struct Handshake {
    state: HandshakeState,
}

impl Handshake {
    pub fn new(identity: &Identity, side: Side) -> Result<Self, CryptoError> {
        let builder =
            snow::Builder::new(NOISE_PARAMS.parse()?).local_private_key(identity.private())?;
        let state = match side {
            Side::Initiator => builder.build_initiator()?,
            Side::Responder => builder.build_responder()?,
        };
        Ok(Self { state })
    }

    pub fn is_finished(&self) -> bool {
        self.state.is_handshake_finished()
    }

    /// True when the next step is ours to send.
    pub fn is_my_turn(&self) -> bool {
        self.state.is_my_turn()
    }

    /// The next handshake message to send.
    pub fn write_message(&mut self) -> Result<Vec<u8>, CryptoError> {
        let mut buffer = vec![0u8; MAX_RECORD];
        let len = self.state.write_message(&[], &mut buffer)?;
        buffer.truncate(len);
        Ok(buffer)
    }

    /// Processes a handshake message from the peer.
    pub fn read_message(&mut self, message: &[u8]) -> Result<(), CryptoError> {
        let mut payload = vec![0u8; MAX_RECORD];
        self.state.read_message(message, &mut payload)?;
        Ok(())
    }

    /// Turns a finished handshake into an encrypted channel.
    pub fn finish(self) -> Result<Established, CryptoError> {
        if !self.is_finished() {
            return Err(failure("handshake is not finished"));
        }
        let remote_public: PublicKey = self
            .state
            .get_remote_static()
            .and_then(|key| key.try_into().ok())
            .ok_or_else(|| failure("peer did not present a key"))?;
        let auth_code = auth_code(self.state.get_handshake_hash());
        let transport = Arc::new(self.state.into_stateless_transport_mode()?);
        Ok(Established {
            remote_public,
            auth_code,
            sealer: Sealer {
                transport: Arc::clone(&transport),
                nonce: 0,
            },
            opener: Opener {
                transport,
                nonce: 0,
                buffer: Vec::new(),
            },
        })
    }

    /// Runs the whole handshake over a connection.
    pub fn run<S: Read + Write>(
        identity: &Identity,
        side: Side,
        stream: &mut S,
    ) -> Result<Established, CryptoError> {
        let mut handshake = Self::new(identity, side)?;
        while !handshake.is_finished() {
            if handshake.is_my_turn() {
                write_record(stream, &handshake.write_message()?)?;
            } else {
                handshake.read_message(&read_record(stream)?)?;
            }
        }
        handshake.finish()
    }
}

/// Six digits that are the same on both sides of one handshake and, with
/// overwhelming probability, differ between two different handshakes. If
/// someone relays the connection through their own keys, the two people
/// see different codes.
fn auth_code(handshake_hash: &[u8]) -> String {
    let digest = Sha256::new()
        .chain_update(b"RemoteControl auth code v1")
        .chain_update(handshake_hash)
        .finalize();
    let number = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) % 1_000_000;
    format!("{:03} {:03}", number / 1000, number % 1000)
}

/// The result of a completed handshake.
pub struct Established {
    /// The peer's long-term public key, proven during the handshake.
    pub remote_public: PublicKey,
    /// Short code for two people to compare; see the module documentation.
    pub auth_code: String,
    pub sealer: Sealer,
    pub opener: Opener,
}

/// Encrypts outgoing data. One per connection; use from one thread at a
/// time.
pub struct Sealer {
    transport: Arc<StatelessTransportState>,
    nonce: u64,
}

impl Sealer {
    /// Encrypts `plaintext` into one or more records, ready to write to the
    /// connection as they are.
    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let records = plaintext.len().div_ceil(MAX_PLAINTEXT);
        let mut out = Vec::with_capacity(plaintext.len() + records * (2 + TAG_LEN));
        let mut record = vec![0u8; MAX_RECORD];
        for chunk in plaintext.chunks(MAX_PLAINTEXT) {
            let len = self
                .transport
                .write_message(self.nonce, chunk, &mut record)?;
            self.nonce = self
                .nonce
                .checked_add(1)
                .ok_or_else(|| failure("too many records on one connection"))?;
            out.extend_from_slice(&(len as u16).to_le_bytes());
            out.extend_from_slice(&record[..len]);
        }
        Ok(out)
    }
}

/// Decrypts incoming data that may arrive in arbitrary pieces.
pub struct Opener {
    transport: Arc<StatelessTransportState>,
    nonce: u64,
    buffer: Vec<u8>,
}

impl Opener {
    /// Adds bytes read from the connection.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// The plaintext of the next complete record, or `None` until more
    /// bytes arrive. After an error the channel must be dropped.
    pub fn next_plaintext(&mut self) -> Result<Option<Vec<u8>>, CryptoError> {
        let Some((prefix, rest)) = self.buffer.split_first_chunk::<2>() else {
            return Ok(None);
        };
        let len = usize::from(u16::from_le_bytes(*prefix));
        if len < TAG_LEN {
            return Err(failure("record too short"));
        }
        if rest.len() < len {
            return Ok(None);
        }
        let mut plaintext = vec![0u8; len];
        let written = self
            .transport
            .read_message(self.nonce, &rest[..len], &mut plaintext)?;
        plaintext.truncate(written);
        self.nonce = self
            .nonce
            .checked_add(1)
            .ok_or_else(|| failure("too many records on one connection"))?;
        self.buffer.drain(..2 + len);
        Ok(Some(plaintext))
    }
}
