# Remote Desk

A remote-desktop application for Windows, inspired by RustDesk, built around
one rule: **the person at the shared computer decides, every time.** Rust,
Tauri 2, H.264 video, an end-to-end encrypted channel (Noise), direct or
relayed connections.

**Status: a working prototype, version 0.1.0.** Milestones 1 to 10 are built
and documented. It has been tested on **one computer** (loopback, its own
Wi-Fi address, and a Linux relay in WSL with simulated delay, loss and
bandwidth limits). It has **not been tested across two separate computers
or the internet**, has had **no independent security review**, and the
installer is **not code-signed**. Read `docs/SECURITY.md` before relying on
it.

*Named "Remote Control" until the end of Milestone 10; the milestone records
in `docs/` still use that name for what they measured. Settings and keys
now live in `%APPDATA%\RemoteDesk` (a copy made under the old name is not
picked up), and received files go to `Downloads\Remote Desk`. The labels
inside the key and handshake derivations still say "RemoteControl" on
purpose: changing them would change what identities and codes compute.*

## What it does

- Share a display; a viewer sees it (H.264, about 30 frames per second).
- The host approves each session, comparing a six-digit code with the
  viewer. Nothing is captured before that.
- Optional, each switched on by the host: mouse and keyboard control,
  clipboard text sync, file transfer (each file accepted by its receiver).
- Connect by address, by finding devices on the local network, or by device
  ID through an optional server (direct when possible, relayed otherwise).
- Notices dead connections within 15 s, reconnects (with a new approval),
  and slows the picture instead of falling behind on a slow link.

What it does **not** do: unattended access, start with Windows, run in the
background or hidden, run commands remotely, record, or carry audio.

## Install and use

Windows 10 or 11. Get `Remote Desk_<version>_x64-setup.exe` (see
`docs/RELEASING.md`; there is no published release yet) and read
`docs/USER-GUIDE.md`. The installer is per-user and needs no administrator
rights. It is unsigned: Windows will warn.

## Run a server (optional)

For reaching devices on other networks. Linux, Docker or systemd:

```bash
docker compose -f deploy/compose.yaml up -d --build
```

See `docs/DEPLOYMENT.md`, including the warning that the server has no
accounts: restrict who can reach it.

## Build and test

```powershell
cargo build --release                       # the app: target\release\rc-desktop.exe
cargo test --workspace --locked             # what CI runs
cargo test --workspace --locked -- --include-ignored   # also the tests that need a real desktop
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
node --test apps/desktop/ui/reconnect.test.js
```

The compiler is pinned (`rust-toolchain.toml`). The installer:
`cd apps\desktop; cargo tauri build` (see `docs/RELEASING.md`).

## Layout

| Path | What |
|---|---|
| `apps/desktop` | the application (Tauri shell, sessions, window in `ui/`) |
| `apps/server` | the rendezvous and relay server |
| `crates/rc-capture`, `rc-codec`, `rc-input` | screen capture (DXGI), H.264 (Media Foundation), input injection |
| `crates/rc-crypto`, `rc-net`, `rc-protocol`, `rc-rendezvous` | keys and the encrypted channel, discovery, wire format, server messages |
| `crates/rc-clipboard`, `rc-core` | clipboard sync, settings and logging |
| `deploy/`, `ci/`, `.github/workflows/` | server packaging, test scripts, CI |
| `docs/` | everything below |

## Documents

| | |
|---|---|
| `docs/USER-GUIDE.md` | installing and using it |
| `docs/DEPLOYMENT.md` | running the server |
| `docs/SECURITY.md` | what is protected, what is not |
| `docs/RELEASING.md` | building, CI, what is unsigned and unreleased |
| `docs/ARCHITECTURE.md` | design, and what was chosen differently from RustDesk and why |
| `docs/NETWORK-TESTS.md` | network tests through a Linux relay, with their limits |
| `docs/MILESTONE-1.md` … `MILESTONE-10.md` | one record per milestone: what was built, what was tested, what was not |

## Licence

No licence has been chosen. Until one is added to this repository, all
rights are reserved by the author.
