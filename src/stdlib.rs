//! Standard-library modules written in Faston, compiled into the executable
//! and loaded on first import (their native helpers are in natives.rs).

pub fn source(name: &str) -> Option<&'static str> {
    Some(match name {
        "collections" => include_str!("stdlib/collections.fst"),
        "datetime" => include_str!("stdlib/datetime.fst"),
        "subprocess" => include_str!("stdlib/subprocess.fst"),
        _ => return None,
    })
}
