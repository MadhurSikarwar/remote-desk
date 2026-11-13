# Network tests on one computer

Two computers were not available, so the relay and the network paths were
exercised with what one Windows PC has: a Linux relay server in WSL (a real
virtual network card, a separate network stack), `tc netem` in WSL to damage
that path, and the PC's own Wi-Fi address. Run on 2026-11-13: first with the
release build of commit 0669411 ("Results"), then again after the fix the
first run led to ("After the fix").

**What this is not.** The host and the viewer are still two processes on the
same Windows machine, behind no NAT and no router. It says nothing about NAT
traversal, a home router, a firewall between two machines, or a real ISP.
Every number below is one run; none is a benchmark.

## Setup (to repeat it)

1. In Ubuntu on WSL: install Rust with rustup (user level), copy the source
   (`git archive HEAD | wsl -d Ubuntu -- tar -x -C ~/rc/src`), build with
   `cargo build --release -p rc-server --locked`, run
   `rc-server --listen 0.0.0.0:21117 --data-dir ~/rc/data`. It prints its key
   fingerprint.
2. Host settings: `rendezvous_servers = ["<WSL address>:21117"]`,
   `server_fingerprint = "<printed>"`, `allow_remote_connections = true`.
   Viewer: the same server, `transport = "relay-only"`.
3. Windows reaches the WSL address over the `vEthernet (WSL)` adapter. The
   viewer connects by the host's device ID.
4. Damage the path with `wsl -u root -- tc qdisc replace dev eth0 root netem
   <options>`; remove it with `tc qdisc del dev eth0 root`. netem acts on
   what the server *sends*, which on a relay is both directions of the
   session.

## Results

| Test | Result |
|---|---|
| Host registers with the Linux server; viewer connects by ID, relay-only | Worked: route "through the server", approval prompt, 30 fps, round trip about 1.5–2 ms (0.3 ms on loopback) |
| Fixed delay of 100 ms each way (about 200 ms round trip), 40 s | Stable: 30 fps, 111 ms capture to display |
| Delay 100 ms with 20 ms jitter | 27 fps and 154 ms at first; after about 20 s 9 fps and 3.9 s of lag, round trip 967 ms; recovered a few seconds after the damage was removed. netem's jitter reorders packets, which real paths rarely do, so this may overstate what a real jittery link does; I did not separate the causes |
| 30 ms delay with 2% random loss, 40 s | 27–30 fps, 51–126 ms |
| Cap of 3 Mbit/s with 20 ms delay on a mostly still screen | Degraded for about 10 s (15 fps, 252 ms), then 30 fps, 38 ms; the video needs far less than 3 Mbit/s here |
| Moving test pattern, no limit | Video used about 1.3 Mbit/s |
| Cap of 0.8 Mbit/s (below that) with 20 ms delay, moving pattern | **Fell far behind:** 7.8 s of lag after 10 s, round trip 17 s after 25 s and 26 s after 40 s. The link never counted as dead, because data still arrived, so nothing reacted. Back to 30 fps and 14 ms about 8 s after the cap was removed |
| All traffic dropped (100% loss), about 30 s | Both ends noticed: the host dropped the viewer and went back to waiting after about 12–15 s; the viewer's window said "No answer from the host" by the 12 s sample, declared the link lost (after 15 s of silence) and retried. My samples were several seconds apart, so these times are approximate. After the loss was removed the host re-registered, the viewer reconnected through the relay, a new approval prompt appeared (21 s later), and after approval the session ran at 30 fps |
| "Find devices" on the Wi-Fi network | Found the host at its Wi-Fi address with the right key |
| Connect through the found device (direct, over the Wi-Fi address) | Worked: route "directly"; the connection ran from the PC's Wi-Fi address to itself, not loopback; approval prompt; 30 fps |
| Host with "let other computers connect" off, viewer to the Wi-Fi address | Refused: "actively refused it". The same host accepted loopback |

## After the fix

The first run led to the rate control described in `MILESTONE-9.md`
("Keeping up with the connection"). The same scenarios, same setup, with it:

| Test | Before | After |
|---|---|---|
| Cap of 0.8 Mbit/s, moving pattern, about 50 s | 7.8 s behind after 10 s; round trip 26 s after 40 s | 34–214 ms behind across three runs, 16–35 fps; the host's window showed "Video limited to 0.1–0.5 of 8.0 Mbit/s"; it sent 0.21, 0.29, 0.40 and 0.71 Mbit/s in four 10 s samples |
| Cap removed | Recovered in about 8 s | 30 fps and 10–14 ms within 10 s; bit rate back at the maximum within about 30 s in two runs, at 4.9 of 8.0 Mbit/s after 25 s in the third |
| Delay 100 ms with 20 ms jitter, 60 s | 9 fps and 3.9 s behind after 20 s, still worsening | About 13 fps on average, 0.4–1.3 s behind, at the minimum bit rate; 30 fps again 12 s after removing it |
| First 10 s of a session, moving pattern | not measured | 0.99 Mbit/s (it starts at 30% and works up) |

Two faults were found and fixed on the way, both by these live runs and not
by the tests I had written first:

1. **A bit rate set before the encoder's first frame was ignored**, so the
   gentle start did nothing and 8.2 Mbit/s went out early in a session. The encoder
   is now created at the starting rate.
2. **The video stopped for good after the path got longer.** With the 100 ms
   delay added, the round trip settled at about 180 ms; measured against the
   lowest round trip *ever* seen (0.7 ms) that looked like 180 ms of
   queueing, which is neither over the mark for holding nor under the mark
   for releasing, so a hold that had started never ended (frames stopped at
   4337 for over 20 s). Queueing is now measured against the lowest round
   trip of the last 10 s. A simulation of exactly this fails without the
   change (video held 83% of the time, nothing delivered afterwards) and
   passes with it.

## What it found

1. **A link slower than the video needs made the picture lag by tens of
   seconds, and nothing reacted.** The host kept sending at its own rate; the
   excess waited in buffers on the way. The keepalive only asks "is anything
   arriving?", so the session stayed up. Fixed; see "After the fix".
2. The 15 s dead-link detection and the reconnect flow work on a real
   non-loopback path, with both ends noticing independently.
3. Nothing found that depends on loopback: direct connection by Wi-Fi
   address, discovery and the relay path all work off loopback.

## Changes this made to the machine

Rust (1.100.0) was installed for the user in Ubuntu on WSL, about 760 MB in
`~/.rustup` and `~/.cargo`, plus the source copy and build in `~/rc` (216 MB).
Nothing was installed on Windows, no firewall or system setting was changed,
and the traffic shaping was removed afterwards.

## Still not tested

Two separate computers; NAT and a home router; a firewall between machines;
the app through a real ISP; a Windows Firewall prompt for a *remote* peer (a
connection from this PC to its own address is not filtered by it).
