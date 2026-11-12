use std::collections::VecDeque;
use std::io::{Read, Write};

use rc_crypto::channel::{read_record, write_record, Established, Handshake, Side};
use rc_crypto::{CryptoError, Identity};

/// Runs a handshake between two identities in memory, passing every message
/// through `relay`, which may alter it.
fn handshake_via(
    initiator: &Identity,
    responder: &Identity,
    mut relay: impl FnMut(usize, Vec<u8>) -> Vec<u8>,
) -> Result<(Established, Established), CryptoError> {
    let mut a = Handshake::new(initiator, Side::Initiator)?;
    let mut b = Handshake::new(responder, Side::Responder)?;
    let mut step = 0;
    while !(a.is_finished() && b.is_finished()) {
        let (from, to) = if a.is_my_turn() {
            (&mut a, &mut b)
        } else {
            (&mut b, &mut a)
        };
        let message = relay(step, from.write_message()?);
        to.read_message(&message)?;
        step += 1;
    }
    Ok((a.finish()?, b.finish()?))
}

fn handshake(initiator: &Identity, responder: &Identity) -> (Established, Established) {
    handshake_via(initiator, responder, |_, message| message).unwrap()
}

fn open_all(opener: &mut rc_crypto::channel::Opener) -> Result<Vec<u8>, CryptoError> {
    let mut out = Vec::new();
    while let Some(plaintext) = opener.next_plaintext()? {
        out.extend_from_slice(&plaintext);
    }
    Ok(out)
}

#[test]
fn both_sides_learn_each_others_key_and_see_the_same_code() {
    let viewer = Identity::generate().unwrap();
    let host = Identity::generate().unwrap();
    let (v, h) = handshake(&viewer, &host);

    assert_eq!(&v.remote_public, host.public());
    assert_eq!(&h.remote_public, viewer.public());
    assert_eq!(v.auth_code, h.auth_code);
    assert_eq!(v.auth_code.len(), 7);
    assert!(v.auth_code.chars().all(|c| c.is_ascii_digit() || c == ' '));
}

#[test]
fn every_handshake_gives_a_different_code_and_different_ciphertext() {
    let viewer = Identity::generate().unwrap();
    let host = Identity::generate().unwrap();
    let (mut first, _) = handshake(&viewer, &host);
    let (mut second, _) = handshake(&viewer, &host);

    assert_ne!(first.auth_code, second.auth_code);
    assert_ne!(
        first.sealer.seal(b"same words").unwrap(),
        second.sealer.seal(b"same words").unwrap()
    );
}

#[test]
fn data_round_trips_in_both_directions() {
    let (mut v, mut h) = handshake(
        &Identity::generate().unwrap(),
        &Identity::generate().unwrap(),
    );

    let sealed = v.sealer.seal(b"from the viewer").unwrap();
    assert!(!sealed.windows(15).any(|w| w == b"from the viewer"));
    h.opener.push(&sealed);
    assert_eq!(open_all(&mut h.opener).unwrap(), b"from the viewer");

    h.opener.push(&v.sealer.seal(b"again").unwrap());
    assert_eq!(open_all(&mut h.opener).unwrap(), b"again");

    v.opener.push(&h.sealer.seal(b"from the host").unwrap());
    assert_eq!(open_all(&mut v.opener).unwrap(), b"from the host");
}

#[test]
fn large_data_is_split_into_records_and_survives_byte_by_byte_delivery() {
    let (mut v, mut h) = handshake(
        &Identity::generate().unwrap(),
        &Identity::generate().unwrap(),
    );
    let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();

    let sealed = v.sealer.seal(&big).unwrap();
    // 200 000 bytes need four records of at most 65 519 bytes of plaintext.
    assert_eq!(sealed.len(), big.len() + 4 * (2 + 16));

    let mut received = Vec::new();
    for byte in sealed {
        h.opener.push(&[byte]);
        received.extend_from_slice(&open_all(&mut h.opener).unwrap());
    }
    assert_eq!(received, big);
    assert!(v.sealer.seal(&[]).unwrap().is_empty());
}

#[test]
fn any_changed_bit_is_detected() {
    let viewer = Identity::generate().unwrap();
    let host = Identity::generate().unwrap();
    let pristine = {
        let (mut v, _) = handshake(&viewer, &host);
        v.sealer.seal(b"move the mouse").unwrap().len()
    };
    // Flip one bit at every position of the record in turn.
    for position in 0..pristine {
        let (mut v, mut h) = handshake(&viewer, &host);
        let mut sealed = v.sealer.seal(b"move the mouse").unwrap();
        sealed[position] ^= 0x01;
        h.opener.push(&sealed);
        match h.opener.next_plaintext() {
            Err(CryptoError::Channel(_)) => {}
            // A changed length byte can leave the record incomplete.
            Ok(None) if position < 2 => {}
            other => panic!("bit flip at {position} was not rejected: {other:?}"),
        }
    }
}

#[test]
fn replayed_reordered_and_dropped_records_are_rejected() {
    let fresh = || {
        handshake(
            &Identity::generate().unwrap(),
            &Identity::generate().unwrap(),
        )
    };

    // Replay: the same record delivered twice.
    let (mut v, mut h) = fresh();
    let record = v.sealer.seal(b"click").unwrap();
    h.opener.push(&record);
    assert_eq!(open_all(&mut h.opener).unwrap(), b"click");
    h.opener.push(&record);
    assert!(h.opener.next_plaintext().is_err());

    // Reorder: second record delivered first.
    let (mut v, mut h) = fresh();
    let first = v.sealer.seal(b"one").unwrap();
    let second = v.sealer.seal(b"two").unwrap();
    h.opener.push(&second);
    assert!(h.opener.next_plaintext().is_err());
    drop(first);

    // Drop: first record never arrives.
    let (mut v, mut h) = fresh();
    let _lost = v.sealer.seal(b"one").unwrap();
    h.opener.push(&v.sealer.seal(b"two").unwrap());
    assert!(h.opener.next_plaintext().is_err());
}

#[test]
fn data_cannot_be_reflected_back_or_read_by_another_session() {
    let viewer = Identity::generate().unwrap();
    let host = Identity::generate().unwrap();
    let (mut v, mut h) = handshake(&viewer, &host);
    let (_, mut other_host) = handshake(&viewer, &host);
    let sealed = v.sealer.seal(b"secret").unwrap();

    // Sent back to its own sender.
    v.opener.push(&sealed);
    assert!(v.opener.next_plaintext().is_err());
    // Delivered to a different session between the same two devices.
    other_host.opener.push(&sealed);
    assert!(other_host.opener.next_plaintext().is_err());
    // Delivered where it belongs.
    h.opener.push(&sealed);
    assert_eq!(open_all(&mut h.opener).unwrap(), b"secret");
}

#[test]
fn a_tampered_handshake_fails() {
    let viewer = Identity::generate().unwrap();
    let host = Identity::generate().unwrap();
    // The first message is an unauthenticated public value by design; the
    // second and third carry authenticated content.
    for step in [1, 2] {
        let result = handshake_via(&viewer, &host, |at, mut message| {
            if at == step {
                let last = message.len() - 1;
                message[last] ^= 0x01;
            }
            message
        });
        assert!(
            result.is_err(),
            "tampering with message {step} went unnoticed"
        );
    }
}

#[test]
fn a_man_in_the_middle_shows_each_person_a_different_code() {
    let viewer = Identity::generate().unwrap();
    let host = Identity::generate().unwrap();
    let attacker = Identity::generate().unwrap();

    // The attacker answers the viewer as if it were the host, and calls the
    // host as if it were the viewer.
    let (viewer_side, _) = handshake(&viewer, &attacker);
    let (_, host_side) = handshake(&attacker, &host);

    // Neither sees the other's real key…
    assert_eq!(&viewer_side.remote_public, attacker.public());
    assert_eq!(&host_side.remote_public, attacker.public());
    // …and the codes they would read to each other do not match.
    assert_ne!(viewer_side.auth_code, host_side.auth_code);
}

#[test]
fn garbage_and_short_records_are_errors_not_panics() {
    let (_, mut h) = handshake(
        &Identity::generate().unwrap(),
        &Identity::generate().unwrap(),
    );
    h.opener.push(&[5, 0, 1, 2, 3, 4, 5]);
    assert!(matches!(
        h.opener.next_plaintext(),
        Err(CryptoError::Channel(_))
    ));

    let (_, mut h) = handshake(
        &Identity::generate().unwrap(),
        &Identity::generate().unwrap(),
    );
    h.opener.push(&[40, 0]);
    h.opener.push(&[0xAB; 40]);
    assert!(h.opener.next_plaintext().is_err());

    let mut responder = Handshake::new(&Identity::generate().unwrap(), Side::Responder).unwrap();
    assert!(responder.read_message(b"not a handshake").is_err());
    assert!(responder.read_message(&[]).is_err());
}

/// Two in-memory pipes joined into a connection.
struct Pipe {
    incoming: VecDeque<u8>,
    outgoing: Vec<u8>,
}

impl Read for Pipe {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = buf.len().min(self.incoming.len());
        for slot in &mut buf[..n] {
            *slot = self.incoming.pop_front().unwrap_or_default();
        }
        Ok(n)
    }
}

impl Write for Pipe {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.outgoing.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn records_round_trip_and_a_closed_connection_is_an_error() {
    let mut pipe = Pipe {
        incoming: VecDeque::new(),
        outgoing: Vec::new(),
    };
    write_record(&mut pipe, b"hello").unwrap();
    write_record(&mut pipe, &[]).unwrap();
    assert_eq!(pipe.outgoing, [5, 0, b'h', b'e', b'l', b'l', b'o', 0, 0]);
    assert!(write_record(&mut pipe, &vec![0; 70_000]).is_err());

    pipe.incoming = pipe.outgoing.drain(..).collect();
    assert_eq!(read_record(&mut pipe).unwrap(), b"hello");
    assert_eq!(read_record(&mut pipe).unwrap(), b"");
    assert!(matches!(
        read_record(&mut pipe),
        Err(CryptoError::Connection(_))
    ));
}

#[test]
fn the_handshake_runs_over_a_real_connection() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let host = Identity::generate().unwrap();
    let host_public = *host.public();

    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut session = Handshake::run(&host, Side::Responder, &mut stream).unwrap();
        stream
            .write_all(&session.sealer.seal(b"welcome").unwrap())
            .unwrap();
        session.auth_code
    });

    let viewer = Identity::generate().unwrap();
    let mut stream = std::net::TcpStream::connect(address).unwrap();
    let mut session = Handshake::run(&viewer, Side::Initiator, &mut stream).unwrap();
    assert_eq!(session.remote_public, host_public);

    let mut received = Vec::new();
    let mut chunk = [0u8; 64];
    while received.len() < 7 {
        let n = stream.read(&mut chunk).unwrap();
        assert!(n > 0, "connection closed early");
        session.opener.push(&chunk[..n]);
        received.extend_from_slice(&open_all(&mut session.opener).unwrap());
    }
    assert_eq!(received, b"welcome");
    assert_eq!(server.join().unwrap(), session.auth_code);
}
