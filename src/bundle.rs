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
    /// Machine code compiled at build time, for runtimes without a code generator.
    pub aot: Option<Vec<u8>>,
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
    let mut next = || -> Option<&[u8]> {
        let n = u32::from_le_bytes(data.get(pos..pos + 4)?.try_into().ok()?) as usize;
        pos += 4;
        let s = data.get(pos..pos + n)?;
        pos += n;
        Some(s)
    };
    let text = |b: &[u8]| std::str::from_utf8(b).ok().map(str::to_string);
    let main_name = text(next()?)?;
    let main_src = text(next()?)?;
    let mut modules = HashMap::new();
    let mut aot: Option<Vec<u8>> = None;
    while let Some(name) = next() {
        let body = next()?;
        if name == AOT_KEY.as_bytes() {
            aot = Some(body.to_vec());
        } else {
            modules.insert(text(name)?, text(body)?);
        }
    }
    Some(Bundle { main_name, main_src, modules, aot })
}

fn encode(b: &Bundle) -> Vec<u8> {
    let mut out = Vec::new();
    let mut put = |s: &[u8]| {
        out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        out.extend_from_slice(s);
    };
    put(b.main_name.as_bytes());
    put(b.main_src.as_bytes());
    let mut names: Vec<&String> = b.modules.keys().collect();
    names.sort();
    for n in names {
        put(n.as_bytes());
        put(b.modules[n].as_bytes());
    }
    if let Some(a) = &b.aot {
        put(AOT_KEY.as_bytes());
        put(a);
    }
    out
}

/// Bundle entry holding the program's machine code (binary).
pub const AOT_KEY: &str = "\0aot";

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

/// The runtime a built program starts from.
struct Runtime {
    path: PathBuf,
    /// `faston-rt` / `faston-rt-gui`: no code generator, so the program's machine
    /// code is compiled now and saved in the executable.
    aot: bool,
    gui: bool,
}

/// `faston-rt` (or `faston-rt-gui` for programs that use the gui module) next
/// to this executable; without them, this executable itself (which compiles the
/// program when it starts).
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
    let modules = collect(&dir, &src, &mut Vm::new());
    let sources = || {
        std::iter::once(src.as_str()).chain(modules.iter().filter(|(k, _)| !k.starts_with('\0')).map(|(_, v)| v.as_str()))
    };
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

    // compile everything now so mistakes show up at build time (and, for the
    // small runtimes, keep the machine code)
    #[cfg(feature = "jit")]
    let mut vm = Vm::for_build(rt.gui);
    #[cfg(not(feature = "jit"))]
    let mut vm = Vm::compiled();
    vm.bundle = Some(modules.clone());
    let mut aot: Option<Vec<u8>> = None;
    #[cfg(feature = "jit")]
    if rt.aot {
        match vm.build_native(path, &src, &stdlib_imports(sources())) {
            Ok(code) => aot = Some(code),
            Err(e) => {
                eprintln!("{}", e);
                return 1;
            }
        }
    }
    if aot.is_none() {
        if let Err(e) = vm.check_program(path, &src) {
            eprintln!("{}", e);
            return 1;
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
    let nmods = 1 + modules.keys().filter(|k| !k.starts_with('\0')).count();
    let payload = encode(&Bundle { main_name, main_src: src, modules, aot });
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
        nmods,
        if nmods == 1 { "" } else { "s" },
        native,
        if native == 1 { "" } else { "s" },
        if interpreted > 0 { format!(", {} interpreted", interpreted) } else { String::new() }
    );
    let mb = |n: usize| n as f64 / (1024.0 * 1024.0);
    if rt.aot {
        println!(
            "  {:.1} MB: {} runtime {:.1} MB + machine code {} KB + sources {} KB",
            mb(bytes.len()),
            if rt.gui { "gui" } else { "small" },
            mb(runtime_size),
            code_size.div_ceil(1024),
            (payload.len() - code_size).div_ceil(1024),
        );
    } else {
        println!(
            "  {:.1} MB (full faston as the runtime: it compiles the program when it starts; put faston-rt{} next to faston for smaller programs, see scripts/)",
            mb(bytes.len()),
            if uses_gui || force_gui { "-gui" } else { "" }
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
