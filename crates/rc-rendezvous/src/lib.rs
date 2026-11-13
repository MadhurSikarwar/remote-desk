//! What devices and a rendezvous server say to each other.
//!
//! **Transport.** Each message is the plaintext of one record of an
//! encrypted channel (`rc-crypto`), set up with a Noise handshake in which
//! the device proves its key and checks the server's key against the
//! fingerprint it was configured with. The format here carries no length
//! prefix of its own: the record is the frame.
//!
//! **Relay.** To forward a session, a device opens a *new* connection to the
//! server and sends the fixed [`RELAY_MAGIC`] followed by a 32-byte token
//! that the server gave to both parties over their encrypted channels. The
//! server then joins the two connections and copies bytes between them. The
//! devices run their own end-to-end handshake through it, so the server
//! carries ciphertext only.
//!
//! A device is identified by the first 8 bytes of the SHA-256 of its public
//! key (see `rc_crypto::fingerprint_bytes`). The server cannot choose or
//! reassign an ID, and a viewer checks, after the end-to-end handshake, that
//! the key it reached has the ID it asked for.

#![forbid(unsafe_code)]

use std::net::Ipv4Addr;

/// Bumped on any incompatible change.
pub const RENDEZVOUS_VERSION: u16 = 1;

/// First bytes of a relay connection. Chosen so that it can never be the
/// start of an encrypted-channel record (whose first two bytes are a record
/// length, never `0x4352`), which lets one port serve both.
pub const RELAY_MAGIC: &[u8; 8] = b"RCRELAY1";
pub const TOKEN_LEN: usize = 32;
pub type Token = [u8; TOKEN_LEN];
pub type DeviceId = [u8; 8];

/// Most LAN addresses a device may announce.
pub const MAX_LAN_ADDRESSES: usize = 8;
const MAX_TEXT_BYTES: usize = 64;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MessageError {
    #[error("empty message")]
    Empty,
    #[error("unknown message type {0}")]
    UnknownType(u8),
    #[error("malformed message: {0}")]
    Malformed(&'static str),
}

/// From a device to the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientMessage {
    /// A device that is sharing says it can be reached.
    Register {
        version: u16,
        /// TCP port its listener is on, for direct connections.
        port: u16,
        /// Addresses on its own network, for viewers on the same network.
        lan: Vec<Ipv4Addr>,
        /// The name it goes by. Shown, never trusted.
        name: String,
    },
    /// A viewer asks to reach a device.
    Connect {
        target: DeviceId,
        name: String,
    },
    /// The viewer could not connect directly and wants the server to relay.
    UseRelay {
        request_id: u64,
    },
    Ping,
}

/// From the server to a device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerMessage {
    /// Registration accepted. `address` is where the server sees the device
    /// coming from.
    Registered {
        address: Ipv4Addr,
    },
    /// To a sharing device: someone asked for it.
    Incoming {
        request_id: u64,
        viewer: DeviceId,
        name: String,
    },
    /// To the viewer: where to try a direct connection.
    Peer {
        request_id: u64,
        /// The host's address as the server sees it.
        address: Ipv4Addr,
        port: u16,
        lan: Vec<Ipv4Addr>,
        /// The viewer and the host appear from the same address, so they
        /// are probably on one network and `lan` is worth trying.
        same_network: bool,
    },
    /// No device with that ID is registered.
    NotFound {
        target: DeviceId,
    },
    /// To the viewer: the token for the relay connection.
    RelayToken {
        request_id: u64,
        token: Token,
    },
    /// To the host: open a relay connection with this token.
    RelayRequest {
        request_id: u64,
        token: Token,
    },
    Pong,
    /// A request was refused or a limit was reached.
    Error {
        reason: String,
    },
    /// Another connection registered the same device; this one is dropped.
    Replaced,
}

mod tag {
    pub const REGISTER: u8 = 1;
    pub const CONNECT: u8 = 2;
    pub const USE_RELAY: u8 = 3;
    pub const PING: u8 = 4;

    pub const REGISTERED: u8 = 1;
    pub const INCOMING: u8 = 2;
    pub const PEER: u8 = 3;
    pub const NOT_FOUND: u8 = 4;
    pub const RELAY_TOKEN: u8 = 5;
    pub const RELAY_REQUEST: u8 = 6;
    pub const PONG: u8 = 7;
    pub const ERROR: u8 = 8;
    pub const REPLACED: u8 = 9;
}

struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], MessageError> {
        if self.rest.len() < n {
            return Err(MessageError::Malformed("truncated"));
        }
        let (head, tail) = self.rest.split_at(n);
        self.rest = tail;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], MessageError> {
        let mut out = [0; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, MessageError> {
        Ok(self.array::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, MessageError> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64, MessageError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    fn ip(&mut self) -> Result<Ipv4Addr, MessageError> {
        Ok(Ipv4Addr::from(self.array::<4>()?))
    }

    fn ips(&mut self) -> Result<Vec<Ipv4Addr>, MessageError> {
        let count = usize::from(self.u8()?);
        if count > MAX_LAN_ADDRESSES {
            return Err(MessageError::Malformed("too many addresses"));
        }
        (0..count).map(|_| self.ip()).collect()
    }

    fn flag(&mut self) -> Result<bool, MessageError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(MessageError::Malformed("bad flag")),
        }
    }

    fn text(&mut self) -> Result<String, MessageError> {
        let len = usize::from(self.u8()?);
        if len > MAX_TEXT_BYTES {
            return Err(MessageError::Malformed("text too long"));
        }
        let text = String::from_utf8(self.take(len)?.to_vec())
            .map_err(|_| MessageError::Malformed("text is not UTF-8"))?;
        if text.chars().any(char::is_control) {
            return Err(MessageError::Malformed("control characters in text"));
        }
        Ok(text)
    }

    fn finish(self) -> Result<(), MessageError> {
        if self.rest.is_empty() {
            Ok(())
        } else {
            Err(MessageError::Malformed("trailing bytes"))
        }
    }
}

/// Printable text cut to the size limit on a character boundary.
fn put_text(out: &mut Vec<u8>, text: &str) {
    let mut cleaned = String::new();
    for c in text.chars().filter(|c| !c.is_control()) {
        if cleaned.len() + c.len_utf8() > MAX_TEXT_BYTES {
            break;
        }
        cleaned.push(c);
    }
    out.push(cleaned.len() as u8);
    out.extend_from_slice(cleaned.as_bytes());
}

fn put_ips(out: &mut Vec<u8>, ips: &[Ipv4Addr]) {
    let ips = &ips[..ips.len().min(MAX_LAN_ADDRESSES)];
    out.push(ips.len() as u8);
    for ip in ips {
        out.extend_from_slice(&ip.octets());
    }
}

impl ClientMessage {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Self::Register {
                version,
                port,
                lan,
                name,
            } => {
                out.push(tag::REGISTER);
                out.extend_from_slice(&version.to_le_bytes());
                out.extend_from_slice(&port.to_le_bytes());
                put_ips(&mut out, lan);
                put_text(&mut out, name);
            }
            Self::Connect { target, name } => {
                out.push(tag::CONNECT);
                out.extend_from_slice(target);
                put_text(&mut out, name);
            }
            Self::UseRelay { request_id } => {
                out.push(tag::USE_RELAY);
                out.extend_from_slice(&request_id.to_le_bytes());
            }
            Self::Ping => out.push(tag::PING),
        }
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, MessageError> {
        let mut r = Reader { rest: bytes };
        let message = match r.u8().map_err(|_| MessageError::Empty)? {
            tag::REGISTER => Self::Register {
                version: r.u16()?,
                port: r.u16()?,
                lan: r.ips()?,
                name: r.text()?,
            },
            tag::CONNECT => Self::Connect {
                target: r.array()?,
                name: r.text()?,
            },
            tag::USE_RELAY => Self::UseRelay {
                request_id: r.u64()?,
            },
            tag::PING => Self::Ping,
            other => return Err(MessageError::UnknownType(other)),
        };
        r.finish()?;
        Ok(message)
    }
}

impl ServerMessage {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Self::Registered { address } => {
                out.push(tag::REGISTERED);
                out.extend_from_slice(&address.octets());
            }
            Self::Incoming {
                request_id,
                viewer,
                name,
            } => {
                out.push(tag::INCOMING);
                out.extend_from_slice(&request_id.to_le_bytes());
                out.extend_from_slice(viewer);
                put_text(&mut out, name);
            }
            Self::Peer {
                request_id,
                address,
                port,
                lan,
                same_network,
            } => {
                out.push(tag::PEER);
                out.extend_from_slice(&request_id.to_le_bytes());
                out.extend_from_slice(&address.octets());
                out.extend_from_slice(&port.to_le_bytes());
                put_ips(&mut out, lan);
                out.push(u8::from(*same_network));
            }
            Self::NotFound { target } => {
                out.push(tag::NOT_FOUND);
                out.extend_from_slice(target);
            }
            Self::RelayToken { request_id, token } => {
                out.push(tag::RELAY_TOKEN);
                out.extend_from_slice(&request_id.to_le_bytes());
                out.extend_from_slice(token);
            }
            Self::RelayRequest { request_id, token } => {
                out.push(tag::RELAY_REQUEST);
                out.extend_from_slice(&request_id.to_le_bytes());
                out.extend_from_slice(token);
            }
            Self::Pong => out.push(tag::PONG),
            Self::Error { reason } => {
                out.push(tag::ERROR);
                put_text(&mut out, reason);
            }
            Self::Replaced => out.push(tag::REPLACED),
        }
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, MessageError> {
        let mut r = Reader { rest: bytes };
        let message = match r.u8().map_err(|_| MessageError::Empty)? {
            tag::REGISTERED => Self::Registered { address: r.ip()? },
            tag::INCOMING => Self::Incoming {
                request_id: r.u64()?,
                viewer: r.array()?,
                name: r.text()?,
            },
            tag::PEER => Self::Peer {
                request_id: r.u64()?,
                address: r.ip()?,
                port: r.u16()?,
                lan: r.ips()?,
                same_network: r.flag()?,
            },
            tag::NOT_FOUND => Self::NotFound { target: r.array()? },
            tag::RELAY_TOKEN => Self::RelayToken {
                request_id: r.u64()?,
                token: r.array()?,
            },
            tag::RELAY_REQUEST => Self::RelayRequest {
                request_id: r.u64()?,
                token: r.array()?,
            },
            tag::PONG => Self::Pong,
            tag::ERROR => Self::Error { reason: r.text()? },
            tag::REPLACED => Self::Replaced,
            other => return Err(MessageError::UnknownType(other)),
        };
        r.finish()?;
        Ok(message)
    }
}

/// The bytes that open a relay connection.
pub fn relay_hello(token: &Token) -> Vec<u8> {
    let mut out = RELAY_MAGIC.to_vec();
    out.extend_from_slice(token);
    out
}

/// Total length of [`relay_hello`].
pub const RELAY_HELLO_LEN: usize = RELAY_MAGIC.len() + TOKEN_LEN;

#[cfg(test)]
mod tests {
    use super::*;

    fn client_samples() -> Vec<ClientMessage> {
        vec![
            ClientMessage::Register {
                version: RENDEZVOUS_VERSION,
                port: 51_000,
                lan: vec![Ipv4Addr::new(192, 168, 1, 7), Ipv4Addr::new(10, 0, 0, 2)],
                name: "Workshop PC".into(),
            },
            ClientMessage::Register {
                version: 9,
                port: 1,
                lan: vec![],
                name: String::new(),
            },
            ClientMessage::Connect {
                target: [1, 2, 3, 4, 5, 6, 7, 8],
                name: "Laptop é".into(),
            },
            ClientMessage::UseRelay {
                request_id: u64::MAX,
            },
            ClientMessage::Ping,
        ]
    }

    fn server_samples() -> Vec<ServerMessage> {
        vec![
            ServerMessage::Registered {
                address: Ipv4Addr::new(203, 0, 113, 9),
            },
            ServerMessage::Incoming {
                request_id: 7,
                viewer: [9; 8],
                name: "Laptop".into(),
            },
            ServerMessage::Peer {
                request_id: 8,
                address: Ipv4Addr::new(198, 51, 100, 4),
                port: 40_000,
                lan: vec![Ipv4Addr::new(192, 168, 0, 5)],
                same_network: true,
            },
            ServerMessage::NotFound { target: [3; 8] },
            ServerMessage::RelayToken {
                request_id: 1,
                token: [0xAA; TOKEN_LEN],
            },
            ServerMessage::RelayRequest {
                request_id: 2,
                token: [0x55; TOKEN_LEN],
            },
            ServerMessage::Pong,
            ServerMessage::Error {
                reason: "too many requests".into(),
            },
            ServerMessage::Replaced,
        ]
    }

    #[test]
    fn every_message_round_trips() {
        for message in client_samples() {
            assert_eq!(ClientMessage::decode(&message.encode()), Ok(message));
        }
        for message in server_samples() {
            assert_eq!(ServerMessage::decode(&message.encode()), Ok(message));
        }
    }

    #[test]
    fn every_truncation_and_trailing_byte_is_rejected() {
        for message in client_samples() {
            let bytes = message.encode();
            for cut in 0..bytes.len() {
                assert!(
                    ClientMessage::decode(&bytes[..cut]).is_err(),
                    "{message:?} at {cut}"
                );
            }
            let mut longer = bytes;
            longer.push(0);
            assert!(ClientMessage::decode(&longer).is_err(), "{message:?}");
        }
        for message in server_samples() {
            let bytes = message.encode();
            for cut in 0..bytes.len() {
                assert!(
                    ServerMessage::decode(&bytes[..cut]).is_err(),
                    "{message:?} at {cut}"
                );
            }
            let mut longer = bytes;
            longer.push(0);
            assert!(ServerMessage::decode(&longer).is_err(), "{message:?}");
        }
    }

    #[test]
    fn unknown_types_and_empty_input_are_rejected() {
        assert_eq!(ClientMessage::decode(&[]), Err(MessageError::Empty));
        assert_eq!(ServerMessage::decode(&[]), Err(MessageError::Empty));
        assert_eq!(
            ClientMessage::decode(&[200]),
            Err(MessageError::UnknownType(200))
        );
        assert_eq!(
            ServerMessage::decode(&[0]),
            Err(MessageError::UnknownType(0))
        );
    }

    #[test]
    fn too_many_addresses_bad_flags_and_bad_text_are_rejected() {
        // Register announcing 9 addresses.
        let mut many = vec![tag::REGISTER, 1, 0, 0x10, 0x27, 9];
        many.extend(std::iter::repeat_n(0, 9 * 4));
        many.push(0);
        assert!(ClientMessage::decode(&many).is_err());

        // Peer with a flag of 2.
        let mut peer = ServerMessage::Peer {
            request_id: 0,
            address: Ipv4Addr::LOCALHOST,
            port: 1,
            lan: vec![],
            same_network: false,
        }
        .encode();
        *peer.last_mut().unwrap() = 2;
        assert!(ServerMessage::decode(&peer).is_err());

        // Text: too long, invalid UTF-8, control characters.
        let mut long = vec![tag::ERROR, 65];
        long.extend(std::iter::repeat_n(b'a', 65));
        assert!(ServerMessage::decode(&long).is_err());
        assert!(ServerMessage::decode(&[tag::ERROR, 1, 0xFF]).is_err());
        assert!(ServerMessage::decode(&[tag::ERROR, 1, 0x07]).is_err());
    }

    #[test]
    fn text_is_cleaned_and_cut_on_a_character_boundary_when_sent() {
        let message = ServerMessage::Error {
            reason: format!("ok\n\t{}", "é".repeat(100)),
        };
        match ServerMessage::decode(&message.encode()).unwrap() {
            ServerMessage::Error { reason } => {
                assert!(reason.starts_with("ok") && reason.len() <= MAX_TEXT_BYTES);
                assert!(reason.chars().all(|c| !c.is_control()));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn more_than_the_allowed_addresses_are_cut_when_sent() {
        let many: Vec<Ipv4Addr> = (0..20).map(|i| Ipv4Addr::new(10, 0, 0, i)).collect();
        let message = ClientMessage::Register {
            version: 1,
            port: 1,
            lan: many,
            name: "x".into(),
        };
        match ClientMessage::decode(&message.encode()).unwrap() {
            ClientMessage::Register { lan, .. } => assert_eq!(lan.len(), MAX_LAN_ADDRESSES),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_relay_hello_cannot_be_mistaken_for_an_encrypted_record() {
        let hello = relay_hello(&[7; TOKEN_LEN]);
        assert_eq!(hello.len(), RELAY_HELLO_LEN);
        assert!(hello.starts_with(RELAY_MAGIC));
        // Records begin with a little-endian u16 length of at most 65535; the
        // magic reads as 0x4352 = 17234, which no handshake or data record
        // of this protocol has (handshake messages are 32, 96 or 64 bytes).
        let as_length = u16::from_le_bytes([hello[0], hello[1]]);
        assert_eq!(as_length, 17_234);
        assert!(![32u16, 96, 64].contains(&as_length));
    }
}
