# Platforms

Prometheus was written for Windows. This page says what works on which
system, what has been checked and how, and what is still to be built. Read it
before relying on any non-Windows build.

## Status

| | Windows 10/11 | macOS | Linux |
|---|---|---|---|
| **Builds, passes lint and its tests on CI** | yes | yes (Apple Silicon runner) | yes (Ubuntu, x86_64) |
| **Package built on CI** | `.exe` installer | `.dmg` (Apple Silicon only) | `.deb` and `.AppImage` (x86_64) |
| **Package run by a person** | yes (installed, run, tested) | **no** | **no** |
| **Window, settings, device key, approval prompt** | works | built, never run | built, never run |
| **View another computer's screen** | works (tested) | built; the viewer relies on the web view's H.264 decoder, **untested** | built; the web view may **lack the decoder**, **untested** |
| **Control the other computer from here** | works (tested) | built, untested | built, untested |
| **Clipboard text sync** | works (tested) | built (`arboard`), untested; cannot skip content a password manager marked private | built (`arboard`), untested; same limit |
| **File transfer** | works (tested) | built, untested; no "downloaded from the internet" mark yet | built, untested; no such mark |
| **Share this computer's screen** | **works** | **not built** | **not built** |
| **Be controlled (input injection)** | **works** | **not built** | **not built** |
| **Device key protected at rest** | encrypted for your Windows account (DPAPI) | **file permissions only** (mode 0600) | **file permissions only** (mode 0600) |
| **Installer signed** | no | no | no |

"Built" means it compiles and its unit tests pass on CI. It does not mean
anyone has started the program on that system. No macOS or Linux desktop was
available during development; the only non-Windows runs were of the library
code (128 tests on Ubuntu under WSL, and CI).

So, today: **a Mac or Linux computer can be used to look at and use a
Windows host in principle, but that has not been tried, and neither can share
its own screen.**

The relay server (`rc-server`) is the exception: it runs on Linux and is
tested there (see `docs/DEPLOYMENT.md`).

## What is not built, and what it needs

Sharing a screen has four platform-specific parts. Each needs real hardware
of that kind to test.

| Part | Windows (done) | macOS | Linux X11 | Linux Wayland |
|---|---|---|---|---|
| Screen capture | DXGI Desktop Duplication | ScreenCaptureKit; needs the user to grant *Screen Recording* in System Settings | shared memory X11 capture | the desktop's screen-cast portal and PipeWire; the system asks the user each time |
| H.264 encoding | Media Foundation | VideoToolbox (hardware) or a software encoder | software encoder (OpenH264 or x264) | same |
| Mouse and keyboard injection | `SendInput` | `CGEvent`; needs *Accessibility* permission | XTest | the portal's remote-desktop interface (user is asked) |
| Key mapping | scancodes | macOS key codes | X11 key codes | evdev codes |

Notes that matter:

- **Consent.** macOS and Wayland ask the person at the computer for
  permission themselves. That fits this project's rule, but it also means a
  viewer can never get a picture without someone clicking in the operating
  system's own dialog as well as in Prometheus's.
- **Software H.264 encoders** have patent-licence conditions that depend on
  how they are obtained and distributed. That must be settled before a
  release; it is a legal question, not a technical one.
- **The viewer** decodes video in the web view with WebCodecs. WebView2
  (Windows) has it. Safari's engine (macOS 13 or later) is documented to have
  it. The Linux web view (WebKitGTK) may not. If it does not, decoding has to
  move into the program itself (a software H.264 decoder drawing into the
  window), which is a real piece of work.
- **Key storage.** The macOS Keychain and the Linux Secret Service should
  hold the device key; for now it is a file only its owner can read, which
  anyone who can read your files (or runs as you) can copy.
- **Signing.** macOS refuses unsigned downloaded apps unless the user
  overrides it, and a properly signed and notarised build needs an Apple
  developer account. Windows SmartScreen and Smart App Control have the same
  issue. Linux packages are usually fine unsigned.
- **Mac builds are Apple Silicon only** for now (the CI runner's
  architecture). An Intel or universal build is a small change that has not
  been made or tried.
- **Wayland** also limits the *clipboard* and global input in ways X11 does
  not; expect gaps.

## Plan, in order

1. **Done:** one codebase that builds, lints and tests on all three, with
   packages for each (this page's first table).
2. **Prove the viewer on Linux and macOS.** Run the packages, connect to a
   Windows host, and see whether video decodes. Fix or replace the decoder if
   it does not. Needs a Linux desktop and a Mac, or WSLg with the Linux web
   view libraries installed.
3. **Linux X11 host:** capture, software encoder, XTest input, key map.
4. **macOS host:** ScreenCaptureKit, VideoToolbox, `CGEvent`, key map,
   permission guidance in the window. Needs a Mac to build against and test.
5. **Linux Wayland host** through the portals.
6. **Key storage** in the Keychain and Secret Service; the "downloaded from
   the internet" mark on macOS (quarantine attribute).
7. **Signing and notarisation**, universal (Intel and Apple Silicon) Mac
   build, `.rpm` and Flatpak, an update channel.

Each step ends the way every earlier milestone did: built, tested, and
documented with what was and was not run.
