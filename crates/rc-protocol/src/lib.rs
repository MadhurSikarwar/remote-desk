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
pub const PROTOCOL_VERSION: u16 = 1;

/// Upper bound on one message, so a corrupt or hostile length prefix cannot
/// make the reader allocate without limit. A 4K keyframe is far below this.
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

const MAX_TEXT_BYTES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The device whose screen is shared.
    Host,
    /// The device that views it.
    Viewer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// First message in each direction.
    Hello { version: u16, role: Role },
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

    fn u64(&mut self) -> Result<u64, ProtocolError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    fn bytes(&mut self) -> Result<&'a [u8], ProtocolError> {
        let len = self.u32()? as usize;
        self.take(len)
    }

    fn finish(self) -> Result<(), ProtocolError> {
        if self.rest.is_empty() {
            Ok(())
        } else {
            Err(ProtocolError::Malformed("trailing bytes"))
        }
    }
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

impl Message {
    /// Appends the message body (type byte and payload, without the length
    /// prefix) to `out`.
    pub fn encode_body(&self, out: &mut Vec<u8>) {
        match self {
            Self::Hello { version, role } => {
                out.push(tag::HELLO);
                out.extend_from_slice(&version.to_le_bytes());
                out.push(match role {
                    Role::Host => 0,
                    Role::Viewer => 1,
                });
            }
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
                let mut end = reason.len().min(MAX_TEXT_BYTES);
                while !reason.is_char_boundary(end) {
                    end -= 1;
                }
                put_bytes(out, &reason.as_bytes()[..end]);
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
            },
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
            tag::BYE => {
                let text = r.bytes()?;
                if text.len() > MAX_TEXT_BYTES {
                    return Err(ProtocolError::Malformed("text too long"));
                }
                Self::Bye {
                    reason: String::from_utf8(text.to_vec())
                        .map_err(|_| ProtocolError::Malformed("text is not UTF-8"))?,
                }
            }
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
            },
            Message::Hello {
                version: 0xBEEF,
                role: Role::Viewer,
            },
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
        // Hello with role 9, frame with keyframe flag 2, Bye with invalid UTF-8.
        for body in [
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
