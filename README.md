# Faston — stage 0 (the Rust part)

This branch is the part of [Faston](../../tree/main) written in Rust:

- **the runtime**: bytecode VM, garbage collector, Cranelift JIT and
  ahead-of-time machine code, built-in modules (`os`, `json`, `re`, `gui`, ...),
  the bytecode loader and verifier;
- **the stage 0 compiler**: the original Rust lexer and compiler. Faston's
  compiler is now written in Faston (branch `main`, `compiler/`); stage 0 exists
  to bootstrap it (`faston --stage0 ...` selects it).

The language, its documentation, the self-hosted compiler and the tests that
run it live on `main`. The VS Code extension is on `vscode`. This branch is
frozen apart from fixes.

## Build

Requires a recent stable Rust toolchain (`rustup`).

```sh
scripts/build.sh          # or scripts\build.ps1 on Windows
```

builds into `target/release`:

| binary          | what                                                          | size     |
|-----------------|---------------------------------------------------------------|----------|
| `faston`        | the toolchain: runtime, JIT, gui, stage 0 compiler             | ~16 MB   |
| `faston-rt`     | runtime for built programs (no compiler, no JIT, no gui)       | ~2.6 MB  |
| `faston-rt-gui` | the same with the gui module                                   | ~12 MB   |

`faston build` puts programs on `faston-rt` / `faston-rt-gui` (they must be
next to `faston`): the executable carries the program's bytecode and machine
code, never source or a compiler.

## Which compiler `faston` uses

1. `--stage0` anywhere on the command line, or `FASTON_COMPILER=stage0`: the
   built-in Rust compiler;
2. `FASTON_COMPILER=<dir>`: the Faston compiler in `<dir>/compiler.fbc` (the
   `seed/` folder on `main`);
3. otherwise `compiler/compiler.fbc` next to the `faston` executable;
4. otherwise the built-in Rust compiler.

Both compilers produce identical bytecode (checked on `main` by
`compiler/check.fst` and `compiler/check_errors.fst`).

## Tests

```sh
cargo test --release      # every test interpreted, native, built ahead of time and from .fbc
FASTON_COMPILER=../faston-main/seed cargo test --release   # the same with the Faston compiler
faston --gen-stdlib src --check    # the built-in prelude/stdlib bytecode is up to date
```

`src/prelude.fst` and `src/stdlib/*.fst` are built into the executable as
bytecode (`src/*.fbc`, `src/stdlib/*.fbc`, regenerated with
`faston --gen-stdlib src`), so the runtimes need no compiler.

## Layout

```
src/
  main.rs       CLI, REPL
  lexer.rs      stage 0: tokens; f-strings are desugared here
  compiler.rs   stage 0: single-pass compiler + peephole fusion
  bytecode.rs   instruction set (opcode table shared with the .fbc format), fusion rules
  fbc.rs        bytecode files: format, loader + verifier, the `bytecode` module
  vm.rs         interpreter loop, calls, inline caches, compiler selection
  jit.rs        native code generator: type inference, Cranelift IR (also for `faston build`)
  jit_rt.rs     what compiled code needs at run time: helpers, loading saved machine code
  bundle.rs     `faston build`: standalone executables
  value.rs      Value, heap objects, garbage collector, dicts
  ops.rs        operator semantics (Python-compatible)
  builtins.rs   built-in functions, methods, math/random/time
  bytesmod.rs   bytes, bytearray, text encodings
  fileio.rs     open(), file objects, standard streams
  osmod.rs      sys, os, os.path, shutil
  json.rs, re.rs, natives.rs, gui.rs, pkg.rs, modules.rs, fx.rs
  prelude.fst, stdlib/*.fst   Faston code built into the runtime (as .fbc)
tests/          cases/, errors/ (same as main) and run.rs (cargo test)
scripts/        build.ps1 / build.sh
bench/          benchmarks with CPython equivalents
examples/       example programs
```
