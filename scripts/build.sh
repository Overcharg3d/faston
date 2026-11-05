#!/bin/sh
# Builds faston and the two runtimes `faston build` puts programs on, into
# target/release:
#   faston         the full toolchain (code generator, gui)
#   faston-rt      runtime for programs that don't import gui
#   faston-rt-gui  runtime for programs that do
# The runtimes have no code generator: `faston build` compiles the program to
# machine code and saves it in the executable, so built programs stay small.
set -e
cd "$(dirname "$0")/.."

cargo build --release
cargo build --release --no-default-features --target-dir target/rt
cargo build --release --no-default-features --features gui --target-dir target/rt-gui
cp target/rt/release/faston target/release/faston-rt
cp target/rt-gui/release/faston target/release/faston-rt-gui

ls -lh target/release/faston target/release/faston-rt target/release/faston-rt-gui
