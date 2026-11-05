//! Standard-library modules written in Faston, and the prelude. Their compiled
//! bytecode is built into the executable (so runtimes need no compiler) and
//! loaded on first import (their native helpers are in natives.rs).
//!
//! The .fbc files are generated from the .fst sources next to them with
//! `faston --gen-stdlib` (a test checks they are up to date).

/// The prelude: built-in exception classes, property, Enum, ...
pub const PRELUDE: &[u8] = include_bytes!("prelude.fbc");

/// Names of the standard-library modules written in Faston.
pub const NAMES: &[&str] = &["collections", "datetime", "subprocess"];

/// Compiled bytecode of a standard-library module.
pub fn bytecode(name: &str) -> Option<&'static [u8]> {
    Some(match name {
        "collections" => include_bytes!("stdlib/collections.fbc"),
        "datetime" => include_bytes!("stdlib/datetime.fbc"),
        "subprocess" => include_bytes!("stdlib/subprocess.fbc"),
        _ => return None,
    })
}

/// Source of a standard-library module (or "prelude").
#[cfg(feature = "compiler")]
pub fn source(name: &str) -> Option<&'static str> {
    Some(match name {
        "prelude" => include_str!("prelude.fst"),
        "collections" => include_str!("stdlib/collections.fst"),
        "datetime" => include_str!("stdlib/datetime.fst"),
        "subprocess" => include_str!("stdlib/subprocess.fst"),
        _ => return None,
    })
}

/// Is `name` a standard-library module (written in Faston)?
pub fn is_module(name: &str) -> bool {
    NAMES.contains(&name)
}
