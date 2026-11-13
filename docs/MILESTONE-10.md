# Milestone 10 — production deployment documentation and installer

## Scope

1. A Windows installer that is honest about what it does.
2. A way to run the server in production (Linux), with packaging.
3. Documentation a user, an operator and a reviewer can work from.
4. CI that builds and tests the above on clean machines.

## Status

Built, and tested on CI's clean runners and (for the server unit) on Ubuntu
in WSL. **Not done:** code signing, a published release, an updater, Docker
on this development machine (so the container was only ever built and run by
CI), anything on two real computers.

| Part | Files |
|---|---|
| Per-user NSIS installer | `apps/desktop/tauri.conf.json` (`bundle`) |
| Install / run / uninstall test | `ci/installer-test.ps1` |
| Server container, compose file, systemd unit | `deploy/` |
| Container test | `ci/server-smoke.sh` |
| CI: Linux job, installer workflow | `.github/workflows/ci.yml`, `installer.yml` |
| Pinned compiler | `rust-toolchain.toml`, `deploy/Dockerfile` |
| Server stops on SIGTERM | `apps/server/src/main.rs` |
| Documents | `README.md`, `docs/USER-GUIDE.md`, `DEPLOYMENT.md`, `SECURITY.md`, `RELEASING.md` |

## Results

**Installer (CI, `windows-latest`, commit e30d1db).** `Remote Control_0.1.0_x64-setup.exe`,
2.5 MB, SHA-256 `c85c561e…3ed4e`. Silent install put `rc-desktop.exe` and
`uninstall.exe` in `%LOCALAPPDATA%\Remote Control`, registered for the
current user only, created a Start menu and a Desktop shortcut, and set
nothing to start by itself (checked: Run and RunOnce keys, Startup folders,
scheduled tasks, services). The installed app ran for 12 s without
listening on any port. The silent uninstall removed the program, its entry
and both shortcuts, and left the user's settings and web-view data in three
folders (listed in the user guide). Publisher shows as `remotedesk` (taken
from the app identifier; not chosen).

**Server on Linux.** Clippy and 67 tests pass on Linux (WSL, and CI's
Ubuntu job). The systemd unit, copied to a temporary path: started as a
dynamic user, key file mode 0600, printed the fingerprint, stopped with
status 0, same key after restart, `systemd-analyze security` score 1.8. CI
also builds the image and runs `ci/server-smoke.sh` (starts, prints key,
port answers, uid 10001, clean quick stop, same key after restart) and
`systemd-analyze verify`; the CI job passed on e30d1db. Server tarball
SHA-256 `85954843…ea2827`.

**Whole workspace** (`--include-ignored`, pinned compiler 1.99.1): 264 passed;
node tests 5 passed.

## Found on the way

- `security.idle_timeout_minutes` (default 30) was offered in the window and
  read by nothing. It is now greyed out and documented as not active.
  Implementing it needs a decision about what "idle" means for a viewer that
  only watches.
- A Linux-only compiler warning (a Windows-only constant) was fixed.
- The server did not stop cleanly on SIGTERM (what containers and systemd
  send); it now does.
- The unauthenticated GitHub API quota (60 an hour) ran out while watching
  CI; the status scripts now wait for the reset.

## Not covered

- **No code signing**: Windows warns; Smart App Control blocks it.
- **No published release, no updater, no MSI.**
- **The installer could not be built on this PC**: the Tauri CLI's download
  of the NSIS toolset from GitHub was reset twice and with `curl`. CI built
  it. Not checked: that a local build produces the same installer.
- **Builds are not verified byte-for-byte reproducible** (source, lock file
  and compiler are pinned; runner software is not).
- **No automated dependency advisory scan.**
- **Two computers, a real network, a Windows Firewall prompt:** untested.
- The container was not run on this machine.

## Next

The brief's ten milestones are done. Open items, roughly by value: test on
two real computers; sign the installer; decide and implement idle timeout;
NAT hole punching and TLS/WebSocket wrapping; a security review by someone
else.
