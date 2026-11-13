# Milestone 9 — keeping a session healthy: keepalive and reconnect

## Scope

1. Notice a connection that has died without anyone saying goodbye.
2. Stop a relay from closing a session whose screen is simply not changing.
3. Show how the link is doing.
4. Get a lost connection back, without weakening the host's consent.

## Status

Complete as designed, tested in-process and with two real instances of the
app on **one** computer (loopback). **Not tested across two computers, over
a real flaky network, or across sleep/hibernate.** The performance numbers
below are a single spot measurement, not a benchmark.

| Part | Files |
|---|---|
| `Ping` / `Pong` messages, protocol v5 | `crates/rc-protocol/src/lib.rs` |
| Link monitor: pings, silence detection, round-trip time | `apps/desktop/src/link.rs` |
| Used by host and viewer; "lost" is told apart from "ended" | `apps/desktop/src/share.rs`, `viewer.rs` |
| Reconnect policy (plain functions, tested with node) | `apps/desktop/ui/reconnect.js` |
| Reconnect flow and link display in the window | `apps/desktop/ui/session.js` |

## How it works

**Keepalive.** Once a session is allowed, each end pings the other four times
a second (every 5 s when this was first written; see "Keeping up with the
connection" for why it changed)
and answers the other's pings. If *nothing at all* (a ping, a pong, video,
input) arrives for 15 s, the link is declared dead and the connection is
closed, so the session ends with a reason instead of hanging. Nothing is
sent and nothing times out while a viewer is still waiting for the host's
approval; the person at the host may take as long as the approval timeout.

Each ping is about 31 bytes on the wire (from the record format, not
measured), so about 1 kbit/s in each direction.

**Two threads, on purpose.** One thread only watches the clock and decides;
another sends the pings. A write to a dead connection can block until the
write timeout (5 s without progress), and the watcher must not be stuck
behind it. Measured: shutting a socket down does *not* end a write that is
already blocked on Windows; the write timeout does (a stuck sender was freed
4.6 s after the link was declared dead). So the worst case for noticing a
dead link is the 15 s silence limit, plus up to 5 s for a write that was
already stuck.

**Relays.** The pings are traffic in both directions, so a relay no longer
sees a still screen as idle. Milestone 7 listed this as a limit (a still,
relayed session ended after 15 minutes); it is fixed. Test: a relay that
closes after 1.2 s of silence carried a still-or-moving session for 4.5 s.
With pings turned off the same test fails ("the host lost the viewer"), so
the test does show the effect.

**Keeping up with the connection.** Added after the network tests (see
`NETWORK-TESTS.md`) showed a link slightly too slow leaving the picture 26 s
behind. The host cannot see the buffers on the way, but its pings wait in
them behind the video, so their round trip shows the wait. Rule, in
`apps/desktop/src/congestion.rs`:

- Queueing delay is the round trip (or how long the oldest unanswered ping
  has waited) minus the lowest round trip of the last 10 s.
- Above 300 ms: stop sending video until it is back under 100 ms, and aim
  the encoder at a lower bit rate (cut to 70%, then halved for each further
  second of waiting, never below 150 kbit/s, and never above the maximum set in the settings even when that is lower).
- Under 100 ms: raise the bit rate again, quickly back to just under where
  the trouble was, gently beyond it, quickly again after a clear spell.
- A session starts at 30% of the maximum and works up.

Frames are skipped before they are encoded, so the viewer never misses a
frame the next one depends on. The host's window says when it is doing this
("Video limited to 0.4 of 8.0 Mbit/s for this connection", "Holding video
back"). `video.adaptive_quality = false` turns it off.

**Lost, not ended.** The window is told one of two things:

- `ENDED`: someone ended it on purpose (a goodbye from either side, the host
  declining, sharing stopped). Never retried.
- `LOST`: the connection broke without a goodbye (the process died, the link
  went silent, a write failed). The window may reconnect.

**Reconnect.** After `LOST` the window tries again after 2, 4, 8, then every
15 seconds, for up to 120 s, showing a countdown and a **Stop reconnecting**
button. Consent is unchanged:

- Every attempt is a **brand new session**. The host asks its user again,
  with a new prompt, every time. There is no "resume" shortcut and nothing is
  carried over (clipboard sync switches off, transfers in progress are gone).
- The device must still hold the key that was asked for. If it does not
  ("not the one you picked"), or does not speak the secure protocol, attempts
  stop and are never retried.
- Only "could not reach it" failures are retried: refused connection, server
  unreachable, a host that accepts but is too slow to finish the handshake.
- If the host answers "busy with another viewer" or "try again in a moment"
  (its old session for this viewer may not have been noticed as dead yet),
  that is retried. "Declined" and "did not answer" are final.

**Display.** Both windows show the round trip to the other end. If the other
end has been quiet for more than 6 s the line says so ("No answer from the
host for 8 s") before the 15 s verdict.

## Tests

| Where | What it shows |
|---|---|
| `link` (7) | Round trip measured at both ends; nothing sent or timed out until armed; a silent peer is declared dead in 0.4–3 s and both ends see the connection close; any traffic counts as alive; pongs with unknown numbers ignored; dropping the monitor stops the pings; a writer stuck on a peer that never reads is freed within the write timeout |
| `viewer` (5 new, fake host) | Host silent after allowing → `LOST` with a reason; host vanishes without goodbye → `LOST`; goodbye → `ENDED`; waiting for approval never times out; a host that only answers pings keeps a still session alive for 3× the timeout |
| `share/link_tests` (2, real sessions) | A viewer that stops answering is dropped in 0.75 s and the host keeps sharing; both ends see round trips and a session outlives the timeout |
| `rendezvous` (1, real relay) | Pings keep a still relayed session open past the relay's idle limit |
| `ui/reconnect.test.js` (5, node) | Wait schedule, give-up limit, what is retried and what never is (including a different device), busy host vs. refusal |

Whole workspace, with the desktop tests (`--include-ignored`): 253 passed in
three of four runs. In the fourth, `input_works_only_while_the_host_allows_it`
failed once at `share/tests.rs:492` (the pointer was not at the centre 30 ms
after an injected move). It passed 6 of 6 when run alone and in the next two
full runs, so I could not reproduce it. It moves the real mouse pointer, so a
real mouse movement in that window would do it; **that is a hypothesis, not
confirmed.** `cargo fmt --check` and clippy (`-D warnings`) were clean. The
node tests run in CI.

Added with the rate control: `congestion` (7, against a simulated path:
a slow path stays under 0.4 s behind once settled while about 95% of its
capacity is used, the rate returns when the path recovers, a path that gets
longer is not mistaken for a full one, limits and hold marks); `rc-codec` (3:
the bit rate changes while encoding, it causes no keyframe, an encoder
created low can be raised); `link` (1: the baseline is the lowest *recent*
round trip). Whole workspace after that: 264 passed, twice in a row.

## Measured in a live run (two app instances, one computer)

- **Host process killed**, then restarted on the same port: the viewer said
  "Connection lost", counted down, reconnected, the host showed a **new
  approval prompt**, and after it was allowed the picture resumed at about
  28 frames per second. (When it was *not* approved within 30 s the viewer
  ended with "the host did not answer" and stopped trying, as designed.)
- **Host frozen** (process suspended, so no connection-closed signal ever
  arrives): the viewer showed "No answer from the host for 8 s", declared the
  link lost at 15 s, then retried (the frozen host accepts connections but
  cannot finish the handshake, which is retried). After resuming the host a
  prompt appeared within 5 s of the next attempt and the session came back
  after approval.
- **Spot measurement**, loopback, 1920×1080, 20 s with a changing screen:
  host process about 44% of one core, memory flat at 133 MB; viewer process
  about 2% of one core and 39 MB (the video decoding happens in the web
  view's own processes, which this did not count); capture-to-display about
  11–16 ms; round trip 0.3 ms. This says nothing about a real network.
- The test apps were closed afterwards and nothing was left listening.

## Not covered

- **Session state is not restored.** A reconnect is a new session; a file
  transfer that was running is lost and must be offered again.
- **Reconnecting by device ID after the host restarted** was not run live.
  A host started with the default "choose any port" listens on a different
  port each time, so reconnecting to a typed `address:port` fails by design;
  the live run used a fixed port. By ID through the server it should find the
  host again after it re-registers, but that path was only covered by the
  existing Milestone 7 tests, not by a reconnect test.
- **Sleep, hibernate and network changes** (Wi-Fi to cable) were not tried.
- **Under a bandwidth cap the video rate is cautious and uneven.** In the
  live runs it used between about a third and nearly all of a 0.8 Mbit/s
  link (0.21, 0.29, 0.40 and 0.71 Mbit/s in four 10 s samples). I did not
  establish why it varies; large keyframes every 4 s are a likely cause, not
  a confirmed one. The picture stays close to live either way.
- **The encoder's bit rate is a target, not a limit.** On very busy pictures
  the Windows software encoder overshoots it by a wide margin (a synthetic
  worst case produced over 30 Mbit/s whatever was asked for). Holding frames
  back is what bounds the lag then, at the cost of frame rate.
- **Delay with jitter is still poor**: about 13 frames per second and 0.4 to
  1.3 s behind at 100 ms delay with 20 ms jitter. netem's jitter reorders
  packets, which TCP handles badly; a real path may behave better.
- **Tuning.** The 300/100 ms marks, the step sizes and the 10 s baseline are
  my choices, checked against a simulation and three live scenarios, not
  tuned on real links.
- **Two computers, a real network, packet loss, high latency:** not tested.
  The 15 s / 5 s timings are my choice, not tuned on real links.
- Still open from Milestone 7: no TLS/WebSocket wrapping for networks that
  allow only web traffic, no hole punching, an open server with no
  allow-list.

## Next milestone

Milestone 10 is for you to define. Candidates from what is still open:
packaging (installer, update story), a security review of the whole design,
user documentation, and the network gaps above (TLS/WebSocket, hole
punching).
