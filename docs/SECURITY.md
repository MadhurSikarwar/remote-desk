# Security model

What this software promises, what it relies on, and what it does not do.
Everything here was written from the code and the tests of Milestones 1 to
10; where something is untested it says so. **No independent security audit
has been done**, and the installer is not signed.

## Principles

1. **The person at the shared computer decides, every time.** There is no
   unattended access and no way to connect without a prompt that a person
   answers. A remembered device is still asked every time.
2. **Nothing is hidden.** The application has a window; closing it ends
   sharing. It does not start with Windows, install a service, or add a
   scheduled task. The installer and the app are tested to confirm this
   (`ci/installer-test.ps1` checks the Run keys, Startup folders, scheduled
   tasks and services before and after installing, running and uninstalling).
3. **Least by default.** Control, clipboard and file transfer start off;
   other computers are not allowed to connect until the user turns that on;
   the connection and the approval are required even for the same computer.
4. **Everything sensitive is encrypted, end to end.** Between two devices,
   whether direct or through a relay.
5. **No secrets in the code or the repository.** Keys are generated on first
   run on each device. `.env` files are ignored by git.

## How a session is protected

- **Identity.** Each device has an X25519 key pair created on first use,
  stored encrypted with Windows DPAPI for the current user (a copy on another
  PC or in another account is useless). The device's ID is the fingerprint of
  its public key (16 hex digits).
- **Channel.** `Noise_XX_25519_ChaChaPoly_BLAKE2s` through the `snow`
  library; no cryptography is implemented in this project. Both sides prove
  their keys. Session keys come from fresh ephemeral keys, so a recording
  cannot be decrypted later if a long-term key leaks. Records are
  authenticated and strictly ordered; a changed, dropped, repeated or
  reordered record ends the connection.
- **Approval.** After the handshake the host asks its user: name, key, a
  six-digit **comparison code** derived from the handshake (the same on both
  screens unless someone relays the connection with their own keys). Until
  the user answers, nothing is captured or sent and nothing the viewer sends
  is read. No answer is "no".
- **Pinning.** A viewer given a device ID or a discovered key checks that
  the device that answers holds exactly that key, and refuses otherwise. A
  device refuses a server whose key does not match the fingerprint it was
  given.
- **Permissions** (input, clipboard, files) are announced by the host and
  **enforced on the host for every event**, not trusted from the viewer. A
  switch turned off takes effect at once, including letting go of any key the
  viewer was holding.
- **Files** need the receiving person's yes per file; names are reduced to a
  safe file name (no paths, no reserved device names), an existing file is
  never overwritten, a SHA-256 is checked when the file ends, a partial file
  is deleted on any failure, and the result is marked as downloaded from the
  internet so Windows warns before it is run. Nothing can be run remotely by
  the program.
- **Clipboard** is text only, only while both ends want it, never sends what
  was there when it was switched on, and skips content marked private.
- **Servers** (rendezvous and relay) see addresses, IDs, names and byte
  counts, never session content, and cannot impersonate a device (see
  `docs/DEPLOYMENT.md`).
- **Liveness.** A connection with no traffic for 15 s ends; a session cannot
  hang open unseen on a dead link.
- **Logs** hold devices, keys, addresses, times and file sizes. They do not
  hold screen, input, clipboard text or file names/contents.

## What it does and does not protect against

| Threat | Covered? |
|---|---|
| Someone on the network reading the screen, input, clipboard or files | Yes: encrypted end to end |
| Someone on the network altering or replaying data | Yes: authenticated, ordered records |
| An unknown device starting a session | Yes: the host's user must allow it, every time |
| A device pretending to be one approved before | Yes: the key is proven; the name is not trusted |
| A server operator reading a session | Yes: they hold no session key |
| A server operator or network attacker sending a viewer to the wrong device | Yes, if the viewer used a device ID or a discovered key: the key is checked |
| Someone in the middle relaying both sides with their own keys | Only if the two people compare the six-digit code |
| A viewer asking for more than the host allowed | Yes: enforced at the host per event |
| A file with a dangerous name or an altered body | Name made safe, hash checked, marked as from the internet; the user decides whether to open it |
| The relay or a device being flooded with connections | Partly: connection limits per address and in total, handshake and idle timeouts, a pause after a refused request. No protection against a determined flood |
| Anyone who can reach an open relay using its bandwidth | **No.** The server has no accounts or allow-list. Restrict it by firewall or network |
| A program running as the same Windows user on the host | **No.** It can use the key through DPAPI and drive the app |
| Someone at the unlocked host clicking Allow | **No.** That is the consent model |
| Malware on the viewer or host | **No** |
| A person at the host being misled by the viewer's name | Partly: the name is shown only as text, stripped of control characters and limited to 64 characters, and the key is shown beside it; people still have to look at the key and code |

## Known gaps

- **No external audit or penetration test.** The design uses a standard
  protocol and library, but the way it is assembled here has been reviewed
  only by its author and tests.
- **Unsigned installer and binaries.** Windows will warn, and Smart App
  Control may block them; there is no certificate behind them and no update
  channel to deliver a fix quickly.
- **The comparison code only helps if people compare it.** There is no
  typed pairing code or other second factor.
- **The viewer does not keep a list of hosts it trusts**; it shows the key
  and code each time.
- **`security.idle_timeout_minutes` does nothing.** It was meant to end idle
  sessions. A forgotten session stays open until someone closes it or the
  connection fails. The setting is shown greyed out.
- **The server's key file is not encrypted at rest**; a device's is (DPAPI).
- **Servers are open** (see above), IPv4 only.
- **Not on every network.** No TLS/WebSocket wrapping, so networks that
  allow only web traffic block it; no NAT hole punching, so two devices
  behind separate home routers always use the relay.
- **Not tested across two separate computers**, through a real NAT, a
  firewall between machines, or on the internet. Network behaviour was tested
  on one PC through a Linux relay with simulated delay, loss and bandwidth
  limits (`docs/NETWORK-TESTS.md`).
- **The secure desktop** (lock screen, UAC prompts) cannot be captured, and
  nothing is sent while it shows. Input into windows of programs running as
  administrator is expected to be blocked by Windows (UIPI) unless this app
  is too; that was not tested.
- **Dependencies** are pinned by `Cargo.lock` and built with `--locked`, but
  were not individually audited and there is no automated advisory scan in
  CI yet.

## Reporting a problem

Open an issue on the project's repository, or contact the maintainer
directly for anything that should not be public first. There is no bounty
and no response-time promise.
