//! Runs every program in tests/cases and compares stdout with the matching
//! `.out` file, and every program in tests/errors and checks that it fails
//! with stderr containing each line of the matching `.err` file. Everything runs
//! twice: interpreted, and compiled to native code (the same expectations apply).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn faston(path: &Path, native: bool) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_faston"))
        .arg(path)
        .env("FASTON_NATIVE", if native { "1" } else { "0" })
        .output()
        .expect("failed to run faston")
}

fn programs(dir: &str) -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join(dir);
    let mut files: Vec<PathBuf> = fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().map_or(false, |e| e == "fst"))
        // `_name.fst` files are helper modules imported by the tests, not tests.
        .filter(|p| !p.file_name().unwrap().to_string_lossy().starts_with('_'))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no test programs in {}", root.display());
    files
}

fn check_outputs(native: bool) {
    let mut failures = Vec::new();
    for prog in programs("cases") {
        let expected = fs::read_to_string(prog.with_extension("out")).unwrap_or_default();
        let out = faston(&prog, native);
        let stdout = String::from_utf8_lossy(&out.stdout);
        if !out.status.success() || stdout != expected {
            failures.push(format!(
                "{}{}\n--- expected\n{}--- got (exit {:?})\n{}{}",
                prog.display(),
                if native { " (native)" } else { "" },
                expected,
                out.status.code(),
                stdout,
                String::from_utf8_lossy(&out.stderr)
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

fn check_errors(native: bool) {
    let mut failures = Vec::new();
    for prog in programs("errors") {
        let expected = fs::read_to_string(prog.with_extension("err")).unwrap();
        let out = faston(&prog, native);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let missing: Vec<&str> = expected
            .lines()
            .filter(|l| !l.is_empty() && !stderr.contains(l))
            .collect();
        if out.status.success() || !missing.is_empty() {
            failures.push(format!(
                "{}{}: exit {:?}, missing {:?} in stderr:\n{}",
                prog.display(),
                if native { " (native)" } else { "" },
                out.status.code(),
                missing,
                stderr
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// `faston build`s `prog` with machine code compiled ahead of time (the faston
/// binary itself standing in for the small runtime) and runs the result. Build
/// failures come back as the output.
fn built(prog: &Path) -> std::process::Output {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("built");
    fs::create_dir_all(&dir).unwrap();
    let exe = dir.join(prog.file_stem().unwrap()).with_extension(std::env::consts::EXE_EXTENSION);
    let b = Command::new(env!("CARGO_BIN_EXE_faston"))
        .arg("build")
        .arg(prog)
        .arg("-o")
        .arg(&exe)
        .env("FASTON_RT", env!("CARGO_BIN_EXE_faston"))
        .output()
        .expect("failed to run faston build");
    if !b.status.success() {
        return b;
    }
    let out = Command::new(&exe).env("FASTON_JIT_DEBUG", "1").output().expect("failed to run a built program");
    // every function must have run as saved machine code, not been recompiled
    // or interpreted because its code didn't match
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    for line in stderr.lines().filter(|l| l.starts_with("[aot]")) {
        let w: Vec<&str> = line.split_whitespace().collect();
        let (got, of) = (w[w.len() - 6], w[w.len() - 4]);
        assert_eq!(got, of, "{}: {}", prog.display(), line);
    }
    assert!(stderr.contains("[aot]"), "{}: saved machine code wasn't used:\n{}", prog.display(), stderr);
    let mut out = out;
    out.stderr = stderr.lines().filter(|l| !l.starts_with("[aot]")).map(|l| format!("{}\n", l)).collect::<String>().into_bytes();
    out
}

/// Compiles `prog` to a bytecode file next to it (so its imports resolve), runs
/// that, and removes it. Compile failures come back as the output.
fn from_bytecode(prog: &Path) -> std::process::Output {
    let fbc = prog.with_extension("test.fbc");
    let c = Command::new(env!("CARGO_BIN_EXE_faston"))
        .arg("compile")
        .arg(prog)
        .arg("-o")
        .arg(&fbc)
        .output()
        .expect("failed to run faston compile");
    if !c.status.success() {
        return c;
    }
    let out = Command::new(env!("CARGO_BIN_EXE_faston")).arg(&fbc).output().expect("failed to run faston");
    let _ = fs::remove_file(&fbc);
    out
}

/// `import lib` finds a compiled `lib.fbc`, also inside a built executable.
#[test]
fn imports_bytecode_modules() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("fbc_import");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let faston = env!("CARGO_BIN_EXE_faston");
    fs::write(dir.join("lib.fst"), "def twice(x) { return x * 2; }\nlet NAME = \"lib\";\n").unwrap();
    fs::write(dir.join("main.fst"), "import lib;\nprint(lib.twice(21), lib.NAME);\n").unwrap();
    let c = Command::new(faston).arg("compile").arg(dir.join("lib.fst")).output().unwrap();
    assert!(c.status.success(), "{}", String::from_utf8_lossy(&c.stderr));
    fs::remove_file(dir.join("lib.fst")).unwrap();
    let out = Command::new(faston).arg(dir.join("main.fst")).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "42 lib\n", "{}", String::from_utf8_lossy(&out.stderr));
    // a built program carries the bytecode module
    let exe = dir.join("main").with_extension(std::env::consts::EXE_EXTENSION);
    let b = Command::new(faston)
        .args(["build", dir.join("main.fst").to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .env("FASTON_RT", faston)
        .output()
        .unwrap();
    assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));
    fs::remove_file(dir.join("lib.fbc")).unwrap();
    let out = Command::new(&exe).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "42 lib\n", "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn bytecode_files_produce_expected_output() {
    let mut failures = Vec::new();
    for prog in programs("cases") {
        let expected = fs::read_to_string(prog.with_extension("out")).unwrap_or_default();
        let out = from_bytecode(&prog);
        let stdout = String::from_utf8_lossy(&out.stdout);
        if !out.status.success() || stdout != expected {
            failures.push(format!(
                "{} (.fbc)\n--- expected\n{}--- got (exit {:?})\n{}{}",
                prog.display(),
                expected,
                out.status.code(),
                stdout,
                String::from_utf8_lossy(&out.stderr)
            ));
        }
    }
    for prog in programs("errors") {
        let expected = fs::read_to_string(prog.with_extension("err")).unwrap();
        let out = from_bytecode(&prog);
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        let missing: Vec<&str> = expected.lines().filter(|l| !l.is_empty() && !text.contains(l)).collect();
        if out.status.success() || !missing.is_empty() {
            failures.push(format!("{} (.fbc): missing {:?} in:\n{}", prog.display(), missing, text));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn built_programs_produce_expected_output() {
    let mut failures = Vec::new();
    for prog in programs("cases") {
        let expected = fs::read_to_string(prog.with_extension("out")).unwrap_or_default();
        let out = built(&prog);
        let stdout = String::from_utf8_lossy(&out.stdout);
        if !out.status.success() || stdout != expected {
            failures.push(format!(
                "{} (built)\n--- expected\n{}--- got (exit {:?})\n{}{}",
                prog.display(),
                expected,
                out.status.code(),
                stdout,
                String::from_utf8_lossy(&out.stderr)
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn built_errors_are_reported() {
    let mut failures = Vec::new();
    for prog in programs("errors") {
        let expected = fs::read_to_string(prog.with_extension("err")).unwrap();
        let out = built(&prog);
        // syntax errors are reported by `faston build` itself
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        let missing: Vec<&str> = expected.lines().filter(|l| !l.is_empty() && !text.contains(l)).collect();
        if out.status.success() || !missing.is_empty() {
            failures.push(format!("{} (built): exit {:?}, missing {:?} in:\n{}", prog.display(), out.status.code(), missing, text));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn programs_produce_expected_output() {
    check_outputs(false);
}

#[test]
fn errors_are_reported() {
    check_errors(false);
}

#[test]
fn native_programs_produce_expected_output() {
    check_outputs(true);
}

#[test]
fn native_errors_are_reported() {
    check_errors(true);
}
