# Milestone 1 — workspace, configuration, application shell

## Scope

1. Cargo workspace with shared dependency versions and lints.
2. `rc-core`: configuration (load, validate, save), paths, structured logging.
3. `apps/desktop`: Tauri 2 shell that loads the configuration, starts logging
   and shows a settings/status window.

## Plan

| Step | Files | State |
|---|---|---|
| Workspace | `Cargo.toml`, `rust-toolchain.toml`, `.gitignore` | written, **not built** |
| Config model + validation | `crates/rc-core/src/config.rs` | written, **not built** |
| Paths | `crates/rc-core/src/paths.rs` | written, **not built** |
| Logging | `crates/rc-core/src/logging.rs` | written, **not built** |
| Tests | `crates/rc-core/tests/config.rs` | written, **not run** |
| Tauri shell | `apps/desktop/` | **not started** — needs the toolchain (`tauri` CLI generates icons and the schema-checked config) |

Nothing here has been compiled: Rust is not installed on the development
machine yet (see `ARCHITECTURE.md` §5). Milestone 1 is **not complete** until
every acceptance test below has been run and its output recorded here.

## Configuration design

- One file, `config.toml`, in the per-user config directory
  (`%APPDATA%\RemoteControl\config\`), overridable with `RC_CONFIG_DIR`.
- Missing file → defaults are written. Missing keys → defaults. A file that
  fails validation is rejected with the offending field named; it is never
  silently replaced.
- Saves are atomic (write to a temporary file, then rename).
- Secure defaults: approval required for every session; input, clipboard and
  file transfer off; no server addresses baked in.
- No secrets: keys and pairing data go to a DPAPI-protected store in
  milestone 5, never to this file.

## Build commands

```
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

## Acceptance tests

| # | Test | How | Result |
|---|---|---|---|
| A1 | Workspace builds with no warnings | `cargo build --workspace` | not run |
| A2 | Lints clean | `cargo clippy … -D warnings`, `cargo fmt --check` | not run |
| A3 | Defaults are valid and secure (input, clipboard, file transfer off; no servers baked in). Approval cannot be disabled: no such setting exists | `defaults_are_valid_and_secure` | not run |
| A4 | First run writes a default file | `load_or_create_writes_defaults` | not run |
| A5 | Save → load round-trips every field | `save_then_load_round_trips` | not run |
| A6 | Partial file is filled from defaults | `partial_file_uses_defaults` | not run |
| A7 | Invalid values are rejected, naming the field, on load and before save | `rejects_*`, `invalid_config_is_never_saved`, `invalid_file_is_rejected_on_load` | not run |
| A7b | File logging writes JSON lines with structured fields | `file_logging_writes_json_lines` | not run |
| A8 | Malformed TOML is an error and the file is left untouched | `malformed_file_is_not_overwritten` | not run |
| A9 | Save leaves no temporary file behind | `save_is_atomic` | not run |
| A10 | Desktop shell starts, shows device name and config path, writes a log line | manual + log inspection | not started |
| A11 | Editing a setting in the UI persists across restart | manual | not started |

## Next milestone

Milestone 2: `rc-capture` — DXGI Desktop Duplication behind an explicit
"start sharing" action, monitor enumeration, measured capture rate and CPU use.
