//! `faston build`: a standalone executable is a Faston runtime with the program
//! appended: the bytecode of the main file and of every module it imports, and
//! (for the small runtimes) their machine code. No source and no compiler go
//! into it; it needs no Faston installation, interpreter or C toolchain.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
#[cfg(feature = "compiler")]
use std::path::{Path, PathBuf};

#[cfg(feature = "compiler")]
use crate::lexer::{self, Tok};
#[cfg(feature = "compiler")]
use crate::vm::Vm;

const MAGIC: &[u8; 8] = b"FSTNBND2";

pub struct Bundle {
    pub main_name: String,
    /// Text entries (metadata such as `sys.path`).
    pub modules: HashMap<String, String>,
    /// Bytecode: the main module (`MAIN_KEY`) and every module, by path (`x.fbc`).
    pub binary: HashMap<String, Vec<u8>>,
    /// Machine code compiled at build time, for runtimes without a code generator.
    pub aot: Option<Vec<u8>>,
}

/// Binary entry holding the main module's bytecode.
pub const MAIN_KEY: &str = "\0main";
/// Name prefix of binary entries in the file.
const BIN: &str = "\0b:";
/// Entry holding the program's machine code.
pub const AOT_KEY: &str = "\0aot";
/// Entry holding the program's `sys.path` (relative, one per line).
pub const PATH_KEY: &str = "\0path";

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
    let mut next = || -> Option<&[u8]> {
        let n = u32::from_le_bytes(data.get(pos..pos + 4)?.try_into().ok()?) as usize;
        pos += 4;
        let s = data.get(pos..pos + n)?;
        pos += n;
        Some(s)
    };
    let text = |b: &[u8]| std::str::from_utf8(b).ok().map(str::to_string);
    let main_name = text(next()?)?;
    let mut modules = HashMap::new();
    let mut binary = HashMap::new();
    let mut aot: Option<Vec<u8>> = None;
    while let Some(name) = next() {
        let body = next()?;
        if name == AOT_KEY.as_bytes() {
            aot = Some(body.to_vec());
        } else if let Some(rest) = name.strip_prefix(BIN.as_bytes()) {
            binary.insert(text(rest)?, body.to_vec());
        } else {
            modules.insert(text(name)?, text(body)?);
        }
    }
    Some(Bundle { main_name, modules, binary, aot })
}

#[cfg(feature = "compiler")]
fn encode(b: &Bundle) -> Vec<u8> {
    let mut out = Vec::new();
    let mut put = |s: &[u8]| {
        out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        out.extend_from_slice(s);
    };
    put(b.main_name.as_bytes());
    let mut names: Vec<&String> = b.modules.keys().collect();
    names.sort();
    for n in names {
        put(n.as_bytes());
        put(b.modules[n].as_bytes());
    }
    let mut names: Vec<&String> = b.binary.keys().collect();
    names.sort();
    for n in names {
        put(format!("{}{}", BIN, n).as_bytes());
        put(&b.binary[n]);
    }
    if let Some(a) = &b.aot {
        put(AOT_KEY.as_bytes());
        put(a);
    }
    out
}

/// Reads a dotted module name starting at token `j`; returns it and the next index.
#[cfg(feature = "compiler")]
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
#[cfg(feature = "compiler")]
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

/// What a program imports, found the way `faston run` finds it: module sources
/// (keyed by path relative to the main file's directory), compiled modules
/// (.fbc files), and the relative `sys.path`.
#[cfg(feature = "compiler")]
struct Collected {
    sources: Vec<(String, String)>,
    compiled: HashMap<String, Vec<u8>>,
    path: String,
}

#[cfg(feature = "compiler")]
fn collect(main_dir: &Path, src: &str, vm: &mut Vm) -> Collected {
    let base = crate::modules::canon(main_dir);
    let search = crate::modules::default_path(&base);
    let mut sources: Vec<(String, String)> = Vec::new();
    let mut compiled: HashMap<String, Vec<u8>> = HashMap::new();
    let mut seen_files: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut todo: Vec<(PathBuf, String)> = vec![(base.clone(), src.to_string())];
    while let Some((dir, s)) = todo.pop() {
        for name in imports(&s) {
            let first = name.split('.').next().unwrap_or("");
            if vm.module(first).is_some() || crate::stdlib::is_module(first) {
                continue;
            }
            let mut dirs = vec![dir.clone()];
            dirs.extend(search.iter().cloned());
            for part in name.split('.') {
                let found = match crate::modules::find(&crate::modules::Source::Disk, part, &dirs) {
                    Some(f) => f,
                    None => break, // a missing module fails at run time, as usual
                };
                let key = crate::modules::relative(&base, &PathBuf::from(&found.key));
                if let Some(code) = &found.bytecode {
                    // a compiled module (its own imports must be found at run time,
                    // or come along through other imports)
                    if seen_files.insert(key.clone()) {
                        compiled.insert(key, code.clone());
                    }
                } else if (!found.src.is_empty() || found.display.ends_with(".fst")) && seen_files.insert(key.clone()) {
                    sources.push((key, found.src.clone()));
                    todo.push((found.dir.clone(), found.src.clone()));
                }
                if !found.package {
                    break;
                }
                dirs = vec![found.dir.clone()];
            }
        }
    }
    sources.sort();
    let rel: Vec<String> = search
        .iter()
        .map(|d| crate::modules::relative(&base, &crate::modules::canon(d)))
        .map(|r| if r.is_empty() { ".".to_string() } else { r })
        .collect();
    Collected { sources, compiled, path: rel.join("\n") }
}

/// The runtime a built program starts from.
#[cfg(feature = "compiler")]
struct Runtime {
    path: PathBuf,
    /// `faston-rt` / `faston-rt-gui`: no code generator, so the program's machine
    /// code is compiled now and saved in the executable.
    aot: bool,
    gui: bool,
}

/// `faston-rt` (or `faston-rt-gui` for programs that use the gui module) next
/// to this executable; without them, this executable itself (which compiles the
/// program's bytecode to machine code when it starts).
#[cfg(feature = "compiler")]
fn runtime(want_gui: bool) -> Option<Runtime> {
    let me = std::env::current_exe().ok()?;
    let _ = want_gui;
    #[cfg(feature = "jit")]
    if crate::vm::jit::Jit::aot_supported() {
        // a specific runtime built with the same features as this faston (tests)
        if let Some(rt) = std::env::var_os("FASTON_RT") {
            return Some(Runtime { path: rt.into(), aot: true, gui: cfg!(feature = "gui") });
        }
        let name = if want_gui { "faston-rt-gui" } else { "faston-rt" };
        let rt = me.with_file_name(if cfg!(windows) { format!("{}.exe", name) } else { name.to_string() });
        if rt.is_file() {
            return Some(Runtime { path: rt, aot: true, gui: want_gui });
        }
    }
    Some(Runtime { path: me, aot: false, gui: cfg!(feature = "gui") })
}

/// The standard-library modules (written in Faston) the program can import.
#[cfg(feature = "compiler")]
fn stdlib_imports<'a>(sources: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut todo: Vec<&str> = sources.collect();
    while let Some(s) = todo.pop() {
        for name in imports(s) {
            let first = name.split('.').next().unwrap_or("").to_string();
            if let Some(src) = crate::stdlib::source(&first) {
                if !found.contains(&first) {
                    found.push(first);
                    todo.push(src);
                }
            }
        }
    }
    found.sort();
    found
}

/// Builds `path` into a native executable. Returns the process exit code.
#[cfg(feature = "compiler")]
pub fn build(path: &str, out: Option<&str>, force_gui: bool) -> i32 {
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
    let found = collect(&dir, &src, &mut Vm::new());
    let sources = || std::iter::once(src.as_str()).chain(found.sources.iter().map(|(_, s)| s.as_str()));
    let uses_gui = sources().any(|s| imports(s).iter().any(|n| n == "gui" || n.starts_with("gui.")));
    if uses_gui && !cfg!(feature = "gui") {
        eprintln!("faston: warning: this program imports gui, but this faston was built without it");
    }
    let rt = match runtime(uses_gui || force_gui) {
        Some(r) => r,
        None => {
            eprintln!("faston: can't find the faston executable");
            return 1;
        }
    };

    // compile everything to bytecode (mistakes show up now)
    #[cfg(feature = "jit")]
    let mut vm = Vm::for_build(rt.gui);
    #[cfg(not(feature = "jit"))]
    let mut vm = Vm::new();
    let mut binary = found.compiled.clone();
    match vm.compile_to_bytecode(&main_name, &src) {
        Ok(b) => {
            binary.insert(MAIN_KEY.to_string(), b);
        }
        Err(e) => {
            eprintln!("{}", e);
            return 1;
        }
    }
    for (key, msrc) in found.sources.iter() {
        let name = Path::new(key).file_name().map_or(key.clone(), |n| n.to_string_lossy().into_owned());
        match vm.compile_to_bytecode(&name, msrc) {
            Ok(b) => {
                let fbc = match key.strip_suffix(".fst") {
                    Some(stem) => format!("{}.fbc", stem),
                    None => format!("{}.fbc", key),
                };
                binary.insert(fbc, b);
            }
            Err(e) => {
                eprintln!("File \"{}\": {}", key, e);
                return 1;
            }
        }
    }

    // machine code for the small runtimes: the program's modules and the
    // standard-library modules it may import
    let mut aot: Option<Vec<u8>> = None;
    #[cfg(feature = "jit")]
    if rt.aot {
        let mut keys: Vec<&String> = binary.keys().collect();
        keys.sort();
        let mut mods: Vec<&[u8]> = keys.iter().map(|k| binary[*k].as_slice()).collect();
        for name in stdlib_imports(sources()) {
            if let Some(code) = crate::stdlib::bytecode(&name) {
                mods.push(code);
            }
        }
        match vm.build_aot(&mods) {
            Ok(code) => aot = Some(code),
            Err(e) => {
                eprintln!("{}", e);
                return 1;
            }
        }
    }
    let (native, interpreted) = vm.native_stats().unwrap_or((0, 0));

    let exe = match fs::read(&rt.path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("faston: can't read {}: {}", rt.path.display(), e);
            return 1;
        }
    };
    // a faston binary that already carries a bundle: use only the binary part
    let exe = match strip(&exe) {
        Some(n) => exe[..n].to_vec(),
        None => exe,
    };
    let runtime_size = exe.len();
    let code_size: usize = aot.as_ref().map_or(0, |a| a.len());
    let nmods = binary.len();
    let mut modules = HashMap::new();
    modules.insert(PATH_KEY.to_string(), found.path.clone());
    let payload = encode(&Bundle { main_name, modules, binary, aot });
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
    let mb = |n: usize| n as f64 / (1024.0 * 1024.0);
    if rt.aot {
        println!(
            "built {} ({} module{}, {} function{} compiled to native code{})",
            out.display(),
            nmods,
            if nmods == 1 { "" } else { "s" },
            native,
            if native == 1 { "" } else { "s" },
            if interpreted > 0 { format!(", {} interpreted", interpreted) } else { String::new() }
        );
        println!(
            "  {:.1} MB: {} runtime {:.1} MB + machine code {} KB + bytecode {} KB",
            mb(bytes.len()),
            if rt.gui { "gui" } else { "small" },
            mb(runtime_size),
            code_size.div_ceil(1024),
            (payload.len() - code_size).div_ceil(1024),
        );
    } else {
        println!("built {} ({} module{})", out.display(), nmods, if nmods == 1 { "" } else { "s" });
        println!(
            "  {:.1} MB (full faston as the runtime: it compiles the program to machine code when it starts; put faston-rt{} next to faston for smaller programs, see scripts/)",
            mb(bytes.len()),
            if uses_gui || force_gui { "-gui" } else { "" }
        );
    }
    0
}

/// Length of the binary part of an executable that carries a bundle.
#[cfg(feature = "compiler")]
fn strip(exe: &[u8]) -> Option<usize> {
    let n = exe.len();
    if n < 16 || &exe[n - 8..] != MAGIC {
        return None;
    }
    let size = u64::from_le_bytes(exe[n - 16..n - 8].try_into().unwrap()) as usize;
    n.checked_sub(16 + size)
}
