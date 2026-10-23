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
