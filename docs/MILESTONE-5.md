# Milestone 5 — device identity, encrypted sessions, approval

## Scope

1. A long-term identity for each device, stored encrypted.
2. An encrypted, mutually authenticated channel for every session.
3. Approval by the person at the host before anything is shared.
4. Remembering devices that were approved.

Sessions are **still limited to this computer** (127.0.0.1). Everything
needed to cross a network safely is now in place; opening the listener to
the network is milestone 6.

## Status

Complete.

| Part | Files |
|---|---|
| Device identity, protected storage | `crates/rc-crypto/src/identity.rs`, `protect.rs` |
| Encrypted channel, comparison code | `crates/rc-crypto/src/channel.rs` |
| Remembered devices | `crates/rc-crypto/src/trust.rs` |
| Greeting with device name, `Accepted` | `crates/rc-protocol/src/lib.rs` (protocol version 3) |
| Messages over the encrypted channel | `apps/desktop/src/secure.rs` |
| Approval on the host | `apps/desktop/src/share.rs` |
| Viewer side | `apps/desktop/src/viewer.rs` |
| Prompt, key display, remembered list | `apps/desktop/ui/` |

## How a session starts now

```
viewer                                              host
  │ ── TCP connect ───────────────────────────────►  │
  │ ◄═ Noise XX handshake (3 messages) ═══════════►  │   both prove their keys
  │ ◄═ Hello(version, role, name) ════════════════►  │   everything encrypted from here
  │                                                  │   PROMPT: name, key, code
  │            (nothing is captured or sent)         │   Allow / Deny / 30 s → Deny
  │ ◄═ Accepted ═══════════════════════════════════  │
  │ ◄═ Permissions, VideoConfig, VideoFrame… ══════  │
```

## Design

- **Identity.** An X25519 key pair generated on first run. The file
  `identity.key` holds it encrypted with Windows DPAPI for the current
  user, so another account, or the file copied to another PC, cannot use
  it. A file that exists but cannot be decrypted is an error; it is never
  replaced, since that would silently change who the device is.
- **Fingerprint.** 16 hex digits from the SHA-256 of the public key, shown
  in the window ("Device key") and in every prompt.
- **Channel.** `Noise_XX_25519_ChaChaPoly_BLAKE2s`, from the `snow` crate.
  No cryptography is implemented in this project. After the handshake each
  side knows the other's public key and that the other holds the private
  key. Session keys come from fresh ephemeral keys, so a recording cannot be
  decrypted later even if a long-term key leaks.
- **Records.** `u16 length | ciphertext`, at most 65535 bytes, each
  authenticated, in strict order. A changed, dropped, repeated or reordered
  record ends the connection.
- **Approval.** After handshake and greeting the host asks its user. The
  prompt shows the viewer's name, key fingerprint, whether that key was
  approved before, and a six-digit code. Until the user answers, the host
  captures nothing, sends nothing about the screen, and reads nothing the
  viewer sends. No answer within `security.approval_timeout_secs` (default
  30) is a refusal. Stopping sharing withdraws the request.
- **Comparison code.** Six digits derived from the handshake, the same on
  both screens. If someone relays the connection through their own keys,
  the two people see different codes.
- **Remembered devices.** `trusted.toml` holds public keys the user chose
  to remember. It only changes the prompt's wording ("you allowed this key
  on …"). **A remembered device is still asked for every time.**
- **Names** come from the other device and are only displayed: stripped of
  control characters, limited to 64 characters, inserted as text.

### Changes from the original plan

- **Comparison code instead of a typed pairing code (PAKE).** The plan
  called for a one-time code typed on the viewer. A code derived from the
  handshake and compared by the two people protects against the same
  attacker, needs no extra cryptographic protocol, and fits a product where
  the host must be present anyway. Its weakness is that it only works if
  people actually compare it. A typed code would be the right addition for
  unattended access, which this project does not have.
- **The viewer does not yet remember hosts.** It shows the host's key and
  code but keeps no list of its own.

## What this does and does not protect against

| Threat | Covered? |
|---|---|
| Someone on the network reading the screen or the input | Yes: everything after the handshake is encrypted |
| Someone on the network altering or replaying data | Yes: authenticated, ordered records |
| An unknown device starting a session | Yes: the host's user must allow it, every time |
| A device pretending to be one approved before | Yes: the key is proven; the name is not trusted |
| Someone in the middle relaying both sides | Only if the two people compare the code |
| A program running as the same Windows user on the host | No: it can read the key through DPAPI and drive the app |
| Someone at the unlocked host clicking Allow | No: that is the consent model |

## Acceptance tests

Run locally on 2026-11-13: `cargo test --workspace --no-fail-fast --
--include-ignored`, 123 passed, 0 failed. Tests marked "desktop" need a real
monitor and are `#[ignore]`d on CI.

| # | Test | How | Result |
|---|---|---|---|
| E1 | DPAPI round trip; ciphertext does not contain the secret; tampered or foreign data rejected | `protect::tests` | pass |
| E2 | Identity is created once and reloaded; the private key is not in the file in the clear; a damaged file is an error and is left alone | `identity::tests` | pass |
| E3 | Fingerprint has a fixed format and a known value for a known key | `fingerprint_format_and_known_value` | pass |
| E4 | Both sides learn the other's key and see the same code | `both_sides_learn_…` | pass |
| E5 | A flipped bit at every position of a record is rejected | `any_changed_bit_is_detected` | pass |
| E6 | Replayed, reordered and dropped records are rejected | `replayed_reordered_and_dropped_…` | pass |
| E7 | A record cannot be reflected to its sender or read by another session | `data_cannot_be_reflected_…` | pass |
| E8 | A tampered handshake fails | `a_tampered_handshake_fails` | pass |
| E9 | With a man in the middle the two codes differ | `a_man_in_the_middle_…` | pass |
| E10 | Large data is split into records and survives byte-by-byte delivery | `large_data_is_split_…` | pass |
| E11 | Remembered devices: persist, rename keeps the first date, forget, names cleaned, damaged file not overwritten | `trust::tests` | pass |
| E12 | Nothing readable crosses the wire | `nothing_readable_crosses_the_wire` | pass |
| E13 | Greetings fail for the same role or another protocol version | `greetings_fail_…` | pass |
| E14 | A peer that does not do the handshake is rejected | `a_peer_that_does_not_do_the_handshake_…`, `a_host_that_does_not_encrypt_is_refused` | pass |
| E15 | Approval: shows who is asking; deny; timeout means no; stopping withdraws it; new id each time | `share::tests::approval` (5) | pass |
| E16 | A declined viewer receives no session data and is not remembered; a stale answer is refused | `a_declined_viewer_gets_nothing_…` (desktop) | pass |
| E17 | An unanswered request is refused when its time runs out | `an_unanswered_request_…` (desktop) | pass |
| E18 | Allow + remember stores the key; next time it is recognised and still asked | `an_approved_and_remembered_viewer_…` (desktop) | pass |
| E19 | An unencrypted client gets under 200 bytes, no prompt, no frames; a proper viewer still gets through | `a_connection_that_is_not_encrypted_…` (desktop) | pass |
| E20 | Input sent before approval is not acted on | step 0 of `input_works_only_while_the_host_allows_it` (desktop) | pass |
| E21 | Stop returns within 2 s with a request pending or a silent viewer | `stop_returns_promptly_…` (desktop) | pass |
| E22 | Two app instances: both show each other's key; the same code on both screens; the viewer shows nothing while waiting | drive both windows | pass |
| E23 | Deny ends the viewer with "the host declined the request" | drive both windows | pass |
| E24 | Allow + remember starts the picture and lists the device; on reconnect the prompt says it was allowed before, and still asks | drive both windows | pass |
| E25 | Requests, answers and decisions are in the log, including one that timed out | log inspection | pass |

Measured in the two-instance run, release build, 1920×1080: 27.8 frames per
second and 12 ms from capture to display, the same as before encryption was
added. One run; the cost of encryption itself was not measured separately.

`input_works_only_while_the_host_allows_it` failed once in the first full
run with the pointer about 30 px from where the test had parked it, in a
direction the test never sends it, and passed three times in a row
afterwards. As in milestone 4, that is consistent with the mouse being
moved by hand during the test.

## Not done

- No rate limit on connection attempts: a program on the same PC can make
  the prompt appear repeatedly. Matters once the listener faces a network.
- The host does not notice a viewer leaving while its request is pending;
  the prompt stays until answered or timed out.
- The viewer keeps no record of hosts.
- Rotating or resetting a device's identity has no user interface.

## Next milestone

Milestone 6: finding devices and connecting to them — listening beyond
loopback, addressing a host by something better than a port number, and
discovery on the local network.
