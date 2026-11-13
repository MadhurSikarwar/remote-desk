# Milestone 6 — discovery and connecting across a network

## Scope

1. Let a host accept connections from other computers, as an explicit
   choice.
2. Let a viewer find sharing devices on the local network.
3. Connect to a device by address, and refuse one whose key is not the one
   that was found.
4. Make the host robust now that anyone on the network can reach it.

Works on one local network (same subnet or reachable by broadcast). Getting
**between** networks — through routers, NAT and firewalls — is milestone 7.

## Status

Complete for the local network. Not tested between two physical computers
(see "What was and was not tested").

| Part | Files |
|---|---|
| Setting `network.allow_remote_connections` (off by default) | `crates/rc-core/src/config.rs` |
| Discovery protocol, responder, scan | `crates/rc-net/src/lib.rs` |
| Network listener, parallel connections, busy handling | `apps/desktop/src/share.rs` |
| Connect by address, key pinning | `apps/desktop/src/viewer.rs` |
| Address field, device list, network warning | `apps/desktop/ui/` |

## Behaviour

**Off by default.** With the setting off, sharing listens on 127.0.0.1 and
nothing else is reachable or discoverable. With it on, sharing listens on
all IPv4 interfaces, the window says so and shows the address other
computers should use, and the log records a warning.

**Every viewer is still encrypted and still needs the Allow click.** Opening
the listener changes who can *ask*, not who is *let in*.

**Discovery.** While sharing is on, remote connections are allowed and
`network.lan_discovery` is on, the host answers queries on UDP port 47821.
Nothing is announced unprompted, and a device that is not sharing sends
nothing. A scan sends its query out of every network interface to that
interface's broadcast address, and lists each device once, at its best
address.

What a reply contains: the device's name, public key and TCP port. What it
means: *try here*. Replies are unauthenticated, so the viewer connects, the
handshake proves which key is really there, and the connection is dropped if
it differs from the key in the reply. Names are claims, never proof.

**Abuse limits.**

- A query must be at least 128 bytes, longer than any reply, so a forged
  query cannot make a device send more data to a victim than was sent to it.
- At most 20 replies a second.
- At most 8 connections being set up at once; each is cut off after 5
  seconds if it does not finish the handshake. A connection that says
  nothing does not delay anyone else.
- One viewer at a time is asked or shown the screen; others are told the
  host is busy.
- After the user refuses a viewer (or lets a request time out), the next one
  is told to try again in 3 seconds, so a device on the network cannot keep
  the prompt on screen.

## Problems found and fixed

1. **The accept loop shared a thread with approval.** While one viewer was
   being asked, nobody else could even connect: they hung instead of being
   told "busy". Found by the new busy-host test; accepting now has its own
   thread.
2. **A single broadcast reaches one adapter only.** On the test machine,
   with a link-local adapter and a campus network adapter, discovery found
   the host only through loopback and the link-local adapter, never at its
   real LAN address. Scans now go out of every interface.
3. **Pointer tests depended on the mouse staying still.** Twice more a test
   failed because the real mouse was moved during it. The pointer test now
   retries each position, and the control test checks that the pointer did
   not reach the target instead of expecting an exact parked position.

## What was and was not tested

Tested (release build, this computer, real network adapter):

- The host listens on `0.0.0.0` (TCP) and UDP 47821 when allowed, and on
  `127.0.0.1` only otherwise (test `other_computers_can_connect_only_…`).
- Discovery lists the host once, at its LAN address `172.17.12.212`; the
  viewer connected from that list; the host logged the connection as coming
  from `172.17.12.212`, not loopback; the prompt and the viewer showed the
  same code; after Allow the picture ran at 26 frames per second with 13 ms
  from capture to display.
- After the apps closed, no listening TCP socket and no UDP 47821 endpoint
  remained.

**Not tested:** a second physical computer. Both ends were on one PC,
talking through its network address. That exercises the network listener,
broadcast discovery on the real adapter and addressing, but not a real
second host, the Windows Firewall's treatment of remote peers, or a
different network equipment. Whether Windows showed a firewall prompt for
the application was not observed.

## Acceptance tests

Run locally on 2026-11-13: `cargo test --workspace --no-fail-fast --
--include-ignored`, 149 passed, 0 failed. Tests marked "desktop" need a real
monitor and are `#[ignore]`d on CI.

| # | Test | How | Result |
|---|---|---|---|
| F1 | Setting defaults to off and round-trips | `rc-core` config tests | pass |
| F2 | Queries and replies: format, padding, truncation at every byte, trailing bytes, bad names, bad ports | `rc-net` tests | pass |
| F3 | No reply is larger than the smallest accepted query | compile-time assertion, `no_reply_is_ever_larger_…` | pass |
| F4 | A short query gets no answer; a proper one does | `a_short_query_gets_no_answer` | pass |
| F5 | Rate limit allows a burst, then recovers | `the_rate_limit_…` | pass |
| F6 | A responder is found, and stops answering when dropped | `a_responder_is_found_…` | pass |
| F7 | Two instances on one computer can share the discovery port | `two_responders_can_share_a_port_…` | pass |
| F8 | Broadcast addresses from address and mask; a route per interface; one entry per device at its best address | `rc-net` tests | pass |
| F9 | Addresses typed by the user are understood; bad ones refused with a reason | `addresses_are_understood`, `bad_addresses_…` | pass |
| F10 | A host with a different key than expected is refused and no connection is left; the right key is let through | `viewer::tests` | pass |
| F11 | Loopback only unless allowed; reachable at the LAN address and still asks when allowed | `other_computers_can_connect_only_…` (desktop) | pass |
| F12 | Discoverable only while sharing, with remote and discovery on; gone when sharing stops | `a_sharing_device_is_discoverable_…` (desktop) | pass |
| F13 | A second viewer is told the host is busy; the first is unaffected | `a_second_viewer_is_told_…` (desktop) | pass |
| F14 | After a refusal the next viewer is asked to wait, then asked again | `after_a_refusal_…` (desktop) | pass |
| F15 | A silent connection does not delay a real viewer | `a_stalled_connection_…` (desktop) | pass |
| F16 | More than 8 simultaneous connections are refused, and it recovers | `too_many_simultaneous_…` (desktop) | pass |
| F17 | Two app instances over the LAN address: discover, connect from the list, approve, picture | drive both windows | pass |
| F18 | No listening sockets remain after the apps close | `Get-NetTCPConnection`, `Get-NetUDPEndpoint` | pass |

## Not done

- IPv6 (listener, discovery and addresses are IPv4).
- No Windows Firewall rule is created; the user's system decides, and the
  installer (milestone 10) is the place to offer one.
- The viewer keeps no list of hosts it has connected to before.
- A device on the network can still make the prompt appear every 3 seconds
  by presenting new keys; each costs the user a click, or stopping sharing.
- Discovery does not cross routers.

## Next milestone

Milestone 7: connecting when the two devices are on different networks —
a rendezvous server so that devices can find each other by identity, direct
connection through NAT where possible, and a relay that forwards the
encrypted stream where it is not.
