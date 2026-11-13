# Milestone 8 — clipboard and file transfer

## Scope

1. Copy text on one computer, paste on the other.
2. Send a file either way.
3. Both are opt-in, visible, and controlled by the person at the receiving
   end, and neither can be used to run anything on the other computer.

## Status

Complete as designed. Tested with unit tests, in-process sessions, and a run of
two real instances of the app on **one** computer (so one shared Windows
clipboard). **Not tested across two computers.**

| Part | Files |
|---|---|
| Protocol v4: permission bits, clipboard and file messages | `crates/rc-protocol/src/lib.rs` |
| Clipboard sync (system and in-memory backends) | `crates/rc-clipboard/` |
| File transfer engine | `apps/desktop/src/transfer.rs` |
| Both, per session | `apps/desktop/src/capabilities.rs` |
| Host and viewer wiring | `apps/desktop/src/share.rs`, `viewer.rs` |
| Commands for the window | `apps/desktop/src/commands.rs` |
| Window controls and transfer lists | `apps/desktop/ui/` |

## Consent rules

- The host has three switches: **input**, **clipboard**, **files**. Each
  can be changed during a session and the change reaches the viewer at once.
  The host enforces them on every event, so a viewer that ignores what it was
  told still gets nothing.
- Clipboard needs **both ends**: the host allows it and the viewer ticks its
  own box. Turning sync on never sends what is already on the clipboard.
- Clipboard content that Windows marks as private (password managers set
  this) is never sent. Only text is synced, up to a size limit.
- Every file needs a yes from the **receiving person**, per file, shown with
  its name and size. Unanswered offers expire after 60 seconds.
- Received files go to `Downloads\Remote Control`. The name is reduced to a
  safe file name (no paths, no reserved names), an existing file is never
  overwritten, and the file is marked as downloaded from the internet
  (Zone.Identifier) so Windows warns before running it.
- Limits: 2 GiB per file, 4 incoming and 2 outgoing at a time. Sending is
  paced (8 MiB/s) so a transfer cannot starve the video on the same
  connection.
- A file is checked with SHA-256 when it ends. A mismatch, a cancel, a
  switch-off, or the session ending deletes the partial file.

## Tests

| Where | What it shows |
|---|---|
| `rc-protocol` (14) | Round trips and limits for the new messages, permission bits |
| `rc-clipboard` (10) | No echo of a remote write, private and non-text content skipped, oversize skipped, enabling never sends existing content, a backend that bumps its counter per write does not loop |
| `transfer` (20) | Accept/refuse, name sanitizing, no overwrite, hash mismatch, size limit, timeouts, cancel from either side, caps on parallel transfers, Zone.Identifier |
| `share/capability_tests` (9, real sessions over loopback) | Viewer is told what the host allows and changes follow at once; clipboard both ways only when allowed and wanted; not applied by a viewer that has not asked; impossible when the host has not allowed it; files both ways after accept; refused with no prompt when files are off; switching files off mid-transfer removes the partial file; ending the session mid-transfer leaves no partial file; the receiver can refuse and the sender is told |

Run on this machine, twice in a row: `cargo test --workspace --locked
--no-fail-fast -- --include-ignored` — 238 tests passed each time, 0 failed.
`cargo fmt --check` and `cargo clippy --workspace --all-targets -D warnings`
were clean. The session tests are `#[ignore]`d on CI (they need a desktop)
and run locally with `--include-ignored`.

## Measured in a live run (two app instances, one computer)

- Host-to-viewer and viewer-to-host transfers were accepted by the receiving
  window; the SHA-256 of the received file matched the original (3 MB and
  400 kB files).
- The received file carried the Zone.Identifier mark.
- A file the host refused (`unwanted.exe`) showed "refused" at the viewer and
  nothing was written.
- Real Windows clipboard: with the viewer's box unticked the viewer ignored
  the host's text. After ticking it, one copy produced one receive; counts did
  not change over the next 3 seconds (no echo loop, even though both
  instances share the same clipboard here).
- After the run both apps were closed, the listener was gone, and the test
  files were deleted.

## Not covered

- **Two computers.** Clipboard in particular was exercised with one shared
  clipboard; real two-machine behaviour (clipboard timing, copy-while-
  sending) is not tested.
- **Text only.** No images, files copied in Explorer, or rich text.
- **Failure detail to the sender.** The sender learns "done", "refused" or
  "cancelled" but not the receiver's disk or permission error.
- **Transfers share the video connection.** The pacing keeps video smooth in
  the cases tried, but it was not measured under a slow link.
- **Private-content check** is covered by a unit test, not repeated live.
- Gaps from Milestone 7 remain: relay connections idle without keepalive,
  no TLS/WebSocket wrapping for networks that only allow web traffic, no hole
  punching.

## Next milestone

Milestone 9: connection monitoring, keepalive and reconnect, and
performance work.
