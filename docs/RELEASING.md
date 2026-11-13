# Building and releasing

What the build produces, how to make it yourself, how CI makes it, and what
is **not** done: nothing is signed, nothing is published automatically, and
there is no update mechanism.

## What is produced

| Output | From | How |
|---|---|---|
| `Remote Desk_<version>_x64-setup.exe` | `apps/desktop` | Tauri 2 with an NSIS installer, per-user |
| `rc-server` (Linux x86_64) and `rc-server-linux-x86_64.tar.gz` | `apps/server` | `cargo build --release -p rc-server` |
| `rc-server:<tag>` image | `deploy/Dockerfile` | `docker build -f deploy/Dockerfile .` |
| `SHA256SUMS.txt` | the two above | `sha256sum` in CI |

The version number comes from `[workspace.package] version` in the top
`Cargo.toml` (the Tauri configuration carries its own `version`; keep them
equal when you change it).

## Reproducible builds: what is and is not guaranteed

- **Pinned:** the dependency versions (`Cargo.lock`, always built with
  `--locked`), and the compiler (`rust-toolchain.toml` names an exact
  version, 1.99.1; the Docker image uses the same). The Tauri CLI version
  used by CI is set in `.github/workflows/installer.yml`
  (`TAURI_CLI_VERSION`).
- **Not pinned:** the CI runners' own software (Windows SDK, Visual Studio
  Build Tools, the NSIS toolset the Tauri CLI downloads, WebView2), and the
  base operating system images.
- **Not verified:** that two builds of the same commit are byte-for-byte
  identical. They were not compared. Windows executables normally embed a
  build timestamp, so assume they are not. What *is* reproducible is the
  source and the toolchain, so anyone can build the same code themselves and
  compare behaviour; compare the published SHA-256 with the file you
  downloaded to know you have the file CI produced.

## Build the installer yourself (Windows)

You need: Windows 10/11, Rust (rustup reads `rust-toolchain.toml` and fetches
1.99.1), the Visual Studio Build Tools (C++ workload), and the Tauri CLI
(`cargo install tauri-cli --version 2.12.4 --locked`).

```powershell
cd apps\desktop
cargo tauri build
```

Output: `target\release\bundle\nsis\Remote Desk_0.1.0_x64-setup.exe`.

The first build downloads the NSIS toolset (nsis-3.13) from GitHub through
the Tauri CLI. **That download failed from this development machine** ("An
existing connection was forcibly closed", twice, and with `curl` too), so the
installer could not be built *here*; it was built on CI instead (see below).
If your network blocks it, the tools can be placed by hand; see the Tauri
documentation for the cache folder and expected files.

Check an installer on a **throwaway machine**:

```powershell
pwsh ci\installer-test.ps1 -Installer "target\release\bundle\nsis\Remote Desk_0.1.0_x64-setup.exe"
```

It installs silently for the current user, checks where files went and that
nothing was registered for all users, that no autostart entry, scheduled task
or service was added, starts the app for 12 s and checks it listens on no
port until sharing is started, then uninstalls and checks what is left. It
fails on the first surprise. It will replace an installed copy, so do not run
it on your own PC.

## Build the server

```bash
cargo build --release -p rc-server --locked
```

Or the image: `docker build -f deploy/Dockerfile -t rc-server .`. See
`docs/DEPLOYMENT.md`.

## What CI does

| Workflow | When | Does |
|---|---|---|
| `CI` (`ci.yml`), job `windows` | every push | formatting, clippy with warnings as errors, the whole test suite, the window-script tests |
| `CI`, job `linux-server` | every push | clippy and tests of the server crates on Linux, builds the server image and runs `ci/server-smoke.sh` against it, verifies the systemd unit |
| `Installer` (`installer.yml`), job `windows-installer` | pushes to `main` that touch code, tags `v*`, manual | builds the installer, writes SHA-256 sums, runs `ci/installer-test.ps1` on the clean runner, keeps the files as a run artifact |
| `Installer`, job `linux-server` | same | builds the release server binary and a tarball with the systemd unit, keeps them with a checksum |

Tests that need a real, interactive desktop (screen capture, driving the real
mouse) are marked `#[ignore]` and are **not run on CI**; run them on a
desktop with `cargo test --workspace -- --include-ignored`. The installer
workflow keeps its files for GitHub's default artifact retention (90 days
unless the repository says otherwise).

## Making a release

There is no automation that publishes. By hand:

1. Raise the version in `Cargo.toml` and `apps/desktop/tauri.conf.json`
   (they must match), update the documents that mention numbers, commit.
2. Wait for `CI` and `Installer` to be green on that commit.
3. Download the `windows-installer` and `linux-server` artifacts from the
   `Installer` run; check the SHA-256 against `SHA256SUMS.txt`.
4. Tag (`git tag v0.1.0 && git push origin v0.1.0`) and attach the files and
   the checksums to a GitHub release, saying plainly that the installer is
   unsigned.

## Not done

- **Code signing.** Needs a certificate from a certificate authority (an
  ongoing cost and an identity check) and a place to keep its key. Without
  it, Windows SmartScreen warns and **Smart App Control blocks unsigned
  programs outright** (it did on the development machine until its owner
  turned it off themselves). Do not ask users to turn that off to run this;
  sign the build first.
- **Updates.** No updater. A new version means installing the new installer
  over the old one (settings and keys are kept).
- **MSI, per-machine installs, silent deployment through management tools:**
  not built. The NSIS installer accepts `/S` for a silent per-user install.
- **A published release.** None has been made.
- **Supply-chain checks** (advisory scans, SBOM, provenance): none yet.
- **Other platforms.** The application is Windows only (screen capture,
  input and clipboard use Windows APIs). Only the server runs on Linux.
