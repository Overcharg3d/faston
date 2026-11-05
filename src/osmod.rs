//! The `sys`, `os` (with `os.path`) and `shutil` modules.

use std::io::Write;
use std::path::{Component, Path, PathBuf};

use crate::fileio::{self, os_error, Stream};
use crate::value::*;
use crate::vm::Vm;

type R = Result<Value, String>;

fn nargs(name: &str, args: &[Value], min: usize, max: usize) -> Result<(), String> {
    if args.len() < min || args.len() > max {
        let expected = if min == max { format!("{}", min) } else { format!("{} to {}", min, max) };
        return Err(format!("TypeError: {}() takes {} argument{} ({} given)", name, expected, if max == 1 { "" } else { "s" }, args.len()));
    }
    Ok(())
}

fn path_arg<'a>(v: Value, f: &str) -> Result<&'a str, String> {
    v.as_str()
        .ok_or_else(|| format!("TypeError: {}() path must be a str, not '{}'", f, v.type_name()))
}

fn bool_kw(vm: &mut Vm, name: u32) -> Result<bool, String> {
    match vm.take_kw(name) {
        Some(v) => vm.truthy(v),
        None => Ok(false),
    }
}

fn s(vm: &mut Vm, text: impl Into<String>) -> Value {
    vm.heap.string(text.into())
}

pub fn register(vm: &mut Vm) {
    register_sys(vm);
    register_os(vm);
    register_shutil(vm);
}

// ---------------------------------------------------------------- sys

fn platform() -> &'static str {
    if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    }
}

fn register_sys(vm: &mut Vm) {
    let stdin = fileio::new_file(vm, "<stdin>", "r", Stream::Stdin);
    let stdout = fileio::new_file(vm, "<stdout>", "w", Stream::Stdout);
    let stderr = fileio::new_file(vm, "<stderr>", "w", Stream::Stderr);
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
    let version = format!("{} (Faston)", env!("CARGO_PKG_VERSION"));
    let mut m: Vec<(&str, Value)> = vec![
        ("stdin", stdin),
        ("stdout", stdout),
        ("stderr", stderr),
        ("platform", s(vm, platform())),
        ("version", s(vm, version)),
        ("executable", s(vm, exe)),
        ("maxsize", Value::Int(i64::MAX)),
    ];
    // argv and path are filled in when the program starts (Vm::set_argv / set_sys_path)
    let argv = vm.heap.list(Vec::new());
    m.push(("argv", argv));
    let path = vm.heap.list(Vec::new());
    m.push(("path", path));
    m.push(("exit", vm.native("exit", sys_exit)));
    vm.define_module("sys", m);
}

fn sys_exit(vm: &mut Vm, args: &[Value]) -> R {
    nargs("exit", args, 0, 1)?;
    vm.flush_all();
    let code = match args.first() {
        None | Some(Value::None) => 0,
        Some(Value::Int(c)) => *c as i32,
        Some(v) => {
            let msg = vm.str_of(*v)?;
            eprintln!("{}", msg);
            1
        }
    };
    std::process::exit(code)
}

// ---------------------------------------------------------------- os

fn register_os(vm: &mut Vm) {
    let fns: &[(&'static str, NativeFn)] = &[
        ("getcwd", os_getcwd),
        ("chdir", os_chdir),
        ("listdir", os_listdir),
        ("mkdir", os_mkdir),
        ("rmdir", os_rmdir),
        ("remove", os_remove),
        ("unlink", os_remove),
        ("rename", os_rename),
        ("replace", os_rename),
        ("getenv", os_getenv),
        ("putenv", os_putenv),
        ("unsetenv", os_unsetenv),
        ("system", os_system),
        ("cpu_count", os_cpu_count),
        ("getpid", os_getpid),
        ("walk", os_walk),
    ];
    let mut m: Vec<(&str, Value)> = fns.iter().map(|&(n, f)| (n, vm.native(n, f))).collect();
    let makedirs = Value::Obj(vm.heap.alloc(Obj::Native(Native { name: "makedirs", func: os_makedirs, kwargs: &[sym::EXIST_OK] })));
    m.push(("makedirs", makedirs));
    m.push(("name", s(vm, if cfg!(windows) { "nt" } else { "posix" })));
    m.push(("sep", s(vm, std::path::MAIN_SEPARATOR.to_string())));
    m.push(("linesep", s(vm, if cfg!(windows) { "\r\n" } else { "\n" })));
    m.push(("curdir", s(vm, ".")));
    m.push(("pardir", s(vm, "..")));
    // environ: a snapshot dict (use putenv/unsetenv to change the real environment)
    let mut env = Dict::default();
    for (k, v) in std::env::vars() {
        let (kv, vv) = (s(vm, k), s(vm, v));
        if let Ok(key) = to_key(kv) {
            env.insert(key, kv, vv);
        }
    }
    m.push(("environ", vm.heap.dict(env)));

    let pfns: &[(&'static str, NativeFn)] = &[
        ("join", path_join),
        ("exists", path_exists),
        ("isfile", path_isfile),
        ("isdir", path_isdir),
        ("isabs", path_isabs),
        ("basename", path_basename),
        ("dirname", path_dirname),
        ("split", path_split),
        ("splitext", path_splitext),
        ("abspath", path_abspath),
        ("normpath", path_normpath),
        ("realpath", path_realpath),
        ("expanduser", path_expanduser),
        ("getsize", path_getsize),
        ("getmtime", path_getmtime),
        ("relpath", path_relpath),
    ];
    let mut p: Vec<(&str, Value)> = pfns.iter().map(|&(n, f)| (n, vm.native(n, f))).collect();
    p.push(("sep", s(vm, std::path::MAIN_SEPARATOR.to_string())));
    vm.define_module("os.path", p);
    let path_mod = vm.module("os.path").expect("os.path registered");
    m.push(("path", path_mod));
    vm.define_module("os", m);
}

fn os_getcwd(vm: &mut Vm, args: &[Value]) -> R {
    nargs("getcwd", args, 0, 0)?;
    let d = std::env::current_dir().map_err(|e| os_error(&e, ""))?;
    Ok(s(vm, d.display().to_string()))
}

fn os_chdir(_: &mut Vm, args: &[Value]) -> R {
    nargs("chdir", args, 1, 1)?;
    let p = path_arg(args[0], "chdir")?;
    std::env::set_current_dir(p).map_err(|e| os_error(&e, p))?;
    Ok(Value::None)
}

fn os_listdir(vm: &mut Vm, args: &[Value]) -> R {
    nargs("listdir", args, 0, 1)?;
    let p = match args.first() {
        Some(v) => path_arg(*v, "listdir")?,
        None => ".",
    };
    let mut names: Vec<String> = std::fs::read_dir(p)
        .map_err(|e| os_error(&e, p))?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let vals = names.into_iter().map(|n| s(vm, n)).collect();
    Ok(vm.heap.list(vals))
}

fn os_mkdir(_: &mut Vm, args: &[Value]) -> R {
    nargs("mkdir", args, 1, 1)?;
    let p = path_arg(args[0], "mkdir")?;
    std::fs::create_dir(p).map_err(|e| os_error(&e, p))?;
    Ok(Value::None)
}

fn os_makedirs(vm: &mut Vm, args: &[Value]) -> R {
    nargs("makedirs", args, 1, 1)?;
    let p = path_arg(args[0], "makedirs")?;
    let exist_ok = bool_kw(vm, sym::EXIST_OK)?;
    if Path::new(p).is_dir() {
        if exist_ok {
            return Ok(Value::None);
        }
        return Err(format!("FileExistsError: [Errno 17] File exists: '{}'", p));
    }
    std::fs::create_dir_all(p).map_err(|e| os_error(&e, p))?;
    Ok(Value::None)
}

fn os_rmdir(_: &mut Vm, args: &[Value]) -> R {
    nargs("rmdir", args, 1, 1)?;
    let p = path_arg(args[0], "rmdir")?;
    std::fs::remove_dir(p).map_err(|e| os_error(&e, p))?;
    Ok(Value::None)
}

fn os_remove(_: &mut Vm, args: &[Value]) -> R {
    nargs("remove", args, 1, 1)?;
    let p = path_arg(args[0], "remove")?;
    if Path::new(p).is_dir() {
        return Err(format!("IsADirectoryError: [Errno 21] Is a directory: '{}'", p));
    }
    std::fs::remove_file(p).map_err(|e| os_error(&e, p))?;
    Ok(Value::None)
}

fn os_rename(_: &mut Vm, args: &[Value]) -> R {
    nargs("rename", args, 2, 2)?;
    let (a, b) = (path_arg(args[0], "rename")?, path_arg(args[1], "rename")?);
    std::fs::rename(a, b).map_err(|e| os_error(&e, a))?;
    Ok(Value::None)
}

fn os_getenv(vm: &mut Vm, args: &[Value]) -> R {
    nargs("getenv", args, 1, 2)?;
    let k = path_arg(args[0], "getenv")?;
    match std::env::var(k) {
        Ok(v) => Ok(s(vm, v)),
        Err(_) => Ok(args.get(1).copied().unwrap_or(Value::None)),
    }
}

fn os_putenv(_: &mut Vm, args: &[Value]) -> R {
    nargs("putenv", args, 2, 2)?;
    let (k, v) = (path_arg(args[0], "putenv")?, path_arg(args[1], "putenv")?);
    if k.is_empty() || k.contains('=') || k.contains('\0') || v.contains('\0') {
        return Err("ValueError: illegal environment variable name".into());
    }
    std::env::set_var(k, v);
    Ok(Value::None)
}

fn os_unsetenv(_: &mut Vm, args: &[Value]) -> R {
    nargs("unsetenv", args, 1, 1)?;
    let k = path_arg(args[0], "unsetenv")?;
    if k.is_empty() || k.contains('=') || k.contains('\0') {
        return Err("ValueError: illegal environment variable name".into());
    }
    std::env::remove_var(k);
    Ok(Value::None)
}

fn os_system(vm: &mut Vm, args: &[Value]) -> R {
    nargs("system", args, 1, 1)?;
    let cmd = path_arg(args[0], "system")?;
    vm.flush_all();
    let status = if cfg!(windows) {
        std::process::Command::new("cmd").args(["/C", cmd]).status()
    } else {
        std::process::Command::new("sh").args(["-c", cmd]).status()
    };
    let st = status.map_err(|e| os_error(&e, ""))?;
    Ok(Value::Int(st.code().unwrap_or(-1) as i64))
}

fn os_cpu_count(_: &mut Vm, args: &[Value]) -> R {
    nargs("cpu_count", args, 0, 0)?;
    Ok(std::thread::available_parallelism().map_or(Value::None, |n| Value::Int(n.get() as i64)))
}

fn os_getpid(_: &mut Vm, args: &[Value]) -> R {
    nargs("getpid", args, 0, 0)?;
    Ok(Value::Int(std::process::id() as i64))
}

/// `os.walk(top)`: a list of (dirpath, dirnames, filenames), top-down.
fn os_walk(vm: &mut Vm, args: &[Value]) -> R {
    nargs("walk", args, 1, 1)?;
    let top = path_arg(args[0], "walk")?.to_string();
    let mut out = Vec::new();
    let mut todo = vec![PathBuf::from(&top)];
    let mut first = true;
    while let Some(dir) = todo.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) if !first => continue,
            Err(_) => break, // like Python: a missing top yields nothing
        };
        first = false;
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        for e in rd.filter_map(|e| e.ok()) {
            let name = e.file_name().to_string_lossy().into_owned();
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                dirs.push(name);
            } else {
                files.push(name);
            }
        }
        dirs.sort();
        files.sort();
        for d in dirs.iter().rev() {
            todo.push(dir.join(d));
        }
        let dv = dirs.into_iter().map(|d| s(vm, d)).collect();
        let fv = files.into_iter().map(|f| s(vm, f)).collect();
        let entry = vec![s(vm, dir.display().to_string()), vm.heap.list(dv), vm.heap.list(fv)];
        out.push(vm.heap.tuple(entry));
    }
    Ok(vm.heap.list(out))
}

// ---------------------------------------------------------------- os.path

fn is_sep(c: char) -> bool {
    c == '/' || (cfg!(windows) && c == '\\')
}

fn path_join(vm: &mut Vm, args: &[Value]) -> R {
    if args.is_empty() {
        return Err("TypeError: join() takes at least 1 argument (0 given)".into());
    }
    let mut out = String::new();
    for a in args {
        let p = path_arg(*a, "join")?;
        if Path::new(p).is_absolute() || p.starts_with('/') || (cfg!(windows) && p.starts_with('\\')) {
            out = p.to_string();
        } else if out.is_empty() || out.ends_with(is_sep) {
            out.push_str(p);
        } else {
            out.push(std::path::MAIN_SEPARATOR);
            out.push_str(p);
        }
    }
    Ok(s(vm, out))
}

fn path_test(args: &[Value], f: &str, test: fn(&Path) -> bool) -> R {
    nargs(f, args, 1, 1)?;
    let p = path_arg(args[0], f)?;
    Ok(Value::Bool(test(Path::new(p))))
}

fn path_exists(_: &mut Vm, args: &[Value]) -> R {
    path_test(args, "exists", |p| p.exists())
}
fn path_isfile(_: &mut Vm, args: &[Value]) -> R {
    path_test(args, "isfile", |p| p.is_file())
}
fn path_isdir(_: &mut Vm, args: &[Value]) -> R {
    path_test(args, "isdir", |p| p.is_dir())
}
fn path_isabs(_: &mut Vm, args: &[Value]) -> R {
    path_test(args, "isabs", |p| p.is_absolute() || p.to_string_lossy().starts_with('/'))
}

/// (head, tail) split at the last separator, like Python's os.path.split.
fn split_path(p: &str) -> (String, String) {
    match p.rfind(is_sep) {
        None => (String::new(), p.to_string()),
        Some(i) => {
            let head = &p[..i + 1];
            let tail = &p[i + 1..];
            // strip trailing separators from head, unless it is all separators (the root)
            let trimmed = head.trim_end_matches(is_sep);
            let head = if trimmed.is_empty() || trimmed.ends_with(':') { head } else { trimmed };
            (head.to_string(), tail.to_string())
        }
    }
}

fn path_basename(vm: &mut Vm, args: &[Value]) -> R {
    nargs("basename", args, 1, 1)?;
    let (_, tail) = split_path(path_arg(args[0], "basename")?);
    Ok(s(vm, tail))
}

fn path_dirname(vm: &mut Vm, args: &[Value]) -> R {
    nargs("dirname", args, 1, 1)?;
    let (head, _) = split_path(path_arg(args[0], "dirname")?);
    Ok(s(vm, head))
}

fn path_split(vm: &mut Vm, args: &[Value]) -> R {
    nargs("split", args, 1, 1)?;
    let (head, tail) = split_path(path_arg(args[0], "split")?);
    let t = vec![s(vm, head), s(vm, tail)];
    Ok(vm.heap.tuple(t))
}

fn path_splitext(vm: &mut Vm, args: &[Value]) -> R {
    nargs("splitext", args, 1, 1)?;
    let p = path_arg(args[0], "splitext")?;
    let base_start = p.rfind(is_sep).map_or(0, |i| i + 1);
    let base = &p[base_start..];
    // the extension starts at the last dot that isn't part of leading dots
    let lead = base.len() - base.trim_start_matches('.').len();
    let (root, ext) = match base[lead..].rfind('.') {
        Some(i) => {
            let at = base_start + lead + i;
            (&p[..at], &p[at..])
        }
        None => (p, ""),
    };
    let t = vec![s(vm, root), s(vm, ext)];
    Ok(vm.heap.tuple(t))
}

/// Lexical normalization: collapses separators, "." and "a/.." (like os.path.normpath).
fn normpath(p: &str) -> String {
    if p.is_empty() {
        return ".".into();
    }
    let path = Path::new(p);
    let mut prefix = String::new();
    let mut absolute = false;
    let mut parts: Vec<String> = Vec::new();
    for c in path.components() {
        match c {
            Component::Prefix(px) => prefix = px.as_os_str().to_string_lossy().into_owned(),
            Component::RootDir => absolute = true,
            Component::CurDir => {}
            Component::ParentDir => {
                if parts.last().is_some_and(|l| l != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push("..".into());
                }
            }
            Component::Normal(n) => parts.push(n.to_string_lossy().into_owned()),
        }
    }
    let sep = std::path::MAIN_SEPARATOR.to_string();
    let mut out = prefix;
    if absolute {
        out.push_str(&sep);
    }
    out.push_str(&parts.join(&sep));
    if out.is_empty() {
        ".".into()
    } else {
        out
    }
}

fn abspath(p: &str) -> Result<String, String> {
    let full = if Path::new(p).is_absolute() {
        PathBuf::from(p)
    } else {
        std::env::current_dir().map_err(|e| os_error(&e, ""))?.join(p)
    };
    Ok(normpath(&full.to_string_lossy()))
}

fn path_abspath(vm: &mut Vm, args: &[Value]) -> R {
    nargs("abspath", args, 1, 1)?;
    let a = abspath(path_arg(args[0], "abspath")?)?;
    Ok(s(vm, a))
}

fn path_normpath(vm: &mut Vm, args: &[Value]) -> R {
    nargs("normpath", args, 1, 1)?;
    let n = normpath(path_arg(args[0], "normpath")?);
    Ok(s(vm, n))
}

fn path_realpath(vm: &mut Vm, args: &[Value]) -> R {
    nargs("realpath", args, 1, 1)?;
    let p = path_arg(args[0], "realpath")?;
    let r = match std::fs::canonicalize(p) {
        Ok(c) => {
            let t = c.to_string_lossy().into_owned();
            // drop Windows' verbatim prefix
            t.strip_prefix(r"\\?\").map(String::from).unwrap_or(t)
        }
        Err(_) => abspath(p)?,
    };
    Ok(s(vm, r))
}

fn home_dir() -> Option<String> {
    std::env::var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).ok()
}

fn path_expanduser(vm: &mut Vm, args: &[Value]) -> R {
    nargs("expanduser", args, 1, 1)?;
    let p = path_arg(args[0], "expanduser")?;
    let r = match (p.strip_prefix('~'), home_dir()) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with(is_sep) => format!("{}{}", home, rest),
        _ => p.to_string(),
    };
    Ok(s(vm, r))
}

fn path_getsize(_: &mut Vm, args: &[Value]) -> R {
    nargs("getsize", args, 1, 1)?;
    let p = path_arg(args[0], "getsize")?;
    let m = std::fs::metadata(p).map_err(|e| os_error(&e, p))?;
    Ok(Value::Int(m.len() as i64))
}

fn path_getmtime(_: &mut Vm, args: &[Value]) -> R {
    nargs("getmtime", args, 1, 1)?;
    let p = path_arg(args[0], "getmtime")?;
    let m = std::fs::metadata(p).map_err(|e| os_error(&e, p))?;
    let t = m.modified().map_err(|e| os_error(&e, p))?;
    let secs = t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    Ok(Value::Float(secs))
}

fn path_relpath(vm: &mut Vm, args: &[Value]) -> R {
    nargs("relpath", args, 1, 2)?;
    let target = abspath(path_arg(args[0], "relpath")?)?;
    let start = match args.get(1) {
        Some(v) => abspath(path_arg(*v, "relpath")?)?,
        None => abspath(".")?,
    };
    let comps = |p: &str| -> Vec<String> {
        Path::new(p).components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect()
    };
    let (t, st) = (comps(&target), comps(&start));
    let same = |a: &String, b: &String| if cfg!(windows) { a.eq_ignore_ascii_case(b) } else { a == b };
    if !t.is_empty() && !st.is_empty() && !same(&t[0], &st[0]) {
        return Err(format!("ValueError: path is on mount '{}', start on mount '{}'", t[0], st[0]));
    }
    let common = t.iter().zip(st.iter()).take_while(|(a, b)| same(a, b)).count();
    let mut parts: Vec<String> = std::iter::repeat_n("..".to_string(), st.len() - common).collect();
    parts.extend(t[common..].iter().cloned());
    let r = if parts.is_empty() { ".".to_string() } else { parts.join(&std::path::MAIN_SEPARATOR.to_string()) };
    Ok(s(vm, r))
}

// ---------------------------------------------------------------- shutil

fn register_shutil(vm: &mut Vm) {
    let fns: &[(&'static str, NativeFn)] = &[
        ("copyfile", sh_copyfile),
        ("copy", sh_copy),
        ("move", sh_move),
        ("rmtree", sh_rmtree),
        ("copytree", sh_copytree),
        ("which", sh_which),
    ];
    let m = fns.iter().map(|&(n, f)| (n, vm.native(n, f))).collect();
    vm.define_module("shutil", m);
}

fn sh_copyfile(_: &mut Vm, args: &[Value]) -> R {
    nargs("copyfile", args, 2, 2)?;
    let (a, b) = (path_arg(args[0], "copyfile")?, path_arg(args[1], "copyfile")?);
    std::fs::copy(a, b).map_err(|e| os_error(&e, a))?;
    Ok(args[1])
}

fn sh_copy(vm: &mut Vm, args: &[Value]) -> R {
    nargs("copy", args, 2, 2)?;
    let (a, b) = (path_arg(args[0], "copy")?, path_arg(args[1], "copy")?);
    let mut dst = PathBuf::from(b);
    if dst.is_dir() {
        if let Some(name) = Path::new(a).file_name() {
            dst = dst.join(name);
        }
    }
    std::fs::copy(a, &dst).map_err(|e| os_error(&e, a))?;
    Ok(s(vm, dst.display().to_string()))
}

fn sh_move(vm: &mut Vm, args: &[Value]) -> R {
    nargs("move", args, 2, 2)?;
    let (a, b) = (path_arg(args[0], "move")?, path_arg(args[1], "move")?);
    let mut dst = PathBuf::from(b);
    if dst.is_dir() {
        if let Some(name) = Path::new(a).file_name() {
            dst = dst.join(name);
        }
    }
    if std::fs::rename(a, &dst).is_err() {
        // across devices: copy, then delete
        if Path::new(a).is_dir() {
            copy_tree(Path::new(a), &dst)?;
            std::fs::remove_dir_all(a).map_err(|e| os_error(&e, a))?;
        } else {
            std::fs::copy(a, &dst).map_err(|e| os_error(&e, a))?;
            std::fs::remove_file(a).map_err(|e| os_error(&e, a))?;
        }
    }
    Ok(s(vm, dst.display().to_string()))
}

fn sh_rmtree(_: &mut Vm, args: &[Value]) -> R {
    nargs("rmtree", args, 1, 1)?;
    let p = path_arg(args[0], "rmtree")?;
    std::fs::remove_dir_all(p).map_err(|e| os_error(&e, p))?;
    Ok(Value::None)
}

fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| os_error(&e, &dst.display().to_string()))?;
    for e in std::fs::read_dir(src).map_err(|e| os_error(&e, &src.display().to_string()))? {
        let e = e.map_err(|e| os_error(&e, ""))?;
        let to = dst.join(e.file_name());
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            copy_tree(&e.path(), &to)?;
        } else {
            std::fs::copy(e.path(), &to).map_err(|er| os_error(&er, &e.path().display().to_string()))?;
        }
    }
    Ok(())
}

fn sh_copytree(vm: &mut Vm, args: &[Value]) -> R {
    nargs("copytree", args, 2, 2)?;
    let (a, b) = (path_arg(args[0], "copytree")?, path_arg(args[1], "copytree")?);
    if Path::new(b).exists() {
        return Err(format!("FileExistsError: [Errno 17] File exists: '{}'", b));
    }
    copy_tree(Path::new(a), Path::new(b))?;
    Ok(s(vm, b))
}

fn sh_which(vm: &mut Vm, args: &[Value]) -> R {
    nargs("which", args, 1, 1)?;
    let name = path_arg(args[0], "which")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT").unwrap_or(".EXE;.CMD;.BAT".into()).split(';').map(|e| e.to_string()).collect()
    } else {
        vec![String::new()]
    };
    let paths = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&paths) {
        for ext in std::iter::once(String::new()).chain(exts.iter().cloned()) {
            let p = dir.join(format!("{}{}", name, ext));
            if p.is_file() {
                return Ok(s(vm, p.display().to_string()));
            }
        }
    }
    Ok(Value::None)
}

/// print(..., file=f): writes through the file's stream (or the object's write()).
pub fn print_to(vm: &mut Vm, file: Value, text: &str, flush: bool) -> Result<(), String> {
    if let Value::Obj(g) = file {
        if let Obj::Handle(h) = g.get() {
            if let Some(f) = h.get::<fileio::FileObj>() {
                let stream = f.stream.clone();
                fileio::write_text(vm, &mut stream.borrow_mut(), text)?;
                if flush {
                    match &mut *stream.borrow_mut() {
                        Stream::Stdout => {
                            let _ = vm.out.flush();
                        }
                        other => fileio::flush_stream(other),
                    }
                }
                return Ok(());
            }
        }
    }
    // any object with a write(text) method
    let write = vm.syms.intern("write");
    let m = vm.get_attr(file, write)?;
    let t = vm.heap.string(text);
    vm.call(m, &[t])?;
    if flush {
        let fl = vm.syms.intern("flush");
        if let Ok(m) = vm.get_attr(file, fl) {
            vm.call(m, &[])?;
        }
    }
    Ok(())
}
