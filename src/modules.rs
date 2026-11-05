//! Finding modules: `name.fst` files, packages (directories, optionally with an
//! `__init__.fst`), on disk or inside a built executable's bundle. The VM and
//! `faston build` share this, so a built program resolves imports exactly like
//! `faston run` does.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

/// Where module files come from.
pub enum Source<'a> {
    Disk,
    /// A built executable: sources keyed by normalized relative path.
    Bundle(&'a HashMap<String, String>),
}

/// A module that `find` located.
#[derive(Clone)]
pub struct Found {
    /// Identity (canonical path, or bundle key) so each file loads once.
    pub key: String,
    /// Source text ("" for a package directory without `__init__.fst`).
    pub src: String,
    /// Name shown in tracebacks.
    pub display: String,
    /// Directory its own imports (and, for packages, submodules) are searched in.
    pub dir: PathBuf,
    pub package: bool,
    /// A compiled module (`name.fbc`) instead of source.
    pub bytecode: Option<Vec<u8>>,
}

/// `canonicalize` without Windows' `\\?\` prefix (which git and humans dislike).
pub fn canon(p: &Path) -> PathBuf {
    let c = match p.canonicalize() {
        Ok(c) => c,
        Err(_) => return p.to_path_buf(),
    };
    let s = c.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        if !rest.starts_with("UNC\\") {
            return PathBuf::from(rest);
        }
    }
    c
}

/// `a/./b/../c` -> "a/c" with forward slashes (bundle keys).
pub fn norm(p: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if parts.last().is_some_and(|l| l != "..") {
                    parts.pop();
                } else {
                    parts.push("..".into());
                }
            }
            Component::Normal(n) => parts.push(n.to_string_lossy().into_owned()),
            // absolute paths don't occur in bundles
            Component::RootDir | Component::Prefix(_) => {}
        }
    }
    parts.join("/")
}

impl Source<'_> {
    fn read(&self, p: &Path) -> Option<String> {
        match self {
            Source::Disk => {
                if p.is_file() {
                    std::fs::read_to_string(p).ok()
                } else {
                    None
                }
            }
            Source::Bundle(b) => b.get(&norm(p)).cloned(),
        }
    }

    /// A binary file (bytecode); bundles keep these in `bundle::BINARY_PREFIX` entries.
    fn read_bytes(&self, p: &Path) -> Option<Vec<u8>> {
        match self {
            Source::Disk => {
                if p.is_file() {
                    std::fs::read(p).ok()
                } else {
                    None
                }
            }
            Source::Bundle(b) => {
                let hex = b.get(&format!("{}{}", crate::bundle::BINARY_PREFIX, norm(p)))?;
                crate::bundle::unhex(hex)
            }
        }
    }

    fn is_dir(&self, p: &Path) -> bool {
        match self {
            Source::Disk => p.is_dir(),
            Source::Bundle(b) => {
                let prefix = format!("{}/", norm(p));
                prefix != "/" && b.keys().any(|k| k.starts_with(&prefix))
            }
        }
    }

    fn key(&self, p: &Path) -> String {
        match self {
            Source::Disk => canon(p).display().to_string(),
            Source::Bundle(_) => norm(p),
        }
    }
}

/// Finds module `name` (one dotted part) in `dirs`, in order: a package with an
/// `__init__.fst`, then `name.fst`; only if neither exists in any directory, a
/// plain directory `name/` (a namespace package).
pub fn find(source: &Source, name: &str, dirs: &[PathBuf]) -> Option<Found> {
    for d in dirs {
        let pkg = d.join(name);
        let init = pkg.join("__init__.fst");
        if let Some(src) = source.read(&init) {
            return Some(Found {
                key: source.key(&init),
                src,
                display: format!("{}/__init__.fst", name),
                dir: pkg,
                package: true,
                bytecode: None,
            });
        }
        let file = d.join(format!("{}.fst", name));
        if let Some(src) = source.read(&file) {
            return Some(Found {
                key: source.key(&file),
                src,
                display: format!("{}.fst", name),
                dir: d.clone(),
                package: false,
                bytecode: None,
            });
        }
        // a compiled module (source wins when both exist)
        let fbc = d.join(format!("{}.fbc", name));
        if let Some(code) = source.read_bytes(&fbc) {
            return Some(Found {
                key: source.key(&fbc),
                src: String::new(),
                display: format!("{}.fbc", name),
                dir: d.clone(),
                package: false,
                bytecode: Some(code),
            });
        }
    }
    for d in dirs {
        let pkg = d.join(name);
        if source.is_dir(&pkg) {
            return Some(Found { key: source.key(&pkg), src: String::new(), display: format!("{}/", name), dir: pkg, package: true, bytecode: None });
        }
    }
    None
}

/// The directory that holds `faston.toml`, searching up from `start`.
pub fn project_root(start: &Path) -> Option<PathBuf> {
    let mut d = canon(start);
    loop {
        if d.join("faston.toml").is_file() {
            return Some(d);
        }
        if !d.pop() {
            return None;
        }
    }
}

/// The initial `sys.path`: the script's directory, the project root and its
/// installed packages (`.faston/packages`), then `FASTON_PATH`.
pub fn default_path(script_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let sd = canon(script_dir);
    out.push(sd.clone());
    if let Some(root) = project_root(&sd) {
        if root != sd {
            out.push(root.clone());
        }
        let deps = root.join(".faston").join("packages");
        if deps.is_dir() {
            out.push(deps);
        }
    }
    if let Some(p) = std::env::var_os("FASTON_PATH") {
        out.extend(std::env::split_paths(&p).filter(|x| !x.as_os_str().is_empty()));
    }
    out
}

/// `target` relative to `base` (both absolute), lexically: "../lib/x.fst".
pub fn relative(base: &Path, target: &Path) -> String {
    let b: Vec<Component> = base.components().collect();
    let t: Vec<Component> = target.components().collect();
    let common = b.iter().zip(t.iter()).take_while(|(x, y)| x == y).count();
    let mut out = PathBuf::new();
    for _ in common..b.len() {
        out.push("..");
    }
    for c in &t[common..] {
        out.push(c.as_os_str());
    }
    norm(&out)
}
