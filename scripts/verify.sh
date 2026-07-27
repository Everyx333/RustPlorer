#!/usr/bin/env bash
# Pre-push verification.
#
# Exists because a Cargo.toml edit once made most dependencies Linux-only:
# a [target.'cfg(...)'] header was inserted mid-list, and TOML silently
# reassigned every key below it. Tests passed on Linux; Windows didn't compile.
#
# The lesson: verifying one target is not verification. Check both.
set -euo pipefail

echo "==> Structural check: shared deps must not fall under a target table"
# Ask Cargo itself which deps are unconditional (target == null). More
# authoritative than parsing the TOML by hand.
cargo metadata --no-deps --format-version 1 2>/dev/null | python3 -c '
import sys, json

pkg = json.load(sys.stdin)["packages"][0]
shared = {d["name"] for d in pkg["dependencies"] if d.get("target") is None}

# Imported unconditionally by the source. If one is missing here, a
# platform-specific section has captured it.
required = {
    "eframe","egui","egui_extras","tracing","tracing-subscriber",
    "tracing-appender","crossbeam-channel","parking_lot","directories",
    "humansize","chrono","arboard",
}

missing = sorted(required - shared)
if missing:
    print("FAIL: used on all platforms but not unconditional dependencies:")
    for m in missing:
        print(f"       - {m}")
    print("      A [target.\x27cfg(...)\x27] header placed mid-list captures every")
    print("      key below it. Move platform overrides below all shared deps.")
    sys.exit(1)

print(f"     ok ({len(shared)} shared dependencies)")
'

echo "==> Windows build (primary target)"
cargo xwin build --target x86_64-pc-windows-msvc

echo "==> Windows binary is a real PE"
exe=$(find target/x86_64-pc-windows-msvc -name 'rustplorer.exe' | head -1)
file "$exe" | grep -q 'PE32+' || { echo "FAIL: not a PE binary"; exit 1; }
echo "     $exe"

echo "==> Tests"
cargo test --quiet

echo
echo "All checks passed."
