# Milestone 2 — authorized screen capture on Windows

## Scope

1. `rc-capture`: list displays and capture one of them with DXGI Desktop
   Duplication.
2. Capture in the desktop app, started and stopped only by the user, with a
   visible indicator while it runs.
3. Measured capture rate and CPU cost.

Frames are counted and dropped. Encoding and sending them is milestone 3.

## Status

Complete, with one open observation (see the end).

| Part | Files |
|---|---|
| Display listing, capturer | `crates/rc-capture/src/dxgi.rs` |
| Frame type, row copy | `crates/rc-capture/src/frame.rs` |
| Benchmark | `crates/rc-capture/examples/capture_bench.rs` |
| Capture worker, pacing | `apps/desktop/src/capture.rs` |
| Commands, capture panel | `apps/desktop/src/commands.rs`, `apps/desktop/ui/` |

## How authorization works

- Capture starts in exactly one place: the `start_capture` command, invoked by
  the Start button. Nothing starts it at launch, from configuration or on a
  timer.
- While capture runs the header shows a red "Capturing screen" badge; it
  returns to "Not sharing" when capture stops.
- Start and stop are written to the log with the display and frame count.
- Capture stops when the application exits.
- An ordinary process cannot capture the lock screen or a UAC prompt. The
  worker reports "waiting" and retries instead of failing. (Handled in code;
  this path was not exercised in a test.)

## Design notes

- Desktop Duplication delivers a frame only when the screen changes, so an
  idle screen costs nothing.
- Each frame is copied GPU texture → staging texture → a reused CPU buffer of
  tightly packed BGRA rows.
- Duplication is lost on display-mode changes; the capturer reopens it on the
  next call. (Handled in code; not exercised in a test.)
- Display geometry is reported in physical pixels regardless of the calling
  process's DPI awareness.
- The frame-rate limit uses fixed deadlines rather than "sleep the rest of the
  interval".
- Rotated displays are listed with their rotation, but frames are delivered
  unrotated. Not handled yet.

## Problems found and fixed

1. **Scaled display size.** `first_frame_matches_the_display` failed: the
   display was listed as 1536×864 while frames were 1920×1080. Cause: the
   monitor runs at 125% scaling and DXGI reports scaled coordinates to a
   process that is not DPI-aware. Fix: enumerate under a per-monitor
   DPI-aware thread context. The test now passes.
2. **Frame-rate limit undershoot.** With a 24 fps limit the app measured
   17.4 fps. Cause: each sleep overshoots by up to one Windows timer tick
   (~15 ms) and the error accumulated. Fix: deadline scheduling (`Pacer`).
   Measured afterwards: 24.1 fps.
3. **Stop reported zero frames.** The stop log line and status were read
   after the worker had been discarded. Fix: totals are taken from the worker
   that just ended. Measured afterwards: `frames: 251`.

## Measurements

Machine: AMD Radeon integrated graphics, one 1920×1080 display at 125%
scaling, Windows 11. A full-screen video was playing, so the screen changed
continuously. Single runs, not averaged.

| Run | Result |
|---|---|
| `capture_bench`, release, 10 s, no rate limit | 288 frames, 28.8 fps, 0.406 s CPU = 4.1% of one core, 1.41 ms CPU per frame |
| Desktop app, release, 24 fps limit, ~10.5 s | 251 frames, 24.1 fps |
| Desktop app process, same run, ~13 s including window start-up | 0.72 s CPU ≈ 5.5% of one core |

The saved first frame of the benchmark was inspected and was a correct
full-colour image of the screen.

The unlimited rate is bounded by how often the screen changed, not by the
capturer; the maximum the capturer can sustain was not measured.

## Acceptance tests

Run locally on 2026-11-12. CI runs the same suite except the three tests
marked "desktop", which need a real monitor and are `#[ignore]`d there.

| # | Test | How | Result |
|---|---|---|---|
| B1 | Row copy handles padded rows, short last row, zero height; rejects bad sizes | `frame::tests` (6) | pass (CI, local) |
| B2 | Unknown display is rejected | `unknown_display_is_rejected` | pass (CI, local) |
| B3 | Displays are listed with exactly one primary and non-zero size | `lists_displays_with_one_primary` (desktop) | pass (local) |
| B4 | First frame has the display's physical size and is not blank | `first_frame_matches_the_display` (desktop) | pass (local) |
| B5 | Pacing: waits the remainder, compensates oversleep, no burst after idle | `capture::tests::pacer_*` (4) | pass (local) |
| B6 | Start, second start refused, frames counted, stop returns totals, restart works | `start_capture_stop_on_the_real_desktop` (desktop) | pass (local) |
| B7 | App is idle at launch: "Not sharing", no capture log line | drive the app window | pass (local) |
| B8 | Start shows "Capturing screen" and live statistics; Stop returns to "Not sharing" | drive the app window | pass (local) |
| B9 | Start and stop are logged with display and frame count | log inspection | pass (local) |
| B10 | Whole workspace: format, Clippy `-D warnings`, 33 tests | `cargo fmt/clippy/test` | pass (local) |

## Open observation

Once, after a 10-second capture in the release build, the window did not
close when asked to (`CloseMainWindow` returned true, the process kept
running and stayed responsive). Five further attempts, including the same
sequence, closed normally and logged `application exiting`. Cause unknown;
not reproduced; not fixed.

## Next milestone

Milestone 3: H.264 encoding of captured frames, a transport between two
instances, decoding, and rendering in the controller window.
