# Running the rendezvous and relay server

The server is optional. Without it, two devices can still connect by address
or on the same network. With it, a viewer can reach a device by its **ID**
from another network, directly when possible and through the server's relay
when not. The server never sees what a session contains (see "What the
server can and cannot see"). It is one small program, `rc-server`, that
listens on one TCP port (default **21117**).

**Read first:** the server has **no accounts and no allow-list**. Anyone who
can reach its port and knows its address can register a device and use the
relay. Run it where that is acceptable: a private network, a firewall rule
that lists the addresses allowed to connect, or a VPN. A public server will
carry strangers' traffic and use your bandwidth.

## What you need

- A machine with a fixed address that both the hosts and the viewers can
  reach on one TCP port. Outbound TCP is all the devices need; nothing is
  needed inbound on the devices for the relay path.
- Linux with systemd or Docker. A Windows build exists (`cargo build
  --release -p rc-server`) and is tested in CI's regular Windows job, but the
  hardening and packaging below are for Linux.
- Bandwidth: a relayed session carries the full video both ways through the
  server. Measured here: about 1 to 8 Mbit/s of video for a 1080p desktop
  depending on what is on screen (the host holds its rate down when the
  connection cannot carry more).

Capacity limits built in (`Limits::default` in `apps/server/src/lib.rs`):
4096 connections, 32 per address, 10 000 registered devices, 512 relays at
once; a relay with no traffic for 15 minutes is closed (sessions send small
pings every 250 ms, so a live one is never idle); a connection that has not
finished the handshake in 5 s is dropped. These are not configurable without
rebuilding.

## Option A: Docker

From the repository root:

```bash
docker compose -f deploy/compose.yaml up -d --build
docker compose -f deploy/compose.yaml logs rc-server
```

The log shows the key fingerprint:

```
Server key: 07A9-11FA-88EB-D7EA
Listening on 0.0.0.0:21117
```

The container runs as an unprivileged user (uid 10001) with a read-only
filesystem, no Linux capabilities, and `no-new-privileges`. The key lives in
the `rc-server-data` volume: **keep that volume**. If it is lost the server
gets a new key and every device has to be given the new fingerprint.

`ci/server-smoke.sh` builds the image and checks: it starts and prints its
key, the port answers, it runs as uid 10001, `docker stop` ends it cleanly
and quickly (not by being killed), and the key is the same after a restart.
CI runs it on every push.

## Option B: a binary and systemd

Build (or take `rc-server-linux-x86_64.tar.gz` from the "Installer" workflow
run, see `docs/RELEASING.md`):

```bash
cargo build --release -p rc-server --locked
sudo install -m 755 target/release/rc-server /usr/local/bin/rc-server
sudo install -m 644 deploy/rc-server.service /etc/systemd/system/rc-server.service
sudo systemctl daemon-reload
sudo systemctl enable --now rc-server
journalctl -u rc-server | grep "Server key"
```

The unit runs the server as a temporary user of its own (`DynamicUser`), with
the key in `/var/lib/rc-server` (mode 0700, key file 0600), no capabilities,
a read-only view of the system, a system-call filter, and only IPv4/IPv6
sockets. `systemd-analyze security` rated the unit **1.8 "OK"** when tried on
Ubuntu (WSL, systemd 259); CI also runs `systemd-analyze verify` on it.

Tried on Ubuntu in WSL with a copy of the unit pointed at a temporary path:
it started, ran as the dynamic user, wrote a key with mode 0600, printed the
fingerprint, stopped with status 0 and "Deactivated successfully", and
printed the **same key** after a restart. Not tried: other distributions,
SELinux, an upgrade of a running service.

## Option C: by hand

```
rc-server --listen 0.0.0.0:21117 --data-dir /path/to/private/folder
```

`RC_LOG=debug` (or `warn`) sets the log detail. Ctrl-C, or SIGTERM on Unix,
stops it. The key file `server.key` in the data folder is **not encrypted**;
keep the folder private to the account that runs the server.

## Firewall

Open **TCP 21117** (or your chosen port) inbound to the server, from the
addresses that should be able to use it. Nothing else. If the devices are on
networks that only allow web traffic, a standard port such as 443 may get
through where 21117 does not: use `--listen 0.0.0.0:443` (Linux needs
`CAP_NET_BIND_SERVICE` for ports below 1024, which the unit above removes;
for 443 map it instead: `docker run -p 443:21117 …`).

**TLS and HTTP proxies:** the connection is not TLS and not WebSocket. It is
TCP carrying the Noise protocol. It passes any network that allows outbound
TCP to that port; it does not pass a proxy that only forwards HTTP(S) or one
that inspects traffic for TLS. A reverse proxy that terminates TLS in front
of it will not work; a plain TCP (stream) proxy will.

## Giving devices the server

On each device (host and viewer), in the settings file (see the user guide
for where it is):

```toml
[network]
rendezvous_servers = ["server.example.org:21117"]
server_fingerprint = "07A9-11FA-88EB-D7EA"
```

The fingerprint is **required**. A device refuses a server whose key does
not match it, so someone who takes over the address cannot stand in for your
server. Give people the fingerprint over a channel you trust, not through the
server itself. A host registers only while sharing is on **and** "let other
computers connect" is on.

Then a viewer enters the host's **device ID** (the 16-digit key fingerprint
shown in the host's window) instead of an address. With
`transport = "relay-only"` it always uses the relay; with `"auto"` it tries a
direct connection first; with `"direct-only"` it never uses the server for
traffic.

## What the server can and cannot see

It can see: which device IDs are registered, their public addresses and
chosen names, who asks to reach whom, when, and how many bytes a relay
carried. It can interrupt, delay or refuse any session.

It cannot see: the screen, the input, the clipboard or files. The session
is encrypted between the two devices (Noise XX with each device's own key),
and the server holds no session key. It cannot make a viewer reach the wrong
device either: the viewer checks that the device that answers has the key
matching the ID it asked for, and the person at the host still has to
approve every session.

## Operating it

- **Logs** go to standard output (journal or `docker logs`): registrations,
  connect requests, relays starting and ending. No session content.
- **Health:** the container health check opens the port. `rc-server` prints
  its key and listening address at start; a registered host's ID appears in
  the log as `device registered`, and each relay logs `relay started` and
  `relay ended` with the bytes carried.
- **Upgrade:** build the new version, stop, replace the binary (or rebuild
  the image), start. The key is kept. A host registers again by itself when
  the server comes back (covered by a test; the time it takes was not
  measured). The server and each device check a protocol version
  (`RENDEZVOUS_VERSION`, now 1) and refuse a mismatch, so a device that is
  much older or newer than the server will not connect.
- **Backup:** the key file, and nothing else. There is no database.
- **Changing the key** (to rotate it) means every device needs the new
  fingerprint in its settings.

## Not covered

- No accounts, allow-list, rate limiting by identity, or per-user quotas
  (see the warning at the top). There is a limit on connect requests per
  device per minute and on connections per address.
- The key file is not encrypted at rest. IPv4 only. One server per device.
- No metrics endpoint.
- Never run on the public internet in this project's own testing.
