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

/// Module names a source file imports.
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
                // import a, b;
                let mut j = i + 1;
                while let Some(Tok::Ident(name)) = toks.get(j).map(|t| &t.tok) {
                    out.push(name.clone());
                    if !matches!(toks.get(j + 1).map(|t| &t.tok), Some(Tok::Comma)) {
                        break;
                    }
                    j += 2;
                }
                i = j;
            }
            Tok::From => {
                if let Some(Tok::Ident(name)) = toks.get(i + 1).map(|t| &t.tok) {
                    out.push(name.clone());
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    out
}

/// Builds `path` into a native executable. Returns the process exit code.
pub fn build(path: &str, out: Option<&str>) -> i32 {
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

    // the imported modules, found next to the importing file
    let mut modules: HashMap<String, String> = HashMap::new();
    let mut todo: Vec<(PathBuf, String)> = vec![(dir.clone(), src.clone())];
    while let Some((d, s)) = todo.pop() {
        for name in imports(&s) {
            if modules.contains_key(&name) {
                continue;
            }
            let file = d.join(format!("{}.fst", name));
            if let Ok(ms) = fs::read_to_string(&file) {
                let md = file.parent().map_or(d.clone(), |x| x.to_path_buf());
                modules.insert(name, ms.clone());
                todo.push((md, ms));
            } // else: a built-in module, or an ImportError at run time as usual
        }
    }

    // compile everything now so mistakes show up at build time
    let mut vm = Vm::compiled();
    vm.bundle = Some(modules.clone());
    if let Err(e) = vm.check_program(path, &src) {
        eprintln!("{}", e);
        return 1;
    }
    let (native, interpreted) = vm.native_stats().unwrap_or((0, 0));

    let exe = match std::env::current_exe().and_then(fs::read) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("faston: can't read the faston executable: {}", e);
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
        modules.len() + 1,
        if modules.is_empty() { "" } else { "s" },
        native,
        if native == 1 { "" } else { "s" },
        if interpreted > 0 { format!(", {} interpreted", interpreted) } else { String::new() }
    );
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
