# Milestone 1 — workspace, configuration, application shell

## Scope

1. Cargo workspace with shared dependency versions and lints.
2. `rc-core`: configuration (load, validate, save), paths, structured logging.
3. `apps/desktop`: Tauri 2 shell that loads the configuration, starts logging
   and shows a settings/status window.

## Status

Complete. Every acceptance test below has been run; see "How this was
verified".

| Part | Files | State |
|---|---|---|
| Workspace | `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml` | builds on CI |
| Config model + validation | `crates/rc-core/src/config.rs` | tested on CI |
| Paths | `crates/rc-core/src/paths.rs` | builds on CI, exercised by the desktop tests |
| Logging | `crates/rc-core/src/logging.rs` | tested on CI |
| Desktop shell | `apps/desktop/src/` | builds on CI; state logic tested on CI |
| Settings window | `apps/desktop/ui/` | exercised in the running app |
| CI | `.github/workflows/ci.yml`, `ci/run.sh` | working |

## How this was verified

**CI** (GitHub Actions, `windows-latest`, Rust 1.99.1):

- Run 43083580717 (commit `7b639fe`): **failed** at Clippy — `type_complexity`
  in `crates/rc-core/tests/config.rs`. Fixed.
- Run 43084180160 (commit `31c2dcd`) and run 43163116046 (commit `4bd40d5`):
  **passed** — Format, Clippy (`-D warnings`), Test.

**Locally** (2026-11-12, after Smart App Control was switched off; before
that every build script was blocked with `os error 4551`):

- `cargo test --workspace --locked`: 17 passed, 0 failed
  (12 config, 1 logging, 4 desktop state).
- The debug build of `rc-desktop` was started with `RC_CONFIG_DIR` pointing at
  an empty folder and its window was driven through the WebView2 debugging
  port:
  - first start: header showed the computer name and version 0.1.0, the
    settings path and log folder were displayed, no error banner, Save
    disabled, `config.toml` created;
  - device name, "Mouse and keyboard control" and frame rate were changed and
    saved: "Settings saved.", header updated, file on disk matched;
  - frame rate 0 was refused with
    `invalid configuration: video.max_fps: 0 is outside 1..=120`, file
    unchanged;
  - after closing and restarting, the window showed the saved values;
  - the log file held JSON lines for `application started`,
    `configuration saved`, `configuration rejected` and `application exiting`.

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
| A1 | Workspace builds with no warnings | CI Clippy step, `-D warnings` | pass (CI, local) |
| A2 | Lints and formatting clean | CI Format and Clippy steps | pass (CI, local) |
| A3 | Defaults are valid and secure; approval cannot be disabled | `defaults_are_valid_and_secure` | pass (CI, local) |
| A4 | First run writes a default file | `load_or_create_writes_defaults` | pass (CI, local) |
| A5 | Save → load round-trips every field | `save_then_load_round_trips` | pass (CI, local) |
| A6 | Partial file is filled from defaults | `partial_file_uses_defaults` | pass (CI, local) |
| A7 | Invalid values are rejected, naming the field, on load and before save | `rejects_*`, `invalid_config_is_never_saved`, `invalid_file_is_rejected_on_load` | pass (CI, local) |
| A7b | File logging writes JSON lines with structured fields | `file_logging_writes_json_lines` | pass (CI, local) |
| A8 | Malformed TOML is an error and the file is left untouched | `malformed_file_is_not_overwritten` | pass (CI, local) |
| A9 | Save leaves no temporary file behind | `save_is_atomic` | pass (CI, local) |
| A9b | Shell state: first start, persistence across restart, invalid update rejected, damaged file never overwritten | `apps/desktop/src/state.rs` tests | pass (CI, local) |
| A10 | Desktop shell starts, shows device name and config path, writes a log line | run the app | pass (local) |
| A11 | Editing a setting in the UI persists across restart | run the app | pass (local) |

## Next milestone

Milestone 2: `rc-capture` — DXGI Desktop Duplication behind an explicit
"start sharing" action, monitor enumeration, measured capture rate and CPU use.
Capture cannot be tested on a CI runner in any meaningful way (no real
display), so its measurements come from local runs.
