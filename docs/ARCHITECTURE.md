# Remote Control — architecture

Status: design baseline, written before any code was compiled. Statements are
labelled **[verified]** (read or run during the study) or **[design]** (a
decision for this project, not yet proven by a build).

## 1. RustDesk analysis

Sources: the public repository `rustdesk/rustdesk` (master, `Cargo.toml` and
`LICENCE` fetched directly; module descriptions from its DeepWiki index), and
the RustDesk 1.4.9 build installed on the development machine.

### 1.1 What the source shows

| Area | RustDesk approach | Key files |
|---|---|---|
| Layout | Cargo workspace: `libs/scrap` (capture + codecs), `libs/hbb_common` (protobuf, config, TCP/UDP wrappers), `libs/enigo` (input), `libs/clipboard`, `libs/virtual_display`, `libs/portable`; app logic in `src/` | `Cargo.toml` [verified] |
| UI | Flutter (desktop + mobile) over `flutter_rust_bridge`; legacy Sciter UI still in tree | `flutter/`, `src/flutter_ffi.rs` |
| Process model | Several processes (service, per-session connection manager, UI) talking over local IPC | `src/ipc.rs` |
| Capture (Windows) | DXGI Desktop Duplication, GDI `BitBlt` fallback, optional Magnification API path | `libs/scrap/src/dxgi/mod.rs` |
| Encode / decode | Software VP8/VP9 (libvpx) and AV1 (libaom); hardware H.264/H.265 through `hwcodec` (FFmpeg: NVENC, QSV, AMF), with a GPU-texture path | `libs/scrap/src/common/{codec,hwcodec,vram}.rs`, `src/server/video_service.rs` |
| Wire format | Protobuf (`message.proto`, `rendezvous.proto`), length-prefixed frames | `libs/hbb_common/protos/` |
| Identity | Numeric ID + Ed25519 key pair, registered with the rendezvous server (`RegisterPk`) | `src/rendezvous_mediator.rs` |
| Connection | `PunchHoleRequest` to the rendezvous server (hbbs) → UDP/TCP hole punch → `RequestRelay` through the relay (hbbr) when punching fails; TCP, KCP-over-UDP, WebSocket and a WebRTC path | `src/client.rs`, `src/rendezvous_mediator.rs` |
| Session crypto | Host sends an Ed25519-signed ID + key; both sides do an X25519 box exchange and then encrypt the stream with a symmetric secretbox key | `src/common.rs` (`secure_tcp`) |
| Auth | Password hashed with a salt and a per-connection challenge; permanent or one-time password; or click-to-accept on the host; optional TOTP | `src/client.rs`, `src/server/connection.rs` |
| Input | `enigo` + `rdev` forks; `SendInput` on Windows | `libs/enigo`, `src/server/input_service.rs` |

### 1.2 What the shipped binary shows

REA (the reverse-engineering connector) was run against the installed
`C:\Program Files\RustDesk\librustdesk.dll` (36 167 024 bytes, SHA-256
`e301faa8…70f02`). Results, reported as they happened:

- `inspect_rust_binary`: **no** cargo-auditable `.dep-v0` section, so the exact
  crate list cannot be read from the binary [verified].
- Rizin provider: did not run. Windows Smart App Control (enforced on this
  machine) blocked Rizin's unsigned DLLs, exit `0xC0E90002` [verified]. Not
  worked around.
- Ghidra provider: import + auto-analysis exceeded REA's 330 s startup deadline
  on a 36 MB image [verified]. Not retried with a raised limit.
- Fallback: a read-only byte scan of the same file [verified]. It references
  `dxgi.dll`, `d3d11.dll`, `gdi32.dll`, `dxva2.dll`, `nvencodeapi64.dll`,
  `nvcuvid.dll`, `amfrt64.dll`, `wtsapi32.dll`, `userenv.dll`, `sas.dll`,
  `conpty.dll`; and contains the strings `h264_nvenc`, `hevc_nvenc`,
  `h264_qsv`, `h264_amf`, `PunchHole`, `RequestRelay`, `RegisterPk`,
  `RelayResponse`, `rs-ny.rustdesk.com`, ports `21116`/`21117`, plus `kcp`,
  `tungstenite`, `webrtc`, `opus`, `hwcodec`, `flutter_rust_bridge`.

This agrees with the source reading: DXGI/GDI capture, FFmpeg hardware
encoders, a rendezvous + relay protocol on 21116/21117, and a Flutter UI. A
string scan proves presence of text, not that a code path is used.

### 1.3 Licensing and reuse

- RustDesk is **AGPL-3.0** [verified: `LICENCE` is "GNU Affero General Public
  License, Version 3"]. Its server (hbbs/hbbr) is AGPL as well.
- Copying or adapting RustDesk code — including its forks under `rustdesk-org/*`
  and the modified `libs/scrap`, `libs/enigo` — would make this application a
  derived work: the whole program would have to be AGPL-3.0, and its source
  offered to every user who interacts with it over a network.
- Decision **[design]**: **no RustDesk code is copied.** Architecture ideas
  (rendezvous + hole punch + relay, capture → encode → transport) are not
  copyrightable and are reused as concepts. Everything is implemented against
  Windows APIs and permissively licensed crates (MIT / Apache-2.0 / BSD).
- Not reusable: RustDesk's protobuf schema as-is, its public rendezvous servers,
  its name or branding.
- The project licence is the owner's choice and is still open; nothing in the
  dependency list forces a copyleft licence.

### 1.4 Where this design deliberately differs

| RustDesk | This project | Reason |
|---|---|---|
| Custom box/secretbox handshake | Noise protocol (`snow`), mutual static keys | Reviewed, formally analysed handshake instead of a bespoke one; forward secrecy |
| Salted password + challenge hash | PAKE-based pairing code, then pinned device keys | A captured exchange cannot be brute-forced offline |
| KCP + TCP + WebRTC + WebSocket | QUIC (`quinn`) for direct paths, TLS/WebSocket on 443 for relay | Two transports cover the same ground with far less code |
| FFmpeg hardware encoders + libvpx | Media Foundation H.264 (hardware when present), software fallback | No bundled GPL/LGPL codec binaries; ships with Windows |
| Flutter | Tauri 2 + WebView2 | Requested stack; small installer; WebCodecs gives hardware decode |
| Unattended access as a service | Not in scope until explicitly designed | Attended, consent-first product |

## 2. Application architecture

```
 ┌──────────────── Controller ────────────────┐      ┌────────────────── Host ──────────────────┐
 │ UI (Tauri/WebView2)                        │      │ UI: consent prompt, session indicator     │
 │  renderer: WebCodecs decode → canvas       │      │                                           │
 │  input capture → events                    │      │ capture (DXGI) → encoder (MF H.264)       │
 │ session ── E2E Noise channel ──────────────┼──────┼── session ── input injector (SendInput)   │
 │ connection manager: LAN → punch → relay    │      │ permission gate · audit log               │
 └───────────────┬────────────────────────────┘      └──────────────────┬────────────────────────┘
                 │   TLS                                                 │  TLS
                 └──────────────►  rc-server: rendezvous + relay  ◄──────┘
                                   (sees device IDs and ciphertext only)
```

### 2.1 Crates

Created only when their milestone starts; nothing is stubbed in advance.

| Crate | Milestone | Responsibility |
|---|---|---|
| `rc-core` | 1 | Configuration, paths, logging, shared errors |
| `apps/desktop` | 1 | Tauri shell, commands, UI |
| `rc-capture` | 2 | DXGI Desktop Duplication, monitor enumeration, dirty rects |
| `rc-codec` | 3 | H.264 encode (Media Foundation), colour conversion |
| `rc-protocol` | 3 | Versioned wire messages, framing, channel multiplexing |
| `rc-input` | 4 | `SendInput` injection, coordinate mapping, key translation |
| `rc-crypto` | 5 | Device identity keys, Noise handshake, pairing, key storage (DPAPI) |
| `rc-net` | 6–7 | Rendezvous client, LAN discovery, hole punching, relay client |
| `apps/server` | 6–7 | Rendezvous + relay service, Docker image |
| `rc-transfer` | 8 | Clipboard sync, file transfer |

### 2.2 Security model

- **Consent first.** Every inbound session raises a prompt on the host showing
  the controller's device name and key fingerprint. Default answer is deny.
  Screen view, input, clipboard and file transfer are separate permissions,
  each off until granted.
- **Always visible.** While a session is active the host shows a persistent
  indicator with a one-click disconnect. There is no hidden mode, no silent
  install and no auto-start unless the user turns it on in settings.
- **End-to-end encryption.** The Noise channel terminates on the two devices.
  The relay forwards ciphertext and cannot read or alter a session.
- **Identity.** Each device owns an Ed25519 key generated on first run and
  protected with Windows DPAPI. The device ID is derived from the public key,
  so the server cannot assign one device's ID to another.
- **Pairing.** First contact uses a short one-time code confirmed through a
  PAKE; after that the peer's key is pinned and a changed key is refused.
- **Audit.** Connection attempts, approvals, permission changes and
  disconnects are appended to a local log.
- **No secrets in config.** `config.toml` holds settings only.

Known limit **[design]**: an ordinary user process cannot capture or drive the
UAC secure desktop or the lock screen. Doing so needs a SYSTEM service, which
is a separate, explicitly reviewed piece of work and not part of the MVP.

### 2.3 "Works over all networks"

The connection manager tries paths in order and keeps the first that works:

1. **Same LAN** — direct QUIC to the address learned by local discovery.
2. **Different networks, cooperative NATs** — UDP hole punch coordinated by the
   rendezvous server, then QUIC.
3. **Symmetric NAT, CGNAT, UDP blocked, corporate proxy** — relay over
   TLS + WebSocket on TCP 443, which passes anything that allows HTTPS.

Two honest constraints:

- Paths 2 and 3 need `rc-server` running on a host with a public address
  (a small VPS and a domain name). No design removes that requirement; the
  alternative is depending on someone else's server.
- A network that blocks or intercepts all outbound HTTPS to that host cannot be
  crossed. Nothing else is a blocker.

## 3. Repository layout

```
Cargo.toml              workspace, shared dependency versions and lints
rust-toolchain.toml     toolchain channel (pin an exact version after first install)
crates/rc-core/         configuration, logging, errors
apps/desktop/           Tauri application (src-tauri + ui)
apps/server/            rendezvous + relay            (milestone 6)
deploy/                 Dockerfile, compose           (milestone 7)
docs/                   this file, milestone records
```

## 4. Dependencies

Versions are the semver ranges in `Cargo.toml`; exact versions are whatever
`Cargo.lock` records at first build and are not asserted here.

| Crate | Used for | Why this one |
|---|---|---|
| `serde`, `toml` | Config (de)serialisation | De facto standard; TOML is hand-editable |
| `thiserror` | Typed library errors | No runtime cost, clear error surface |
| `directories` | Per-user config and log paths | Correct `%APPDATA%` handling without hand-rolled paths |
| `tracing`, `tracing-subscriber`, `tracing-appender` | Structured logs, daily rotation | Async-aware, span-based |
| `tokio` | Async runtime | Requested; required by `quinn` and Tauri |
| `tauri` 2 | Desktop shell | Requested; uses the WebView2 already on the machine |
| `windows` | DXGI, Media Foundation, `SendInput`, DPAPI | Microsoft's own bindings, feature-gated |
| `quinn`, `rustls` | QUIC transport, TLS to the server | Pure Rust, no OpenSSL |
| `snow` | Noise handshake | Small, audited-pattern implementation |
| `ed25519-dalek`, `x25519-dalek` | Device identity | Widely reviewed |
| `prost` | Wire messages | Compact, schema-versioned |
| `tempfile` (dev) | Test isolation | — |

Only the `rc-core` rows are in the manifest today; the rest are added by the
milestone that needs them. PostgreSQL is **not** planned: the server's state
(who is online, at which address) is ephemeral and held in memory.

## 5. Development environment

Found on the machine [verified 2026-11-11]:

| Tool | State |
|---|---|
| Rust (`rustc`, `cargo`, `rustup`) | **not installed** |
| MSVC build tools / Windows SDK | **not installed** (`vswhere` absent, no `Windows Kits\10\Lib`) |
| Node 24.14.0, npm 11.9.0, pnpm | installed |
| Git 2.53.0 | installed |
| WebView2 runtime 157 | installed |
| Docker | not installed (needed from milestone 7) |
| Smart App Control | **enforced** |

Required before anything compiles:

1. Visual Studio 2022 Build Tools with "Desktop development with C++"
   (MSVC + Windows 11 SDK). Needs administrator rights.
2. Rust stable via `rustup` (`x86_64-pc-windows-msvc`). Per-user; adds
   `%USERPROFILE%\.cargo\bin` to the user `PATH`.

Risk: Smart App Control blocks unsigned executables without cloud reputation.
A Rust build produces and runs many such files (build scripts, test binaries,
the application itself), so builds may fail on this machine with
"An Application Control policy has blocked this file". This is a hypothesis
until a build is attempted; it is known to affect Rust development under SAC.
Switching SAC off is a one-way Windows security setting and is the machine
owner's decision.

Second risk: the project lives under OneDrive. `target/` grows to several GB
and OneDrive will try to sync and lock those files. Either exclude the folder
from sync or point `CARGO_TARGET_DIR` at a path outside OneDrive.
