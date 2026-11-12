# Milestone 4 — mouse and keyboard control

## Scope

1. Protocol: input events from the viewer, permissions from the host.
2. `rc-input`: turn those events into real mouse and keyboard input.
3. A host-side switch that decides whether a viewer may control the PC.
4. Viewer: capture input over the picture and send it.

Still **this computer only**. Sessions stay on 127.0.0.1 until the encrypted
channel of milestone 5.

## Status

Control works and is gated by the host. One problem is open: the
application sometimes stalls while shutting down (see "Shutdown").

| Part | Files |
|---|---|
| Input and permission messages | `crates/rc-protocol/src/lib.rs` (protocol version 2) |
| Key names → scan codes | `crates/rc-input/src/keymap.rs` |
| Pointer positions → screen coordinates | `crates/rc-input/src/mapping.rs` |
| What the viewer is holding down | `crates/rc-input/src/pressed.rs` |
| Injection (`SendInput`) | `crates/rc-input/src/windows_injector.rs` |
| Host switch, enforcement | `apps/desktop/src/share.rs` |
| Viewer input | `apps/desktop/src/viewer.rs`, `apps/desktop/ui/session.js` |

## What "authenticated" means at this stage

There is no peer authentication yet; that is milestone 5. What exists is
authorization by the person at the host:

- Control is **off by default** (`security.allow_input = false`).
- The host has a checkbox, "Let the viewer control this computer's mouse and
  keyboard". It can be changed at any moment, also during a session.
- **The host enforces it.** Every input event is checked against the switch
  at the moment it would be carried out. What the viewer sends, or what the
  viewer's interface shows, has no say.
- Withdrawing control releases every key and button the viewer was holding,
  as does the end of a session.
- The host's badge reads "Sharing screen — viewer has control" while control
  is on, and each change of the switch is logged, as is the number of input
  events carried out and ignored per session.
- The viewer must separately tick "Control the shared screen"; moving the
  mouse over the picture does nothing until then.

## Design

- **Positions** travel as fractions of the shared display (0–65535), so they
  do not depend on how large the viewer draws the picture. The host maps
  them to a pixel on that display, then to the virtual-desktop coordinates
  `SendInput` expects, in physical pixels whatever the DPI scaling.
- **Keys** travel as scan codes of the physical key (from
  `KeyboardEvent.code`), so the host's keyboard layout decides the
  character. 104 keys are mapped; others are not forwarded.
- **Wheel** movement is in notches, sign-corrected between browser and
  Windows conventions.
- Pointer moves are limited to one per animation frame.

Limits:

- Windows does not let an ordinary program send input to windows of
  programs running as administrator, nor to the lock screen or UAC prompts.
- Ctrl+Alt+Del cannot be injected.
- Viewing and controlling on the same PC is only a test arrangement: the
  real pointer and the pointer in the picture are the same pointer.

## Problems found and fixed

1. **Tests that failed only together.** Two desktop tests failed in the full
   run and passed alone. One had a stale expectation: the host now announces
   permissions before the video configuration. The other lost its
   connection when several tests used the real desktop at once. Desktop
   tests now take turns. The full suite then passed three times in a row.
   That the cause was concurrent screen capture is consistent with this but
   was not shown directly.
2. **A commit made with failing tests.** That run reported 2 failures and
   the command chain committed regardless. The failures were fixed in the
   next commit; nothing had been pushed in between.
3. **Sharing could outlive the window.** Sessions were stopped only at the
   last step of shutdown. When shutdown stalled, the process went on
   sharing with no window on screen. Sessions are now stopped as soon as the
   window is destroyed.

## Acceptance tests

Run locally on 2026-11-12: `cargo test --workspace --no-fail-fast --
--include-ignored`, 81 passed, 0 failed. Tests marked "desktop" drive the
real mouse or keyboard briefly and are `#[ignore]`d on CI.

| # | Test | How | Result |
|---|---|---|---|
| D1 | Input and permission messages: round trip, truncation, unknown kinds, bad flags | `rc-protocol` tests | pass |
| D2 | 104 keys map to distinct scan codes; unknown names are not forwarded | `keymap::tests` | pass |
| D3 | Positions: corners, centre, a display not at the origin, whole virtual desktop, every column exact, degenerate sizes | `mapping::tests` | pass |
| D4 | Held keys and buttons are released newest first; repeats do not pile up | `pressed::tests` | pass |
| D5 | The real pointer lands within 1 px at four positions | `the_pointer_goes_where_the_viewer_points` (desktop) | pass |
| D6 | A held key is released when control ends | `a_held_key_is_released_when_the_injector_is_dropped` (desktop) | pass |
| D7 | Window input is parsed and converted; unknown keys and buttons dropped | `viewer::tests` | pass |
| D8 | Off by default: input does nothing. Allowed: it works. Withdrawn: held Shift is released and further input does nothing. The viewer is told at each step | `input_works_only_while_the_host_allows_it` (desktop) | pass |
| D9 | Two app instances: the viewer's control box is disabled until the host allows it, and says why | drive both windows | pass |
| D10 | A move sent with the box unticked is not forwarded; ticked, a move to the picture's centre puts the real pointer at the screen's centre | drive both windows; read the pointer | pass |
| D11 | Unticking the viewer's box releases a held Shift; the host counted exactly 3 events | drive both windows | pass |
| D12 | The host withdrawing control disables and unticks the viewer's box | drive both windows | pass |

Not tested end to end through the windows: mouse clicks, the wheel, and
typing text. They are covered at unit level (conversion, mapping) and
`SendInput` itself is exercised only for moves and Shift. A click or
keystrokes at an arbitrary place on the developer's desktop were avoided.

## Shutdown

**Observed.** In about 4 of 40 runs the process did not end after its
window was closed. Logging added for this shows the window is destroyed and
the framework reports "exit requested", but its final "exit" step never
comes. Every thread is waiting, so it is a deadlock rather than slowness.
It has only been seen in runs driven through the WebView2 debugging port,
and after the window had been showing video.

**Not known.** The cause. Reading the stuck threads' stacks needs a
debugger, which is not installed on the development machine.

**Done.**

- Sharing and viewing stop when the window is destroyed, before the
  framework's teardown. In the instrumented runs this ordering completed.
- A watchdog ends the process if shutdown has not finished 5 s after the
  window is destroyed. It was seen to fire once in six runs and the process
  ended.
- Close, destroy and exit requests are logged; the steps in between are
  logged at debug level (`RC_LOG=debug`).

So the stall still happens, but can no longer leave a process sharing a
screen or lingering beyond 5 seconds. This is containment, not a fix.

**Also seen, unexplained.** A few times a close request sent by the test
harness (`Process.CloseMainWindow`) had no effect and a second one worked.
Whether the harness failed to send it or the application dropped it was not
established; it did not occur in the last five instrumented pair runs.

## Next milestone

Milestone 5: device identity, pairing, an encrypted and authenticated
channel, and an approval prompt on the host. Only then may sessions leave
the loopback interface.
