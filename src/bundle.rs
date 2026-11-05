//! `faston build`: a standalone executable is this program's own binary with the
//! Faston sources (the main file and every module it imports) appended. When it
//! starts it finds them, compiles them to machine code and runs them; no Faston
//! installation, interpreter or C toolchain is needed on the target machine.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::lexer::{self, Tok};
use crate::vm::Vm;

const MAGIC: &[u8; 8] = b"FSTNBND1";

pub struct Bundle {
    pub main_name: String,
    pub main_src: String,
    pub modules: HashMap<String, String>,
}

/// The program bundled into the running executable, if any.
pub fn embedded() -> Option<Bundle> {
    let exe = std::env::current_exe().ok()?;
    let mut f = fs::File::open(exe).ok()?;
    let len = f.metadata().ok()?.len();
    if len < 16 {
        return None;
    }
    f.seek(SeekFrom::Start(len - 16)).ok()?;
    let mut tail = [0u8; 16];
    f.read_exact(&mut tail).ok()?;
    if &tail[8..] != MAGIC {
        return None;
    }
    let size = u64::from_le_bytes(tail[..8].try_into().unwrap());
    if size > len - 16 {
        return None;
    }
    f.seek(SeekFrom::Start(len - 16 - size)).ok()?;
    let mut data = vec![0u8; size as usize];
    f.read_exact(&mut data).ok()?;
    decode(&data)
}

fn decode(data: &[u8]) -> Option<Bundle> {
    let mut pos = 0usize;
    let mut next = || -> Option<String> {
        let n = u32::from_le_bytes(data.get(pos..pos + 4)?.try_into().ok()?) as usize;
        pos += 4;
        let s = std::str::from_utf8(data.get(pos..pos + n)?).ok()?.to_string();
        pos += n;
        Some(s)
    };
    let main_name = next()?;
    let main_src = next()?;
    let mut modules = HashMap::new();
    while let Some(name) = next() {
        let src = next()?;
        modules.insert(name, src);
    }
    Some(Bundle { main_name, main_src, modules })
}

fn encode(b: &Bundle) -> Vec<u8> {
    let mut out = Vec::new();
    let mut put = |s: &str| {
        out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        out.extend_from_slice(s.as_bytes());
    };
    put(&b.main_name);
    put(&b.main_src);
    let mut names: Vec<&String> = b.modules.keys().collect();
    names.sort();
    for n in names {
        put(n);
        put(&b.modules[n]);
    }
    out
}

/// Bundle entry holding the program's `sys.path` (relative, one per line).
pub const PATH_KEY: &str = "\0path";

/// Reads a dotted module name starting at token `j`; returns it and the next index.
fn dotted(toks: &[lexer::Token], mut j: usize) -> Option<(String, usize)> {
    let mut name = match toks.get(j).map(|t| &t.tok) {
        Some(Tok::Ident(n)) => n.clone(),
        _ => return None,
    };
    j += 1;
    while matches!(toks.get(j).map(|t| &t.tok), Some(Tok::Dot)) {
        match toks.get(j + 1).map(|t| &t.tok) {
            Some(Tok::Ident(n)) => {
                name.push('.');
                name.push_str(n);
                j += 2;
            }
            _ => break,
        }
    }
    Some((name, j))
}

/// Module names (possibly dotted) a source file imports.
fn imports(src: &str) -> Vec<String> {
    let toks = match lexer::lex(src) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        match toks[i].tok {
            Tok::Import => {
                // import a, b.c as d;
                let mut j = i + 1;
                while let Some((name, next)) = dotted(&toks, j) {
                    out.push(name);
                    j = next;
                    if matches!(toks.get(j).map(|t| &t.tok), Some(Tok::As)) {
                        j += 2;
                    }
                    if !matches!(toks.get(j).map(|t| &t.tok), Some(Tok::Comma)) {
                        break;
                    }
                    j += 1;
                }
                i = j.max(i + 1);
            }
            Tok::From => {
                if let Some((name, next)) = dotted(&toks, i + 1) {
                    // `from pkg import sub`: sub may be a submodule
                    if matches!(toks.get(next).map(|t| &t.tok), Some(Tok::Import)) {
                        let mut j = next + 1;
                        while let Some(Tok::Ident(n)) = toks.get(j).map(|t| &t.tok) {
                            out.push(format!("{}.{}", name, n));
                            j += 1;
                            if matches!(toks.get(j).map(|t| &t.tok), Some(Tok::As)) {
                                j += 2;
                            }
                            if !matches!(toks.get(j).map(|t| &t.tok), Some(Tok::Comma)) {
                                break;
                            }
                            j += 1;
                        }
                    }
                    out.push(name);
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    out
}

/// Every module file the program can import, found the way `faston run` finds
/// them, keyed by path relative to the main file's directory.
fn collect(main_dir: &Path, src: &str, vm: &mut Vm) -> HashMap<String, String> {
    let base = crate::modules::canon(main_dir);
    let search = crate::modules::default_path(&base);
    let mut modules: HashMap<String, String> = HashMap::new();
    let mut seen_files: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut todo: Vec<(PathBuf, String)> = vec![(base.clone(), src.to_string())];
    while let Some((dir, s)) = todo.pop() {
        for name in imports(&s) {
            let first = name.split('.').next().unwrap_or("");
            if vm.module(first).is_some() || crate::stdlib::source(first).is_some() {
                continue;
            }
            let mut dirs = vec![dir.clone()];
            dirs.extend(search.iter().cloned());
            for part in name.split('.') {
                let found = match crate::modules::find(&crate::modules::Source::Disk, part, &dirs) {
                    Some(f) => f,
                    None => break, // a missing module fails at run time, as usual
                };
                if !found.src.is_empty() || found.display.ends_with(".fst") {
                    let file = PathBuf::from(&found.key);
                    let key = crate::modules::relative(&base, &file);
                    if seen_files.insert(key.clone()) {
                        modules.insert(key, found.src.clone());
                        todo.push((found.dir.clone(), found.src.clone()));
                    }
                }
                if !found.package {
                    break;
                }
                dirs = vec![found.dir.clone()];
            }
        }
    }
    let rel: Vec<String> = search
        .iter()
        .map(|d| crate::modules::relative(&base, &crate::modules::canon(d)))
        .map(|r| if r.is_empty() { ".".to_string() } else { r })
        .collect();
    modules.insert(PATH_KEY.to_string(), rel.join("\n"));
    modules
}

/// The runtime a built program starts from: `faston-slim` (no gui, much
/// smaller) next to this executable when the program doesn't import `gui`.
fn runtime(uses_gui: bool, force_full: bool) -> Option<(PathBuf, bool)> {
    let me = std::env::current_exe().ok()?;
    if !uses_gui && !force_full {
        let slim = me.with_file_name(if cfg!(windows) { "faston-slim.exe" } else { "faston-slim" });
        if slim.is_file() {
            return Some((slim, true));
        }
    }
    Some((me, false))
}

/// Builds `path` into a native executable. Returns the process exit code.
pub fn build(path: &str, out: Option<&str>, force_full: bool) -> i32 {
    let src = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("faston: can't open '{}': {}", path, e);
            return 2;
        }
    };
    let p = Path::new(path);
    let dir = match p.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let main_name = p.file_name().map_or(path.to_string(), |n| n.to_string_lossy().into_owned());

    // the imported modules (and packages), found exactly like `faston run` would
    let mut vm = Vm::compiled();
    let modules = collect(&dir, &src, &mut vm);

    // compile everything now so mistakes show up at build time
    vm.bundle = Some(modules.clone());
    if let Err(e) = vm.check_program(path, &src) {
        eprintln!("{}", e);
        return 1;
    }
    let (native, interpreted) = vm.native_stats().unwrap_or((0, 0));

    let uses_gui = std::iter::once(src.as_str())
        .chain(modules.iter().filter(|(k, _)| !k.starts_with('\0')).map(|(_, v)| v.as_str()))
        .any(|s| imports(s).iter().any(|n| n == "gui" || n.starts_with("gui.")));
    if uses_gui && !cfg!(feature = "gui") {
        eprintln!("faston: warning: this program imports gui, but this faston was built without it");
    }
    let (runtime_path, slim) = match runtime(uses_gui, force_full) {
        Some(r) => r,
        None => {
            eprintln!("faston: can't find the faston executable");
            return 1;
        }
    };
    let exe = match fs::read(&runtime_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("faston: can't read {}: {}", runtime_path.display(), e);
            return 1;
        }
    };
    // a faston binary that already carries a bundle: use only the binary part
    let exe = match strip(&exe) {
        Some(n) => exe[..n].to_vec(),
        None => exe,
    };
    let payload = encode(&Bundle { main_name, main_src: src, modules: modules.clone() });
    let mut bytes = exe;
    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    bytes.extend_from_slice(MAGIC);

    let out = match out {
        Some(o) => PathBuf::from(o),
        None => {
            let stem = p.file_stem().map_or("a".to_string(), |s| s.to_string_lossy().into_owned());
            PathBuf::from(stem)
        }
    };
    let out = if cfg!(windows) && out.extension().is_none() { out.with_extension("exe") } else { out };
    if let Err(e) = fs::write(&out, &bytes) {
        eprintln!("faston: can't write {}: {}", out.display(), e);
        return 1;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&out, fs::Permissions::from_mode(0o755));
    }
    println!(
        "built {} ({} module{}, {} function{} compiled to native code{})",
        out.display(),
        modules.len(),
        if modules.len() == 1 { "" } else { "s" },
        native,
        if native == 1 { "" } else { "s" },
        if interpreted > 0 { format!(", {} interpreted", interpreted) } else { String::new() }
    );
    let mb = bytes.len() as f64 / (1024.0 * 1024.0);
    if slim {
        println!("  {:.1} MB (slim runtime: no gui)", mb);
    } else if uses_gui {
        println!("  {:.1} MB (full runtime: the program uses gui)", mb);
    } else {
        println!(
            "  {:.1} MB (full runtime{})",
            mb,
            if force_full { "" } else { "; install faston-slim next to faston for much smaller programs: see scripts/" }
        );
    }
    0
}

/// Length of the binary part of an executable that carries a bundle.
fn strip(exe: &[u8]) -> Option<usize> {
    let n = exe.len();
    if n < 16 || &exe[n - 8..] != MAGIC {
        return None;
    }
    let size = u64::from_le_bytes(exe[n - 16..n - 8].try_into().unwrap()) as usize;
    n.checked_sub(16 + size)
}
