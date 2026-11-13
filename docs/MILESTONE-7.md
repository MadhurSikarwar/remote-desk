# Milestone 7 — reaching devices on other networks

## Scope

1. A server that devices register with, so a viewer can ask for one by ID.
2. A direct connection when the device can be reached.
3. A relay through the server when it cannot.
4. Nothing about the server needs to be trusted with the content of a
   session or with deciding *which* device a viewer reaches.

## Status

Complete as designed, and tested in-process and on one computer with the
real server binary. **Not tested across two computers, a real NAT, or the
internet** (see "What was and was not tested"). Two gaps matter for
"works on every network" and are listed under "Not covered".

| Part | Files |
|---|---|
| Device ID helpers, server key file | `crates/rc-crypto/src/identity.rs` |
| Messages between devices and server | `crates/rc-rendezvous/src/lib.rs` |
| The server (library and `rc-server` program) | `apps/server/` |
| Registration, direct-then-relay connecting | `apps/desktop/src/rendezvous.rs` |
| Host takes relayed connections; viewer connects by ID | `apps/desktop/src/share.rs`, `viewer.rs` |
| Settings and window | `crates/rc-core/src/config.rs`, `apps/desktop/ui/` |

## How it works

A device's **ID** is its key fingerprint (the 16 hex digits already shown as
"Device key"). It is derived from the key, so a server cannot hand out or
reassign IDs, and nothing about IDs is stored on the server.

```
host ── encrypted control channel ──► server      registers: "I am 8F7C-…, port 17951"
viewer ─ encrypted control channel ─► server      asks: "connect me to 8F7C-…"
        ◄── where to try directly ───            host is told someone asked

viewer ── direct TCP ──────────────► host         tried first
   │ (refused, unreachable, or the wrong device)
   └─ asks the server for a relay
host ──► server ◄── viewer                        both open a connection to the server
              └─ joins them, copies bytes ─┘      carrying the usual end-to-end encrypted session
```

- **One port.** Registration and relaying share a TCP port. A connection
  that begins with the relay marker is a relay; anything else is a device's
  control channel. One firewall rule, such as 443, is enough.
- **Control channel.** Noise handshake with the server, then encrypted
  messages. A device refuses a server whose key does not match the
  fingerprint set in its settings.
- **Relay.** A new connection that starts with a marker and a 32-byte
  one-time token given to both sides over their control channels. The server
  waits up to 30 s for the second side, then copies bytes both ways. The
  token works once.
- **End to end.** Through the relay the two devices run their normal
  handshake and approval. The server carries ciphertext and cannot read or
  alter it.
- **Who is at the other end.** After the handshake the viewer checks that
  the key it reached has the ID it asked for. If a direct attempt reaches
  some other device, it is turned away (no prompt appears there) and the next
  way is tried. A server that routes a viewer to the wrong device therefore
  gets a refusal, not a session.
- **Which route.** The window shows how the host was reached ("directly" or
  "through the server"). The settings can force either: *direct only* or
  *relay only*.
- **Consent.** A device registers only while sharing is on **and** "Let
  other computers connect" is on **and** a server is set. Stopping sharing
  unregisters it at once. The approval prompt is unchanged: every viewer,
  however it arrived, needs an Allow click.

### What the server learns and can do

| | |
|---|---|
| Sees | Which keys register, from which addresses, who asks for whom and when, how many bytes a relay carries |
| Cannot do | Read or change a session; make a viewer connect to a device other than the one it asked for; start a session without the host's approval |
| Can do | Refuse service, drop or delay connections, log the above |

### Server limits (defaults)

4096 connections, 32 per address, 10 000 registered devices, 512 relays,
10 connect requests per device per minute, 5 s to finish a handshake, 90 s of
silence on a control channel, 30 s for a relay to be joined, 15 min of
silence on a relay. Nothing is stored on disk except the server's own key.

### Running it

```
rc-server --listen 0.0.0.0:21117 --data-dir ./data
```

The first run creates `data/server.key` and prints `Server key: XXXX-XXXX-XXXX-XXXX`.
Users put `host:21117` and that fingerprint in Settings → Network. The key
file is **not encrypted**; protect the directory. A Docker image and a
walk-through are part of milestone 10.

## Problems found and fixed

None surfaced by testing in this milestone. Designs that were changed
before testing, for reasons found while writing:

- **IDs.** A server-assigned numeric ID (as in the plan) would let a server
  squat or reassign IDs. A key-derived ID needs no server state and is
  checked by the viewer.
- **Relay on the same port, TLS/WebSocket deferred.** See "Not covered".
- **One server entry.** The earlier `relay_servers` setting was removed: the
  relay is part of the one server.

## What was and was not tested

Tested:

- The server with real encrypted clients (20 tests): registration, connect
  requests, replacing a registration, cancelling requests, relay pairing,
  byte-exact forwarding of 200 kB each way, single-use tokens, unknown
  tokens, unjoined relays timing out, idle relays closed, relay capacity,
  per-address limit, request rate limit, protocol version check, registry
  size limit, garbage and unencrypted peers, quiet and pinging devices,
  shutdown.
- A full end-to-end handshake between two devices *through* a relay, each
  learning the other's key and sharing the same code.
- The device side (10 tests): server key mismatch refused; direct; relay;
  relay-only never touches the host's listener; direct-only refuses to
  relay; a wrong device at the direct address turned away and the relay
  used; the error listing what was tried; unknown ID; server outage noticed
  and registration resumed when the server returned.
- Whole sessions through the server (7, on a real monitor): picture, approval
  and code agreement for direct, relayed and relay-only; a different device
  at the advertised address is neither prompted nor approved; direct-only
  fails without relaying; unknown ID and no server configured say so;
  registration only while sharing with other computers allowed.
- The real `rc-server` program with two app instances (release builds):
  registration, connecting by ID directly, then relay-only. The relayed
  session ran, and the server logged `relay ended bytes=302292 seconds=8`.

**Not tested:**

- Two computers, a real NAT, a router, or the internet. Everything ran on
  one PC, so "direct" meant a loopback connection and the relay was
  loopback-to-loopback. The logic for each path is exercised, including the
  failure and fallback paths, but not the networks that would trigger them.
- Latency and throughput over a real wide-area relay. The 12 ms measured
  with the relay is loopback and says nothing about the internet.
- A server under real load, or a long-running server.
- Whether the Windows Firewall prompts for the server or the app.

## Not covered (read this for "all networks")

1. **No NAT hole punching.** A device is reached directly only if the
   viewer can connect to it: same network, a public address, or a port
   forward. Two devices each behind their own home router will fall back to
   the relay every time. That always works but costs the server's bandwidth
   and adds a hop. UDP/QUIC hole punching is the next step for a direct path
   in that case.
2. **Only plain TCP.** The control channel and relay are raw TCP with the
   Noise protocol. They pass any network that allows outbound TCP to the
   server's port (use 443 where possible), but a network that only allows
   web traffic through an HTTP proxy, or inspects for TLS, will block them.
   Wrapping the connection in TLS/WebSocket is not done.
3. **A relayed session with a perfectly still screen and no input ends after
   15 minutes**, because the session itself has no keepalive. Fixing it
   belongs with reconnect handling (milestone 9).
4. **Open server.** Anyone who can reach the server and has its address can
   register and use the relay; there is no allow-list or accounts, so a
   public server would carry strangers' traffic. Fine for a private server
   behind a firewall rule.
5. **Server key not encrypted at rest**; IPv4 only; one server per device.

## Acceptance tests

Run locally on 2026-11-13: `cargo test --workspace --no-fail-fast --
--include-ignored`, 197 passed, 0 failed, run twice. Tests marked "desktop"
need a real monitor and are `#[ignore]`d on CI.

| # | Test | How | Result |
|---|---|---|---|
| G1 | Fingerprints parse as typed (case, dashes, spaces) and malformed ones are refused; the server key file is created once and never replaced | `rc-crypto` tests | pass |
| G2 | Message format: round trip, every truncation, trailing bytes, bad flags and text, address count limit | `rc-rendezvous` tests (7) | pass |
| G3 | A relay hello cannot be mistaken for an encrypted record | `the_relay_hello_cannot_…` | pass |
| G4 | The server proves its key; registration and connect requests tell both sides what they need | `server` tests | pass |
| G5 | Replacement, unregistering and cancelled requests | `server` tests | pass |
| G6 | Only the requesting viewer may ask for a relay, once | `only_the_requesting_viewer_…` | pass |
| G7 | Relay forwards 200 kB each way unchanged; the token works once | `a_relay_carries_bytes_…` | pass |
| G8 | Two devices handshake through a relay and see each other's keys | `two_devices_can_run_…` | pass |
| G9 | Unknown token, unjoined relay, idle relay, relay capacity | `server` tests | pass |
| G10 | Connection, request-rate, registry limits; version check; garbage input; idle and pinging devices; shutdown | `server` tests | pass |
| G11 | Server with a different key than configured is refused (viewer and host) | `a_server_with_another_key_…` | pass |
| G12 | Direct when reachable (server carries nothing); relay when not; relay-only skips direct; direct-only fails instead of relaying | `rendezvous::tests` | pass |
| G13 | A wrong device at the direct address is turned away and the relay used | `rendezvous::tests` + `a_different_device_at_the_advertised_address_…` | pass |
| G14 | Server outage is reported; registration resumes when it returns | `an_unreachable_server_is_reported_…` | pass |
| G15 | Whole sessions through the server (direct, relayed, relay-only) with matching codes and approval | `rendezvous::tests::sessions` (desktop) | pass |
| G16 | Registration only while sharing with other computers allowed; unregistered when sharing stops | `a_device_registers_only_while_…` (desktop) | pass |
| G17 | Real `rc-server` with two app instances: connect by ID directly, then relayed; server log shows the relay | drive both windows | pass |
| G18 | Nothing left listening after the apps and server stop | `Get-NetTCPConnection` | pass |

## Next milestone

Milestone 8: clipboard synchronisation and file transfer, each off until
the host allows it and visible while in use.
