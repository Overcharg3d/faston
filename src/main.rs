// a runtime without the code generator leaves parts of the build-time code unused
#![cfg_attr(not(feature = "jit"), allow(dead_code, unused_mut))]

mod builtins;
mod bundle;
mod bytecode;
mod bytesmod;
mod compiler;
mod fileio;
mod fx;
#[cfg(feature = "gui")]
mod gui;
mod json;
mod lexer;
mod modules;
mod natives;
mod ops;
mod osmod;
mod pkg;
mod re;
mod stdlib;
mod value;
mod vm;

use std::io::{self, Write};
use std::{env, fs, process};

use vm::Vm;

// mimalloc is much faster than the system allocator for the many small,
// short-lived strings and lists a dynamic language creates.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Native code recurses on the machine stack, so the program runs on a thread
/// with a large one (only touched pages are actually used).
const STACK_BYTES: usize = 512 << 20;

fn main() {
    let t = std::thread::Builder::new()
        .stack_size(STACK_BYTES)
        .spawn(real_main)
        .expect("faston: failed to start");
    let code = t.join().unwrap_or(101);
    process::exit(code);
}

fn usage() {
    println!("Usage: faston [file.fst] [args...]        run a program (no file: interactive prompt)");
    println!("       faston run [--native] [file.fst]    run it (--native: compile to machine code first;");
    println!("                                           no file: the project's main file)");
    println!("       faston build [file.fst] [-o name]   build a standalone native executable");
    println!("                    [--full]               (--full: include the gui runtime even if unused)");
    println!("       faston compile file.fst [-o f.fbc]  compile to a bytecode file (run it with: faston f.fbc)");
    println!("       faston --dis file.fst               show the compiled bytecode (of a .fst or .fbc file)");
    println!();
    println!("Projects:");
    println!("       faston new name [--lib]             create a project (or a library package)");
    println!("       faston add <git url | path> [name]  install a package into .faston/packages");
    println!("       faston install                      install every package listed in faston.toml");
    println!("       faston remove name                  uninstall a package");
}

fn real_main() -> i32 {
    if let Some(b) = bundle::embedded() {
        // a built executable: run the bundled program as native code, either the
        // machine code saved in it or (full runtime) compiled now
        let mut vm = match &b.aot {
            Some(code) => {
                let mut vm = Vm::new();
                if vm.load_aot(code) {
                    vm
                } else if cfg!(feature = "jit") {
                    Vm::compiled()
                } else {
                    if env::var_os("FASTON_JIT_DEBUG").is_some() {
                        eprintln!("[aot] saved machine code doesn't fit this runtime: interpreting");
                    }
                    vm
                }
            }
            None => Vm::compiled(),
        };
        let path: Vec<std::path::PathBuf> = b
            .modules
            .get(bundle::PATH_KEY)
            .map(|p| p.lines().map(std::path::PathBuf::from).collect())
            .unwrap_or_default();
        vm.bundle = Some(b.modules);
        vm.set_argv(env::args().collect());
        vm.set_sys_path(path);
        return report(vm.run_file(&b.main_name, &b.main_src));
    }
    let args: Vec<String> = env::args().collect();
    let native_env = env::var_os("FASTON_NATIVE").is_some_and(|v| v != "0");
    match args.get(1).map(String::as_str) {
        None => repl(),
        Some("-v") | Some("--version") => println!("Faston {}", VERSION),
        Some("-h") | Some("--help") => usage(),
        Some("--dis") => {
            let path = args.get(2).unwrap_or_else(|| {
                eprintln!("faston: --dis needs a file");
                process::exit(2);
            });
            let listing = match fs::read(path) {
                Ok(b) if b.starts_with(vm::fbc::MAGIC) => Vm::new().disassemble_bytecode(&b),
                _ => Vm::new().disassemble(&read_file(path)),
            };
            match listing {
                Ok(listing) => print!("{}", listing),
                Err(e) => {
                    eprintln!("{}", e);
                    return 1;
                }
            }
        }
        Some("run") => {
            let mut native = native_env;
            let mut path = None;
            let mut rest = Vec::new();
            // options come before the file; everything after it goes to the program
            for a in &args[2..] {
                match (a.as_str(), &path) {
                    ("--native" | "-n", None) => native = true,
                    ("--interp" | "-i", None) => native = false,
                    (p, None) => path = Some(p.to_string()),
                    (p, Some(_)) => rest.push(p.to_string()),
                }
            }
            let path = match path {
                Some(p) => p,
                None => match pkg::project_main() {
                    Ok((p, _)) => p,
                    Err(e) => {
                        eprintln!("faston: run needs a file ({})", e);
                        process::exit(2);
                    }
                },
            };
            return run(&path, native, rest);
        }
        Some("build") => {
            let mut out = None;
            let mut path = None;
            let mut full = false;
            let mut it = args[2..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "-o" => out = it.next().cloned(),
                    "--full" => full = true,
                    p if path.is_none() => path = Some(p.to_string()),
                    _ => {}
                }
            }
            let path = match path {
                Some(p) => p,
                None => match pkg::project_main() {
                    Ok((p, name)) => {
                        if out.is_none() {
                            out = Some(name);
                        }
                        p
                    }
                    Err(e) => {
                        eprintln!("faston: build needs a file ({})", e);
                        process::exit(2);
                    }
                },
            };
            return bundle::build(&path, out.as_deref(), full);
        }
        Some("compile") => {
            let mut out = None;
            let mut path = None;
            let mut it = args[2..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "-o" => out = it.next().cloned(),
                    p if path.is_none() => path = Some(p.to_string()),
                    _ => {}
                }
            }
            let Some(path) = path else {
                eprintln!("faston: compile needs a file");
                return 2;
            };
            let src = read_file(&path);
            let name = std::path::Path::new(&path).file_name().map_or(path.clone(), |n| n.to_string_lossy().into_owned());
            let out = out.unwrap_or_else(|| std::path::Path::new(&path).with_extension("fbc").to_string_lossy().into_owned());
            let mut vm = Vm::new();
            match vm.compile_to_bytecode(&name, &src) {
                Ok(b) => {
                    if let Err(e) = fs::write(&out, &b) {
                        eprintln!("faston: can't write {}: {}", out, e);
                        return 1;
                    }
                    println!("compiled {} -> {} ({} bytes)", path, out, b.len());
                }
                Err(e) => {
                    eprintln!("{}", e);
                    return 1;
                }
            }
        }
        Some("new") => return pkg::cmd_new(&args[2..]),
        Some("add") => return pkg::cmd_add(&args[2..]),
        Some("install") => return pkg::cmd_install(),
        Some("remove") => return pkg::cmd_remove(&args[2..]),
        Some(path) => return run(path, native_env, args[2..].to_vec()),
    }
    0
}

fn run(path: &str, native: bool, program_args: Vec<String>) -> i32 {
    let bytes = fs::read(path).unwrap_or_else(|e| {
        eprintln!("faston: can't open '{}': {}", path, e);
        process::exit(2);
    });
    let mut vm = if native { Vm::compiled() } else { Vm::new() };
    let mut argv = vec![path.to_string()];
    argv.extend(program_args);
    vm.set_argv(argv);
    let dir = std::path::Path::new(path).parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(std::path::Path::new("."));
    vm.set_sys_path(modules::default_path(dir));
    if bytes.starts_with(vm::fbc::MAGIC) {
        return report(vm.run_bytecode_file(path, &bytes));
    }
    let src = String::from_utf8(bytes).unwrap_or_else(|_| {
        eprintln!("faston: '{}' is not UTF-8 text", path);
        process::exit(2);
    });
    report(vm.run_file(path, &src))
}

fn report(r: Result<(), String>) -> i32 {
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{}", e);
            1
        }
    }
}

fn read_file(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("faston: can't open '{}': {}", path, e);
        process::exit(2);
    })
}

/// Net count of open brackets, ignoring strings and comments.
fn open_depth(src: &str) -> i32 {
    let mut depth = 0;
    let mut quote: Option<char> = None;
    let mut chars = src.chars();
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            if c == '\\' {
                chars.next();
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '#' => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
    }
    depth
}

fn repl() {
    println!("Faston {} — type 'exit' to quit", VERSION);
    let mut vm = Vm::new();
    vm.set_sys_path(modules::default_path(std::path::Path::new(".")));
    let stdin = io::stdin();
    let mut buf = String::new();
    loop {
        print!("{}", if buf.is_empty() { ">>> " } else { "... " });
        let _ = io::stdout().flush();
        let mut line = String::new();
        if stdin.read_line(&mut line).unwrap_or(0) == 0 {
            println!();
            break;
        }
        if buf.is_empty() && matches!(line.trim(), "exit" | "exit()" | "quit") {
            break;
        }
        buf.push_str(&line);
        if open_depth(&buf) > 0 {
            continue;
        }
        let src = std::mem::take(&mut buf);
        let trimmed = src.trim();
        if trimmed.is_empty() {
            continue;
        }

        // A bare expression without ';' gets its value echoed, like Python's prompt.
        let result = if !trimmed.ends_with(';') && !trimmed.ends_with('}') {
            let echo = format!(
                "__repl__ = ({});\nif (__repl__ != None) {{ print(repr(__repl__)); }}",
                trimmed
            );
            match vm.run_source(&echo) {
                Err(e) if e.starts_with("[line") => vm.run_source(&format!("{};", trimmed)),
                other => other,
            }
        } else {
            vm.run_source(&src)
        };
        if let Err(e) = result {
            eprintln!("{}", e);
        }
    }
}
