# Building RustPlorer

## Requirements

- Rust 1.85 or newer (stable). Get it from https://rustup.rs
- **No C/C++ toolchain needed.** Every dependency is pure Rust, including all
  archive formats. This is a deliberate constraint — crates requiring MSVC build
  tools were rejected during selection.

## Build

```sh
cargo build --release
```

The binary lands at `target/release/rustplorer.exe`.

For a development build with the console window and verbose logs:

```sh
cargo run
```

## Logging

Logs are written to `%LOCALAPPDATA%\RustPlorer\logs\rustplorer.log`, rotated daily.

Raise the level with the `RUSTPLORER_LOG` environment variable:

```powershell
$env:RUSTPLORER_LOG="debug"; cargo run
# or target one module
$env:RUSTPLORER_LOG="rustplorer::fs=trace"; cargo run
```

## Reporting a problem

Click **Copy diagnostics** in the sidebar and paste the result into the issue.
It includes the version, environment, worker count, current path, and the last
500 log lines — enough to diagnose most failures without a back-and-forth.

## Tests

```sh
cargo test
```

Tests cover threading, cancellation, panic isolation, sorting, and scanning.
They are platform-agnostic and run on any host.
