# Milestone 1 — workspace, configuration, application shell

## Scope

1. Cargo workspace with shared dependency versions and lints.
2. `rc-core`: configuration (load, validate, save), paths, structured logging.
3. `apps/desktop`: Tauri 2 shell that loads the configuration, starts logging
   and shows a settings/status window.

## Status

Code complete and verified by CI. **Not yet verified: the application running
on a desktop** (A10, A11) — see "How this was verified".

| Part | Files | State |
|---|---|---|
| Workspace | `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml` | builds on CI |
| Config model + validation | `crates/rc-core/src/config.rs` | tested on CI |
| Paths | `crates/rc-core/src/paths.rs` | builds on CI, exercised by the desktop tests |
| Logging | `crates/rc-core/src/logging.rs` | tested on CI |
| Desktop shell | `apps/desktop/src/` | builds on CI; state logic tested on CI |
| Settings window | `apps/desktop/ui/` | **never run** |
| CI | `.github/workflows/ci.yml`, `ci/run.sh` | working |

## How this was verified

The development machine has Smart App Control enforced. It blocks the build
scripts Cargo compiles and runs, so `cargo build` fails locally with
`An Application Control policy has blocked this file (os error 4551)`. Only
`cargo fmt` runs locally.

Every build and test result below therefore comes from GitHub Actions on
`windows-latest`, toolchain Rust 1.99.1:

- Run 43083580717 (commit `7b639fe`): **failed** at Clippy — `type_complexity`
  in `crates/rc-core/tests/config.rs`. Fixed.
- Run 43084180160 (commit `31c2dcd`): **passed** — Format, Clippy
  (`-D warnings`), Test.

The job log itself is not readable without signing in, so the per-test lines
were not inspected; the table records that the Test step, which runs
`cargo test --workspace --locked`, exited successfully.

## Configuration design

- One file, `config.toml`, in the per-user config directory
  (`%APPDATA%\RemoteControl\config\`), overridable with `RC_CONFIG_DIR`.
- Missing file → defaults are written. Missing keys → defaults. A file that
  fails validation is rejected with the offending field named; it is never
  silently replaced, and the UI goes read-only.
- Saves are atomic (write to a temporary file, then rename).
- Secure defaults: input, clipboard and file transfer off; no server addresses
  baked in. There is no setting that disables the approval prompt.
- No secrets: keys and pairing data go to a DPAPI-protected store in
  milestone 5, never to this file.

## Build commands

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo run -p rc-desktop
```

## Acceptance tests

| # | Test | How | Result |
|---|---|---|---|
| A1 | Workspace builds with no warnings | CI Clippy step, `-D warnings` | pass (CI) |
| A2 | Lints and formatting clean | CI Format and Clippy steps | pass (CI) |
| A3 | Defaults are valid and secure; approval cannot be disabled | `defaults_are_valid_and_secure` | pass (CI) |
| A4 | First run writes a default file | `load_or_create_writes_defaults` | pass (CI) |
| A5 | Save → load round-trips every field | `save_then_load_round_trips` | pass (CI) |
| A6 | Partial file is filled from defaults | `partial_file_uses_defaults` | pass (CI) |
| A7 | Invalid values are rejected, naming the field, on load and before save | `rejects_*`, `invalid_config_is_never_saved`, `invalid_file_is_rejected_on_load` | pass (CI) |
| A7b | File logging writes JSON lines with structured fields | `file_logging_writes_json_lines` | pass (CI) |
| A8 | Malformed TOML is an error and the file is left untouched | `malformed_file_is_not_overwritten` | pass (CI) |
| A9 | Save leaves no temporary file behind | `save_is_atomic` | pass (CI) |
| A9b | Shell state: first start, persistence across restart, invalid update rejected, damaged file never overwritten | `apps/desktop/src/state.rs` tests | pass (CI) |
| A10 | Desktop shell starts, shows device name and config path, writes a log line | run the app | **not run** |
| A11 | Editing a setting in the UI persists across restart | run the app | **not run** |

A10 and A11 need the application to run on a Windows desktop, which Smart App
Control prevents on the development machine.

## Next milestone

Milestone 2: `rc-capture` — DXGI Desktop Duplication behind an explicit
"start sharing" action, monitor enumeration, measured capture rate and CPU use.
Capture cannot be tested on a CI runner in any meaningful way (no real
display), so it depends on a machine that can run the build.
