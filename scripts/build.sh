#!/bin/sh
# Builds faston (with the gui module) and faston-slim (without it) into
# target/release. `faston build` uses faston-slim for programs that don't
# import gui, which makes those executables much smaller.
set -e
cd "$(dirname "$0")/.."

cargo build --release
cargo build --release --no-default-features --target-dir target/slim
cp target/slim/release/faston target/release/faston-slim

ls -lh target/release/faston target/release/faston-slim
