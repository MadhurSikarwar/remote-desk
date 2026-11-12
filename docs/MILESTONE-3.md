# Milestone 3 — encode, transmit, decode, render

## Scope

1. `rc-codec`: BGRA → NV12 conversion and H.264 encoding.
2. `rc-protocol`: framed wire messages.
3. Host session: capture → encode → send to one viewer.
4. Viewer session: receive → decode → draw in the window.

**Restriction: this computer only.** The stream is not encrypted yet, so the
host listens on 127.0.0.1 and the viewer connects to 127.0.0.1. Nothing can
cross a network until milestone 5 adds the encrypted channel. Two instances
of the app on one PC are enough to exercise the whole pipeline.

## Status

Complete, with one open problem (application exit, see the end).

| Part | Files |
|---|---|
| Colour conversion | `crates/rc-codec/src/nv12.rs` |
| H.264 encoder | `crates/rc-codec/src/mf_encoder.rs` |
| Wire messages, incremental decoder | `crates/rc-protocol/src/lib.rs` |
| Host session | `apps/desktop/src/share.rs` |
| Viewer session | `apps/desktop/src/viewer.rs` |
| Decoder and canvas | `apps/desktop/ui/session.js` |

`apps/desktop/src/capture.rs` from milestone 2 is gone: the capture-only test
panel was replaced by real sharing, which reuses its worker and pacing code.

## Design

```
host:   DXGI capture → BGRA→NV12 (BT.709, limited range) → H.264 encoder
        → VideoFrame message → TCP 127.0.0.1
viewer: TCP → message decoder → packet to the window (Tauri channel, raw bytes)
        → WebCodecs VideoDecoder → canvas
```

- **Encoder.** Microsoft's software H.264 encoder (Media Foundation), Main
  profile, CBR, low-latency mode, a keyframe at least every 4 s and on
  request. One input frame gives one output frame, as Annex B.
- **Protocol.** `u32 length | u8 type | payload`, little endian, 16 MiB
  message limit, version checked in a `Hello` exchange. Hand-written rather
  than protobuf: five messages do not justify a code generator that needs
  `protoc` installed. To be revisited if the message set grows.
- **Sessions.** One viewer at a time. Capture and encoding run only while a
  viewer is connected; "sharing on" with no viewer costs nothing. A new
  viewer always starts with a keyframe.
- **Threads, not async.** Capture and encoding block by nature, so each
  session is a pair of plain threads. Tokio arrives with the network
  transport in milestones 6–7.
- **Decoding.** WebView2's WebCodecs `VideoDecoder`, which uses the GPU when
  it can. If more than 8 frames are queued the viewer drops to the next
  keyframe instead of building up delay.
- **Latency.** Each frame carries its capture time; the viewer subtracts it
  from the time the frame is drawn. Valid only while both ends share a clock,
  which is the case on one PC.

## Authorization and visibility

- Sharing starts only from the Start sharing button.
- The header badge shows "Sharing on — no viewer" (blue) or "Sharing screen"
  (red) the whole time.
- Start, stop, each viewer connection and its end are logged.
- There is no approval prompt for an incoming viewer yet. Until milestone 5
  the only possible viewer is a program on the same PC under the same login.

## Problems found and fixed

1. **Tinted white.** `black_white_and_grey_have_neutral_chroma` failed: white
   converted to Cb 127 instead of 128. The blue-difference coefficients did
   not sum to zero. Corrected (−26, −86, 112).
2. **"Stop sharing" froze the host.** After Stop the host window kept showing
   "Sharing screen" and ignored clicks until the viewer app exited (15 s in
   the log). Cause: the host waited for its reader thread, which sat in a
   blocking socket read. On Windows, shutting a socket down does not wake a
   read on another thread, and the viewer kept its socket open after the
   session ended. The command also ran on the UI thread. Fixes:
   - readers poll with a 200 ms timeout and an incremental message decoder
     (`poll_messages`, `MessageDecoder`), so they stop without help from the
     peer;
   - the viewer closes its socket when a session ends;
   - blocking commands run off the UI thread.
   Covered by `stop_returns_promptly_while_a_silent_viewer_stays_connected`,
   `disconnect_returns_promptly_from_a_silent_host` and
   `the_viewer_closes_its_socket_when_the_host_says_goodbye`.

## Measurements

Machine: AMD Radeon integrated graphics, one 1920×1080 display at 125%
scaling, Windows 11. Release builds. Settings: 30 fps, 8000 kbit/s. Single
runs, not averaged.

| Run | Result |
|---|---|
| `encode_bench`, 10 s of the real screen | 300 frames, 30.0 fps; BGRA→NV12 3.94 ms per frame; encode 3.78 ms per frame (slowest 15.3 ms); 2065 kbit/s on a mostly static screen |
| Same stream checked with ffmpeg | ffprobe: H.264 Main, 1920×1080, yuv420p, 300 frames; decoded with 0 errors; a decoded frame was inspected and correct |
| Two instances, viewer watching for 30 s | 898 frames received, 898 drawn, 0 skipped, 0 decode errors; 30.1 fps; 13.2 ms average capture → display |
| Same session, host side | 889 frames, 20.6 MB sent ≈ 5.5 Mbit/s |
| Second run, 20 s | 493 frames, 13.8 ms average capture → display |
| CPU, that 20 s run (about 21 s between samples) | host process 16.0 s CPU ≈ 76% of one core; viewer process 0.86 s ≈ 4% of one core |

Notes on these numbers:

- The viewer window was itself on the shared screen, so the picture changed
  continuously.
- 13 ms is loopback, where the network adds nothing. It measures capture,
  conversion, encoding, decoding and drawing, not what a real network will
  give.
- Viewer CPU excludes the WebView2 helper processes, where decoding runs.
- The host's CPU cost is the software encoder, which uses several threads.
  It is the main performance problem and the reason hardware encoding is
  planned for milestone 9.
- The stream carries no colour-range/matrix signalling (ffprobe reports
  both as unknown). Decoders assume BT.709 for HD, which matches, and the
  inspected frames looked right, but it is not explicit.

## Acceptance tests

Run locally on 2026-11-12: `cargo test --workspace -- --include-ignored`,
65 passed, 0 failed. Tests marked "desktop" need a real monitor and are
`#[ignore]`d on CI.

| # | Test | How | Result |
|---|---|---|---|
| C1 | Colour conversion: neutral greys, BT.709 primaries, per-pixel luma, block-averaged chroma, stride, odd sizes rejected | `nv12::tests` (6) | pass |
| C2 | Encoder: invalid settings and wrong frame size rejected | `encoder.rs` (2) | pass |
| C3 | Encoder: one output per input, first is a keyframe with SPS, PPS and IDR, timestamps increase | `emits_one_frame_per_input_starting_with_a_keyframe` | pass |
| C4 | Encoder: a requested keyframe arrives on the next frame | `a_requested_keyframe_arrives_on_the_next_frame` | pass |
| C5 | Protocol: round trip, stable layout, every truncation rejected, trailing bytes, unknown types, bad fields, oversized and zero lengths | `rc-protocol` tests (12) | pass |
| C6 | Protocol: messages reassembled from single bytes and from one large push | `decoder_*` | pass |
| C7 | Handshake: matching peer accepted; other versions, roles and messages refused | `host_handshake_*`, `viewer_handshake_*` | pass |
| C8 | Host listens on loopback, refuses a second start, releases the port on stop | `sharing_listens_on_loopback_only…` (desktop) | pass |
| C9 | A viewer receives the config and a first keyframe; the host returns to waiting after it leaves | `a_viewer_receives_the_shared_screen` (desktop) | pass |
| C10 | Stop and disconnect return within 2 s against a silent peer | three tests named above | pass |
| C11 | An independent decoder accepts the stream | ffprobe + ffmpeg on `encode_bench` output | pass |
| C12 | Two app instances: the viewer shows the host's screen, live | drive both windows; screenshot inspected | pass |
| C13 | Host Stop ends the viewer with "sharing stopped" and the host window updates at once | drive both windows | pass |
| C14 | Connect with sharing off fails with a message; restart and reconnect work; viewer Disconnect returns the host to waiting | drive both windows | pass |

Not tested: a decoder error and the keyframe-request recovery that follows
it (the code path exists; no error occurred in any run), and the
queue-overflow skip.

## Open problem: the window sometimes does not close

Twice in roughly fifteen runs, an instance ignored a request to close its
window. The process stayed alive and its page kept running. Both times it
followed a long measurement run driven through the debugging port; the first
time was in milestone 2, before any of this session code existed.

In the second case the viewer's log showed the session had ended, but its
page never received the final packet. That suggested the exit handler
(on the UI thread) waiting for the viewer thread while that thread waited
on the UI thread. Deliberate attempts to reproduce this — closing while
viewing, closing the host first, closing both at once (five times) — all
exited normally, so the cause is **not confirmed**.

What was done:

- the exit path no longer waits for the viewer thread (`abandon`);
- window close, destroy and exit requests are now logged, so the next
  occurrence shows how far the close got.

This is hardening against a suspected cause, not a verified fix.

## Next milestone

Milestone 4: mouse and keyboard from the viewer to the host, off by default
and allowed only when the host has enabled it.
