//! Wire protocol between two Remote Control devices.
//!
//! A connection carries a sequence of messages. Each one is framed as
//!
//! ```text
//! u32 length (little endian) | u8 message type | payload
//! ```
//!
//! where `length` counts the type byte and the payload. Integers are little
//! endian; byte strings and text carry a u32 length prefix.
//!
//! The format says nothing about confidentiality: it must only ever run
//! inside an encrypted channel or over the loopback interface.

#![forbid(unsafe_code)]

use std::io::{self, Read, Write};

/// Bumped on any incompatible change; peers with different versions refuse
/// to talk.
pub const PROTOCOL_VERSION: u16 = 5;

/// Upper bound on one message, so a corrupt or hostile length prefix cannot
/// make the reader allocate without limit. A 4K keyframe is far below this.
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

const MAX_TEXT_BYTES: usize = 1024;

/// Largest piece of text one clipboard message may carry.
pub const MAX_CLIPBOARD_BYTES: usize = 1024 * 1024;
/// Largest piece of a file one chunk message may carry.
pub const MAX_FILE_CHUNK_BYTES: usize = 64 * 1024;
/// Longest file name accepted, in bytes.
pub const MAX_FILE_NAME_BYTES: usize = 255;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The device whose screen is shared.
    Host,
    /// The device that views it.
    Viewer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// One mouse or keyboard action by the viewer.
///
/// Positions are fractions of the shared display: 0 is the left or top
/// edge, 65535 the right or bottom edge. That keeps them independent of the
/// size at which the viewer happens to draw the picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    MouseMove {
        x: u16,
        y: u16,
    },
    MouseButton {
        button: MouseButton,
        down: bool,
        x: u16,
        y: u16,
    },
    /// Wheel movement in the units Windows uses: 120 per notch, positive
    /// away from the user (`dy`) or to the right (`dx`).
    MouseWheel {
        dx: i16,
        dy: i16,
    },
    /// A physical key, identified by its PC keyboard scan code so the
    /// host's own keyboard layout decides which character it produces.
    Key {
        scancode: u16,
        extended: bool,
        down: bool,
    },
}

/// What the host currently lets the viewer do besides watching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Permissions {
    /// Mouse and keyboard control.
    pub input: bool,
    /// Clipboard text may be synchronised (the viewer must also want it).
    pub clipboard: bool,
    /// Files may be offered in either direction (each still needs the
    /// receiving person's acceptance).
    pub files: bool,
}

impl Permissions {
    pub const INPUT: u8 = 1;
    pub const CLIPBOARD: u8 = 2;
    pub const FILES: u8 = 4;

    /// The permissions as the bit set carried on the wire.
    pub fn bits(self) -> u8 {
        let mut bits = 0;
        if self.input {
            bits |= Self::INPUT;
        }
        if self.clipboard {
            bits |= Self::CLIPBOARD;
        }
        if self.files {
            bits |= Self::FILES;
        }
        bits
    }

    /// The inverse of [`Permissions::bits`]; `None` if an unknown bit is
    /// set.
    pub fn from_bits(bits: u8) -> Option<Self> {
        if bits & !(Self::INPUT | Self::CLIPBOARD | Self::FILES) != 0 {
            return None;
        }
        Some(Self {
            input: bits & Self::INPUT != 0,
            clipboard: bits & Self::CLIPBOARD != 0,
            files: bits & Self::FILES != 0,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// First message in each direction.
    Hello {
        version: u16,
        role: Role,
        /// The name the device goes by. Chosen by its user, so it is shown
        /// to people but never used to decide anything.
        name: String,
    },
    /// Host to viewer: the person at the host allowed this session.
    Accepted,
    /// Sent by the host before the first frame and whenever the size changes.
    VideoConfig { width: u32, height: u32, fps: u32 },
    /// One H.264 access unit in Annex B format.
    VideoFrame {
        keyframe: bool,
        /// When the frame was captured: microseconds since the Unix epoch on
        /// the host's clock.
        timestamp_us: u64,
        data: Vec<u8>,
    },
    /// The viewer cannot decode and needs a fresh keyframe.
    KeyframeRequest,
    /// Orderly end of the session.
    Bye { reason: String },
    /// Viewer to host. Ignored unless the host has allowed input.
    Input(InputEvent),
    /// Host to viewer, at the start of a session and whenever it changes.
    Permissions(Permissions),
    /// Text copied on the sender's computer, in either direction. Sent only
    /// while clipboard sync is on at both ends.
    ClipboardText(String),
    /// A file the sender wants to send. `id` is chosen by the sender and
    /// names the transfer in every message about it.
    FileOffer { id: u32, name: String, size: u64 },
    /// The receiver's answer to an offer.
    FileAnswer {
        id: u32,
        accept: bool,
        /// Why it was refused; empty when accepted.
        reason: String,
    },
    /// Part of an accepted file, in order.
    FileChunk { id: u32, data: Vec<u8> },
    /// The whole file has been sent; `sha256` is the hash of its bytes.
    FileEnd { id: u32, sha256: [u8; 32] },
    /// The receiver has the whole file and its hash matched. Last message
    /// of a successful transfer.
    FileDone { id: u32 },
    /// Either side stops a transfer. `by_sender` says whose transfer
    /// numbering `id` belongs to.
    FileCancel {
        id: u32,
        by_sender: bool,
        reason: String,
    },
    /// Liveness check, sent by both ends while a session runs. The other end
    /// answers with a [`Message::Pong`] carrying the same `nonce`.
    Ping { nonce: u64 },
    /// The answer to a [`Message::Ping`].
    Pong { nonce: u64 },
}

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("connection error: {0}")]
    Io(#[from] io::Error),
    #[error("message of {0} bytes exceeds the limit")]
    TooLarge(usize),
    #[error("empty message")]
    Empty,
    #[error("unknown message type {0}")]
    UnknownType(u8),
    #[error("malformed message: {0}")]
    Malformed(&'static str),
}

mod tag {
    pub const HELLO: u8 = 1;
    pub const VIDEO_CONFIG: u8 = 2;
    pub const VIDEO_FRAME: u8 = 3;
    pub const KEYFRAME_REQUEST: u8 = 4;
    pub const BYE: u8 = 5;
    pub const INPUT: u8 = 6;
    pub const PERMISSIONS: u8 = 7;
    pub const ACCEPTED: u8 = 8;
    pub const CLIPBOARD_TEXT: u8 = 9;
    pub const FILE_OFFER: u8 = 10;
    pub const FILE_ANSWER: u8 = 11;
    pub const FILE_CHUNK: u8 = 12;
    pub const FILE_END: u8 = 13;
    pub const FILE_CANCEL: u8 = 14;
    pub const FILE_DONE: u8 = 15;
    pub const PING: u8 = 16;
    pub const PONG: u8 = 17;
}

mod input_tag {
    pub const MOUSE_MOVE: u8 = 1;
    pub const MOUSE_BUTTON: u8 = 2;
    pub const MOUSE_WHEEL: u8 = 3;
    pub const KEY: u8 = 4;
}

/// Reads fields from a message body, failing instead of panicking when the
/// body is short.
struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ProtocolError> {
        if self.rest.len() < n {
            return Err(ProtocolError::Malformed("truncated"));
        }
        let (head, tail) = self.rest.split_at(n);
        self.rest = tail;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], ProtocolError> {
        let mut out = [0; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, ProtocolError> {
        Ok(self.array::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, ProtocolError> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    fn u32(&mut self) -> Result<u32, ProtocolError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    /// A byte string no longer than `max`.
    fn limited_bytes(&mut self, max: usize) -> Result<&'a [u8], ProtocolError> {
        let bytes = self.bytes()?;
        if bytes.len() > max {
            return Err(ProtocolError::Malformed("too long"));
        }
        Ok(bytes)
    }

    fn i16(&mut self) -> Result<i16, ProtocolError> {
        Ok(i16::from_le_bytes(self.array()?))
    }

    fn bool(&mut self) -> Result<bool, ProtocolError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(ProtocolError::Malformed("bad flag")),
        }
    }

    fn u64(&mut self) -> Result<u64, ProtocolError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    fn bytes(&mut self) -> Result<&'a [u8], ProtocolError> {
        let len = self.u32()? as usize;
        self.take(len)
    }

    fn text(&mut self) -> Result<String, ProtocolError> {
        let bytes = self.bytes()?;
        if bytes.len() > MAX_TEXT_BYTES {
            return Err(ProtocolError::Malformed("text too long"));
        }
        String::from_utf8(bytes.to_vec()).map_err(|_| ProtocolError::Malformed("text is not UTF-8"))
    }

    fn finish(self) -> Result<(), ProtocolError> {
        if self.rest.is_empty() {
            Ok(())
        } else {
            Err(ProtocolError::Malformed("trailing bytes"))
        }
    }
}

/// Writes text, cut to the size limit on a character boundary.
fn put_text(out: &mut Vec<u8>, text: &str) {
    let mut end = text.len().min(MAX_TEXT_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    put_bytes(out, &text.as_bytes()[..end]);
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

impl InputEvent {
    fn encode(&self, out: &mut Vec<u8>) {
        match *self {
            Self::MouseMove { x, y } => {
                out.push(input_tag::MOUSE_MOVE);
                out.extend_from_slice(&x.to_le_bytes());
                out.extend_from_slice(&y.to_le_bytes());
            }
            Self::MouseButton { button, down, x, y } => {
                out.push(input_tag::MOUSE_BUTTON);
                out.push(match button {
                    MouseButton::Left => 0,
                    MouseButton::Right => 1,
                    MouseButton::Middle => 2,
                });
                out.push(u8::from(down));
                out.extend_from_slice(&x.to_le_bytes());
                out.extend_from_slice(&y.to_le_bytes());
            }
            Self::MouseWheel { dx, dy } => {
                out.push(input_tag::MOUSE_WHEEL);
                out.extend_from_slice(&dx.to_le_bytes());
                out.extend_from_slice(&dy.to_le_bytes());
            }
            Self::Key {
                scancode,
                extended,
                down,
            } => {
                out.push(input_tag::KEY);
                out.extend_from_slice(&scancode.to_le_bytes());
                out.push(u8::from(extended));
                out.push(u8::from(down));
            }
        }
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self, ProtocolError> {
        Ok(match r.u8()? {
            input_tag::MOUSE_MOVE => Self::MouseMove {
                x: r.u16()?,
                y: r.u16()?,
            },
            input_tag::MOUSE_BUTTON => Self::MouseButton {
                button: match r.u8()? {
                    0 => MouseButton::Left,
                    1 => MouseButton::Right,
                    2 => MouseButton::Middle,
                    _ => return Err(ProtocolError::Malformed("unknown mouse button")),
                },
                down: r.bool()?,
                x: r.u16()?,
                y: r.u16()?,
            },
            input_tag::MOUSE_WHEEL => Self::MouseWheel {
                dx: r.i16()?,
                dy: r.i16()?,
            },
            input_tag::KEY => Self::Key {
                scancode: r.u16()?,
                extended: r.bool()?,
                down: r.bool()?,
            },
            _ => return Err(ProtocolError::Malformed("unknown input event")),
        })
    }
}

impl Message {
    /// Appends the message body (type byte and payload, without the length
    /// prefix) to `out`.
    pub fn encode_body(&self, out: &mut Vec<u8>) {
        match self {
            Self::Hello {
                version,
                role,
                name,
            } => {
                out.push(tag::HELLO);
                out.extend_from_slice(&version.to_le_bytes());
                out.push(match role {
                    Role::Host => 0,
                    Role::Viewer => 1,
                });
                put_text(out, name);
            }
            Self::Accepted => out.push(tag::ACCEPTED),
            Self::VideoConfig { width, height, fps } => {
                out.push(tag::VIDEO_CONFIG);
                for value in [width, height, fps] {
                    out.extend_from_slice(&value.to_le_bytes());
                }
            }
            Self::VideoFrame {
                keyframe,
                timestamp_us,
                data,
            } => {
                out.push(tag::VIDEO_FRAME);
                out.push(u8::from(*keyframe));
                out.extend_from_slice(&timestamp_us.to_le_bytes());
                put_bytes(out, data);
            }
            Self::KeyframeRequest => out.push(tag::KEYFRAME_REQUEST),
            Self::Bye { reason } => {
                out.push(tag::BYE);
                put_text(out, reason);
            }
            Self::Input(event) => {
                out.push(tag::INPUT);
                event.encode(out);
            }
            Self::Permissions(permissions) => {
                out.push(tag::PERMISSIONS);
                out.push(permissions.bits());
            }
            Self::ClipboardText(text) => {
                out.push(tag::CLIPBOARD_TEXT);
                let mut end = text.len().min(MAX_CLIPBOARD_BYTES);
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                put_bytes(out, &text.as_bytes()[..end]);
            }
            Self::FileOffer { id, name, size } => {
                out.push(tag::FILE_OFFER);
                out.extend_from_slice(&id.to_le_bytes());
                out.extend_from_slice(&size.to_le_bytes());
                let mut end = name.len().min(MAX_FILE_NAME_BYTES);
                while !name.is_char_boundary(end) {
                    end -= 1;
                }
                put_bytes(out, &name.as_bytes()[..end]);
            }
            Self::FileAnswer { id, accept, reason } => {
                out.push(tag::FILE_ANSWER);
                out.extend_from_slice(&id.to_le_bytes());
                out.push(u8::from(*accept));
                put_text(out, reason);
            }
            Self::FileChunk { id, data } => {
                out.push(tag::FILE_CHUNK);
                out.extend_from_slice(&id.to_le_bytes());
                put_bytes(out, &data[..data.len().min(MAX_FILE_CHUNK_BYTES)]);
            }
            Self::FileEnd { id, sha256 } => {
                out.push(tag::FILE_END);
                out.extend_from_slice(&id.to_le_bytes());
                out.extend_from_slice(sha256);
            }
            Self::FileDone { id } => {
                out.push(tag::FILE_DONE);
                out.extend_from_slice(&id.to_le_bytes());
            }
            Self::Ping { nonce } => {
                out.push(tag::PING);
                out.extend_from_slice(&nonce.to_le_bytes());
            }
            Self::Pong { nonce } => {
                out.push(tag::PONG);
                out.extend_from_slice(&nonce.to_le_bytes());
            }
            Self::FileCancel {
                id,
                by_sender,
                reason,
            } => {
                out.push(tag::FILE_CANCEL);
                out.extend_from_slice(&id.to_le_bytes());
                out.push(u8::from(*by_sender));
                put_text(out, reason);
            }
        }
    }

    /// Parses a message body. Every byte must be consumed.
    pub fn decode_body(body: &[u8]) -> Result<Self, ProtocolError> {
        let mut r = Reader { rest: body };
        let kind = r.u8().map_err(|_| ProtocolError::Empty)?;
        let message = match kind {
            tag::HELLO => Self::Hello {
                version: r.u16()?,
                role: match r.u8()? {
                    0 => Role::Host,
                    1 => Role::Viewer,
                    _ => return Err(ProtocolError::Malformed("unknown role")),
                },
                name: r.text()?,
            },
            tag::ACCEPTED => Self::Accepted,
            tag::VIDEO_CONFIG => Self::VideoConfig {
                width: r.u32()?,
                height: r.u32()?,
                fps: r.u32()?,
            },
            tag::VIDEO_FRAME => Self::VideoFrame {
                keyframe: match r.u8()? {
                    0 => false,
                    1 => true,
                    _ => return Err(ProtocolError::Malformed("bad keyframe flag")),
                },
                timestamp_us: r.u64()?,
                data: r.bytes()?.to_vec(),
            },
            tag::KEYFRAME_REQUEST => Self::KeyframeRequest,
            tag::BYE => Self::Bye { reason: r.text()? },
            tag::INPUT => Self::Input(InputEvent::decode(&mut r)?),
            tag::PERMISSIONS => Self::Permissions(
                Permissions::from_bits(r.u8()?)
                    .ok_or(ProtocolError::Malformed("unknown permission"))?,
            ),
            tag::CLIPBOARD_TEXT => Self::ClipboardText(
                String::from_utf8(r.limited_bytes(MAX_CLIPBOARD_BYTES)?.to_vec())
                    .map_err(|_| ProtocolError::Malformed("text is not UTF-8"))?,
            ),
            tag::FILE_OFFER => {
                let id = r.u32()?;
                let size = r.u64()?;
                let name = String::from_utf8(r.limited_bytes(MAX_FILE_NAME_BYTES)?.to_vec())
                    .map_err(|_| ProtocolError::Malformed("name is not UTF-8"))?;
                Self::FileOffer { id, name, size }
            }
            tag::FILE_ANSWER => Self::FileAnswer {
                id: r.u32()?,
                accept: r.bool()?,
                reason: r.text()?,
            },
            tag::FILE_CHUNK => Self::FileChunk {
                id: r.u32()?,
                data: r.limited_bytes(MAX_FILE_CHUNK_BYTES)?.to_vec(),
            },
            tag::FILE_END => Self::FileEnd {
                id: r.u32()?,
                sha256: r.array()?,
            },
            tag::FILE_DONE => Self::FileDone { id: r.u32()? },
            tag::PING => Self::Ping { nonce: r.u64()? },
            tag::PONG => Self::Pong { nonce: r.u64()? },
            tag::FILE_CANCEL => Self::FileCancel {
                id: r.u32()?,
                by_sender: r.bool()?,
                reason: r.text()?,
            },
            other => return Err(ProtocolError::UnknownType(other)),
        };
        r.finish()?;
        Ok(message)
    }
}

/// Writes one framed message. The caller flushes if the writer buffers.
pub fn write_message<W: Write>(writer: &mut W, message: &Message) -> Result<(), ProtocolError> {
    let mut frame = vec![0u8; 4];
    message.encode_body(&mut frame);
    let body_len = frame.len() - 4;
    if body_len > MAX_MESSAGE_BYTES {
        return Err(ProtocolError::TooLarge(body_len));
    }
    frame[..4].copy_from_slice(&(body_len as u32).to_le_bytes());
    writer.write_all(&frame)?;
    Ok(())
}

/// Reads one framed message. A connection closed between messages surfaces
/// as an `Io` error of kind `UnexpectedEof`.
pub fn read_message<R: Read>(reader: &mut R) -> Result<Message, ProtocolError> {
    let mut len = [0u8; 4];
    reader.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len == 0 {
        return Err(ProtocolError::Empty);
    }
    if len > MAX_MESSAGE_BYTES {
        return Err(ProtocolError::TooLarge(len));
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    Message::decode_body(&body)
}

/// Reassembles messages from bytes that arrive in arbitrary pieces.
///
/// Unlike [`read_message`], this never blocks and never loses data when a
/// read is interrupted part-way through a message, so it suits sockets with
/// a read timeout.
#[derive(Debug, Default)]
pub struct MessageDecoder {
    buffer: Vec<u8>,
}

impl MessageDecoder {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// The next complete message, or `None` until more bytes arrive. After
    /// an error the stream is unusable.
    pub fn next_message(&mut self) -> Result<Option<Message>, ProtocolError> {
        let Some((prefix, rest)) = self.buffer.split_first_chunk::<4>() else {
            return Ok(None);
        };
        let len = u32::from_le_bytes(*prefix) as usize;
        if len == 0 {
            return Err(ProtocolError::Empty);
        }
        if len > MAX_MESSAGE_BYTES {
            return Err(ProtocolError::TooLarge(len));
        }
        if rest.len() < len {
            return Ok(None);
        }
        let message = Message::decode_body(&rest[..len]);
        self.buffer.drain(..4 + len);
        message.map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples() -> Vec<Message> {
        vec![
            Message::Hello {
                version: PROTOCOL_VERSION,
                role: Role::Host,
                name: "Workshop PC".into(),
            },
            Message::Hello {
                version: 0xBEEF,
                role: Role::Viewer,
                name: String::new(),
            },
            Message::Accepted,
            Message::VideoConfig {
                width: 1920,
                height: 1080,
                fps: 30,
            },
            Message::VideoFrame {
                keyframe: true,
                timestamp_us: u64::MAX - 7,
                data: vec![0, 0, 0, 1, 0x67, 0xFF],
            },
            Message::VideoFrame {
                keyframe: false,
                timestamp_us: 0,
                data: Vec::new(),
            },
            Message::KeyframeRequest,
            Message::Bye {
                reason: "stopped by the user — bye".into(),
            },
            Message::Input(InputEvent::MouseMove { x: 0, y: 65535 }),
            Message::Input(InputEvent::MouseButton {
                button: MouseButton::Right,
                down: true,
                x: 1234,
                y: 4321,
            }),
            Message::Input(InputEvent::MouseWheel { dx: -120, dy: 240 }),
            Message::Input(InputEvent::Key {
                scancode: 0x1D,
                extended: true,
                down: false,
            }),
            Message::Permissions(Permissions {
                input: true,
                clipboard: true,
                files: true,
            }),
            Message::Permissions(Permissions::default()),
            Message::Permissions(Permissions {
                input: false,
                clipboard: true,
                files: false,
            }),
            Message::ClipboardText("copied — ünïcode ✓".into()),
            Message::ClipboardText(String::new()),
            Message::FileOffer {
                id: 7,
                name: "report (final).pdf".into(),
                size: u64::MAX,
            },
            Message::FileAnswer {
                id: 7,
                accept: false,
                reason: "not now".into(),
            },
            Message::FileAnswer {
                id: 8,
                accept: true,
                reason: String::new(),
            },
            Message::FileChunk {
                id: 7,
                data: vec![0xAB; 1000],
            },
            Message::FileChunk {
                id: 7,
                data: Vec::new(),
            },
            Message::FileEnd {
                id: 7,
                sha256: [0x5A; 32],
            },
            Message::FileDone { id: 7 },
            Message::Ping { nonce: u64::MAX },
            Message::Pong {
                nonce: 0x0123_4567_89AB_CDEF,
            },
            Message::FileCancel {
                id: 7,
                by_sender: true,
                reason: "changed my mind".into(),
            },
        ]
    }

    fn framed(message: &Message) -> Vec<u8> {
        let mut out = Vec::new();
        write_message(&mut out, message).unwrap();
        out
    }

    #[test]
    fn every_message_round_trips() {
        for message in samples() {
            let bytes = framed(&message);
            assert_eq!(read_message(&mut bytes.as_slice()).unwrap(), message);
        }
    }

    #[test]
    fn a_stream_of_messages_is_read_back_in_order() {
        let mut stream = Vec::new();
        for message in samples() {
            write_message(&mut stream, &message).unwrap();
        }
        let mut reader = stream.as_slice();
        for message in samples() {
            assert_eq!(read_message(&mut reader).unwrap(), message);
        }
        match read_message(&mut reader) {
            Err(ProtocolError::Io(e)) => assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof),
            other => panic!("expected end of stream, got {other:?}"),
        }
    }

    #[test]
    fn layout_is_stable() {
        let bytes = framed(&Message::VideoConfig {
            width: 2,
            height: 3,
            fps: 4,
        });
        assert_eq!(bytes, [13, 0, 0, 0, 2, 2, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0]);
    }

    #[test]
    fn every_truncation_is_rejected_without_panicking() {
        for message in samples() {
            let mut body = Vec::new();
            message.encode_body(&mut body);
            for cut in 0..body.len() {
                assert!(
                    Message::decode_body(&body[..cut]).is_err(),
                    "{message:?} cut at {cut} was accepted"
                );
            }
        }
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        for message in samples() {
            let mut body = Vec::new();
            message.encode_body(&mut body);
            body.push(0);
            assert!(matches!(
                Message::decode_body(&body),
                Err(ProtocolError::Malformed(_))
            ));
        }
    }

    #[test]
    fn unknown_type_and_bad_fields_are_rejected() {
        assert!(matches!(
            Message::decode_body(&[200]),
            Err(ProtocolError::UnknownType(200))
        ));
        assert!(matches!(
            Message::decode_body(&[]),
            Err(ProtocolError::Empty)
        ));
        // Hello with role 9, frame with keyframe flag 2, Bye with invalid UTF-8,
        // input of an unknown kind, mouse button 7, key with flag 2,
        // permissions with an unknown bit.
        for body in [
            vec![tag::INPUT, 99],
            vec![tag::INPUT, input_tag::MOUSE_BUTTON, 7, 1, 0, 0, 0, 0],
            vec![tag::INPUT, input_tag::KEY, 0x1E, 0, 0, 2],
            vec![tag::PERMISSIONS, 0x80],
            vec![tag::PERMISSIONS, 0x08],
            vec![tag::FILE_ANSWER, 0, 0, 0, 0, 2, 0],
            vec![tag::FILE_CANCEL, 0, 0, 0, 0, 9, 0],
            vec![tag::CLIPBOARD_TEXT, 2, 0, 0, 0, 0xFF, 0xFE],
            vec![tag::HELLO, 1, 0, 9],
            vec![tag::VIDEO_FRAME, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            vec![tag::BYE, 2, 0, 0, 0, 0xFF, 0xFE],
        ] {
            assert!(matches!(
                Message::decode_body(&body),
                Err(ProtocolError::Malformed(_))
            ));
        }
    }

    #[test]
    fn oversized_and_zero_lengths_are_rejected_before_reading_the_body() {
        let huge = ((MAX_MESSAGE_BYTES + 1) as u32).to_le_bytes();
        assert!(matches!(
            read_message(&mut huge.as_slice()),
            Err(ProtocolError::TooLarge(_))
        ));
        assert!(matches!(
            read_message(&mut [0u8; 4].as_slice()),
            Err(ProtocolError::Empty)
        ));
    }

    #[test]
    fn a_length_longer_than_the_data_is_an_io_error() {
        let mut bytes = framed(&Message::KeyframeRequest);
        bytes[0] = 50;
        assert!(matches!(
            read_message(&mut bytes.as_slice()),
            Err(ProtocolError::Io(_))
        ));
    }

    #[test]
    fn decoder_reassembles_messages_fed_one_byte_at_a_time() {
        let mut stream = Vec::new();
        for message in samples() {
            write_message(&mut stream, &message).unwrap();
        }
        let mut decoder = MessageDecoder::default();
        let mut decoded = Vec::new();
        for byte in stream {
            decoder.push(&[byte]);
            while let Some(message) = decoder.next_message().unwrap() {
                decoded.push(message);
            }
        }
        assert_eq!(decoded, samples());
        assert_eq!(decoder.next_message().unwrap(), None);
    }

    #[test]
    fn decoder_yields_every_message_from_one_large_push() {
        let mut stream = Vec::new();
        for message in samples() {
            write_message(&mut stream, &message).unwrap();
        }
        let mut decoder = MessageDecoder::default();
        decoder.push(&stream);
        let decoded: Vec<_> = std::iter::from_fn(|| decoder.next_message().unwrap()).collect();
        assert_eq!(decoded, samples());
    }

    #[test]
    fn decoder_rejects_bad_lengths_before_the_body_arrives() {
        let mut decoder = MessageDecoder::default();
        decoder.push(&((MAX_MESSAGE_BYTES + 1) as u32).to_le_bytes());
        assert!(matches!(
            decoder.next_message(),
            Err(ProtocolError::TooLarge(_))
        ));

        let mut decoder = MessageDecoder::default();
        decoder.push(&[0; 4]);
        assert!(matches!(decoder.next_message(), Err(ProtocolError::Empty)));
    }

    #[test]
    fn permissions_bits_round_trip_for_every_combination() {
        for bits in 0..8u8 {
            let permissions = Permissions::from_bits(bits).unwrap();
            assert_eq!(permissions.bits(), bits);
        }
        for bits in 8..=255u8 {
            assert_eq!(Permissions::from_bits(bits), None, "{bits}");
        }
    }

    #[test]
    fn oversized_clipboard_text_names_and_chunks_are_refused_on_receipt_and_cut_on_send() {
        // On the wire: lengths above the limits are rejected without
        // allocating what they claim.
        for (kind, limit) in [
            (tag::CLIPBOARD_TEXT, MAX_CLIPBOARD_BYTES),
            (tag::FILE_CHUNK, MAX_FILE_CHUNK_BYTES),
        ] {
            let mut body = vec![kind];
            if kind == tag::FILE_CHUNK {
                body.extend_from_slice(&[0, 0, 0, 0]);
            }
            body.extend_from_slice(&((limit + 1) as u32).to_le_bytes());
            assert!(matches!(
                Message::decode_body(&body),
                Err(ProtocolError::Malformed(_))
            ));
        }
        let mut offer = vec![tag::FILE_OFFER, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0];
        offer.extend_from_slice(&((MAX_FILE_NAME_BYTES + 1) as u32).to_le_bytes());
        assert!(Message::decode_body(&offer).is_err());

        // When sending, over-long values are shortened rather than sent.
        let big = "é".repeat(MAX_CLIPBOARD_BYTES);
        let bytes = framed(&Message::ClipboardText(big));
        match read_message(&mut bytes.as_slice()).unwrap() {
            Message::ClipboardText(text) => {
                assert!(text.len() <= MAX_CLIPBOARD_BYTES && text.chars().all(|c| c == 'é'));
            }
            other => panic!("{other:?}"),
        }
        let name = "ü".repeat(300);
        let bytes = framed(&Message::FileOffer {
            id: 1,
            name,
            size: 5,
        });
        match read_message(&mut bytes.as_slice()).unwrap() {
            Message::FileOffer { name, .. } => assert!(name.len() <= MAX_FILE_NAME_BYTES),
            other => panic!("{other:?}"),
        }
        let bytes = framed(&Message::FileChunk {
            id: 1,
            data: vec![1; MAX_FILE_CHUNK_BYTES + 100],
        });
        match read_message(&mut bytes.as_slice()).unwrap() {
            Message::FileChunk { data, .. } => assert_eq!(data.len(), MAX_FILE_CHUNK_BYTES),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn long_reason_is_cut_on_a_character_boundary() {
        let reason = "é".repeat(MAX_TEXT_BYTES);
        let bytes = framed(&Message::Bye { reason });
        match read_message(&mut bytes.as_slice()).unwrap() {
            Message::Bye { reason } => {
                assert!(reason.len() <= MAX_TEXT_BYTES);
                assert!(reason.chars().all(|c| c == 'é'));
            }
            other => panic!("{other:?}"),
        }
    }
}
