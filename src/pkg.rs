//! Projects and packages: `faston.toml`, `faston new / add / install / remove`.
//!
//! A project is a directory with a `faston.toml`:
//!
//! ```toml
//! [package]
//! name = "game"
//! version = "0.1.0"
//! main = "main.fst"
//!
//! [dependencies]
//! mathx = "https://github.com/someone/mathx"
//! utils = "../utils"
//! ```
//!
//! Packages are installed into `.faston/packages/<name>/` (a git clone, or a copy
//! of a local directory) and that directory is on `sys.path`, so `import mathx;`
//! finds `.faston/packages/mathx/__init__.fst`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const MANIFEST: &str = "faston.toml";

fn project_root() -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    crate::modules::project_root(&cwd).ok_or_else(|| "no faston.toml here or above (start one with: faston new NAME)".to_string())
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    let v = v.split(" #").next().unwrap_or(v).trim();
    if v.len() >= 2 && ((v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\''))) {
        v[1..v.len() - 1].to_string()
    } else {
        v.to_string()
    }
}

/// `key = "value"` entries of `[section]`.
fn entries(text: &str, section: &str) -> Vec<(String, String)> {
    let mut cur = String::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            cur = t[1..t.len() - 1].trim().to_string();
        } else if cur == section && !t.starts_with('#') {
            if let Some((k, v)) = t.split_once('=') {
                out.push((k.trim().trim_matches('"').to_string(), unquote(v)));
            }
        }
    }
    out
}

fn get(text: &str, section: &str, key: &str) -> Option<String> {
    entries(text, section).into_iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// Sets `key = "value"` in `[section]` (adding the section if needed).
fn set(text: &str, section: &str, key: &str, value: &str) -> String {
    let new_line = format!("{} = \"{}\"", key, value.replace('\\', "/"));
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    let mut cur = String::new();
    let mut last_in_section = None;
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            cur = t[1..t.len() - 1].trim().to_string();
            if cur == section {
                last_in_section = Some(i);
            }
            continue;
        }
        if cur == section {
            if let Some((k, _)) = t.split_once('=') {
                if k.trim().trim_matches('"') == key {
                    lines[i] = new_line;
                    return lines.join("\n") + "\n";
                }
            }
            if !t.is_empty() {
                last_in_section = Some(i);
            }
        }
    }
    match last_in_section {
        Some(i) => lines.insert(i + 1, new_line),
        None => {
            if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push(format!("[{}]", section));
            lines.push(new_line);
        }
    }
    lines.join("\n") + "\n"
}

fn remove_key(text: &str, section: &str, key: &str) -> String {
    let mut cur = String::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            cur = t[1..t.len() - 1].trim().to_string();
        } else if cur == section {
            if let Some((k, _)) = t.split_once('=') {
                if k.trim().trim_matches('"') == key {
                    continue;
                }
            }
        }
        out.push(line.to_string());
    }
    out.join("\n") + "\n"
}

/// The project's main file and name (for `faston run` / `faston build` without a file).
pub fn project_main() -> Result<(String, String), String> {
    let root = project_root()?;
    let text = fs::read_to_string(root.join(MANIFEST)).map_err(|e| e.to_string())?;
    let name = get(&text, "package", "name")
        .unwrap_or_else(|| root.file_name().map_or("app".into(), |n| n.to_string_lossy().into_owned()));
    let main = get(&text, "package", "main").unwrap_or_else(|| "main.fst".into());
    let path = root.join(&main);
    if !path.is_file() {
        return Err(format!("the project's main file {} doesn't exist", path.display()));
    }
    Ok((path.display().to_string(), name))
}

fn valid_name(n: &str) -> bool {
    let mut cs = n.chars();
    matches!(cs.next(), Some(c) if c.is_alphabetic() || c == '_') && cs.all(|c| c.is_alphanumeric() || c == '_')
}

pub fn cmd_new(args: &[String]) -> i32 {
    let lib = args.iter().any(|a| a == "--lib");
    let name = match args.iter().find(|a| !a.starts_with("--")) {
        Some(n) => n.clone(),
        None => {
            eprintln!("usage: faston new NAME [--lib]");
            return 2;
        }
    };
    let dir = PathBuf::from(&name);
    let pkg_name = dir.file_name().map_or(name.clone(), |n| n.to_string_lossy().into_owned());
    if lib && !valid_name(&pkg_name) {
        eprintln!("faston: a library name must be a valid Faston name (letters, digits, _): '{}'", pkg_name);
        return 2;
    }
    if dir.exists() && fs::read_dir(&dir).map(|mut d| d.next().is_some()).unwrap_or(true) {
        eprintln!("faston: '{}' already exists and isn't empty", dir.display());
        return 1;
    }
    let write = |file: &str, text: &str| -> Result<(), String> {
        let p = dir.join(file);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::write(&p, text).map_err(|e| format!("can't write {}: {}", p.display(), e))
    };
    let r = (|| -> Result<(), String> {
        if lib {
            write(
                MANIFEST,
                &format!("[package]\nname = \"{}\"\nversion = \"0.1.0\"\nmain = \"example.fst\"\n\n[dependencies]\n", pkg_name),
            )?;
            write(
                "__init__.fst",
                &format!(
                    "# {name}: a Faston library. Other projects use it with:\n#   faston add <this repo's git URL or folder>\n#   import {name};\n\ndef greet(who) {{\n    return f\"Hello, {{who}}, from {name}!\";\n}}\n",
                    name = pkg_name
                ),
            )?;
            write(
                "example.fst",
                &format!(
                    "# Try the library: faston run\n# (the folder above this one goes on sys.path, so `import {name}` finds this folder)\nimport sys, os;\nsys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(sys.argv[0]))));\nimport {name};\n\nprint({name}.greet(\"world\"));\n",
                    name = pkg_name
                ),
            )?;
        } else {
            write(
                MANIFEST,
                &format!("[package]\nname = \"{}\"\nversion = \"0.1.0\"\nmain = \"main.fst\"\n\n[dependencies]\n", pkg_name),
            )?;
            write(
                "main.fst",
                &format!("# {name}: run it with `faston run`, build it with `faston build`.\nimport tools.text;\n\nprint(tools.text.banner(\"Hello from {name}!\"));\n", name = pkg_name),
            )?;
            write("tools/__init__.fst", "# The `tools` package: modules in this folder are tools.<name>.\n")?;
            write(
                "tools/text.fst",
                "def banner(message) {\n    let line = \"=\" * (len(message) + 4);\n    return f\"{line}\\n| {message} |\\n{line}\";\n}\n",
            )?;
        }
        write(".gitignore", ".faston/\n*.exe\n")?;
        Ok(())
    })();
    match r {
        Ok(()) => {
            println!("created {} '{}' in {}", if lib { "library" } else { "project" }, pkg_name, dir.display());
            println!("  cd {}", dir.display());
            println!("  faston run");
            0
        }
        Err(e) => {
            eprintln!("faston: {}", e);
            1
        }
    }
}

fn copy_dir(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| format!("can't create {}: {}", dst.display(), e))?;
    for e in fs::read_dir(src).map_err(|e| format!("can't read {}: {}", src.display(), e))? {
        let e = e.map_err(|e| e.to_string())?;
        let name = e.file_name();
        if name == ".git" || name == ".faston" {
            continue;
        }
        let to = dst.join(&name);
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            copy_dir(&e.path(), &to)?;
        } else {
            fs::copy(e.path(), &to).map_err(|er| format!("can't copy {}: {}", e.path().display(), er))?;
        }
    }
    Ok(())
}

/// A local directory dependency (paths in faston.toml are relative to the project root).
fn local_dir(root: &Path, source: &str) -> Option<PathBuf> {
    if source.contains("://") || source.starts_with("git@") {
        return None;
    }
    let p = if Path::new(source).is_absolute() { PathBuf::from(source) } else { root.join(source) };
    if p.is_dir() {
        Some(p)
    } else {
        None
    }
}

/// Installs `source` into the project's packages directory as `name`, then the
/// package's own dependencies.
fn install(root: &Path, name: &str, source: &str, depth: usize) -> Result<(), String> {
    let dest = root.join(".faston").join("packages").join(name);
    if dest.exists() {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    match local_dir(root, source) {
        Some(dir) => copy_dir(&dir, &dest)?,
        None => {
            let st = Command::new("git")
                .args(["clone", "--depth", "1", "--quiet", source])
                .arg(&dest)
                .status()
                .map_err(|e| format!("can't run git (is it installed?): {}", e))?;
            if !st.success() {
                let _ = fs::remove_dir_all(&dest);
                return Err(format!("git clone {} failed", source));
            }
            let _ = fs::remove_dir_all(dest.join(".git"));
        }
    }
    println!("  installed {} ({})", name, source);
    if !dest.join("__init__.fst").is_file() {
        println!("  note: {} has no __init__.fst; import its files as {}.<module>", name, name);
    }
    // the package's own dependencies
    if depth < 16 {
        if let Ok(text) = fs::read_to_string(dest.join(MANIFEST)) {
            for (dep, src) in entries(&text, "dependencies") {
                let src = match local_dir(&dest, &src) {
                    Some(p) => p.display().to_string(),
                    None => src,
                };
                install(root, &dep, &src, depth + 1)?;
            }
        }
    }
    Ok(())
}

pub fn cmd_add(args: &[String]) -> i32 {
    let source = match args.first() {
        Some(s) => s.clone(),
        None => {
            eprintln!("usage: faston add <git url | folder> [name]");
            return 2;
        }
    };
    let r = (|| -> Result<String, String> {
        let root = project_root()?;
        let name = match args.get(1) {
            Some(n) => n.clone(),
            None => {
                let last = source.trim_end_matches(['/', '\\']).rsplit(['/', '\\', ':']).next().unwrap_or("").to_string();
                last.strip_suffix(".git").unwrap_or(&last).replace('-', "_")
            }
        };
        if !valid_name(&name) {
            return Err(format!("'{}' isn't a valid package name; pass one: faston add {} NAME", name, source));
        }
        if root.join(".faston").join("packages").join(&name).exists() {
            return Err(format!("'{}' is already installed (faston remove {} first)", name, name));
        }
        // store local paths relative to the project
        let recorded = match Path::new(&source).is_dir().then(|| crate::modules::canon(Path::new(&source))) {
            Some(p) => {
                let r = crate::modules::relative(&root, &p);
                if r.is_empty() { ".".into() } else { r }
            }
            _ => source.clone(),
        };
        install(&root, &name, &recorded, 0)?;
        let path = root.join(MANIFEST);
        let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
        fs::write(&path, set(&text, "dependencies", &name, &recorded)).map_err(|e| e.to_string())?;
        Ok(name)
    })();
    match r {
        Ok(name) => {
            println!("added {} (use it with: import {};)", name, name);
            0
        }
        Err(e) => {
            eprintln!("faston: {}", e);
            1
        }
    }
}

pub fn cmd_install() -> i32 {
    let r = (|| -> Result<usize, String> {
        let root = project_root()?;
        let text = fs::read_to_string(root.join(MANIFEST)).map_err(|e| e.to_string())?;
        let deps = entries(&text, "dependencies");
        for (name, src) in &deps {
            install(&root, name, src, 0)?;
        }
        Ok(deps.len())
    })();
    match r {
        Ok(n) => {
            println!("{} package{} ready", n, if n == 1 { "" } else { "s" });
            0
        }
        Err(e) => {
            eprintln!("faston: {}", e);
            1
        }
    }
}

pub fn cmd_remove(args: &[String]) -> i32 {
    let name = match args.first() {
        Some(n) => n.clone(),
        None => {
            eprintln!("usage: faston remove NAME");
            return 2;
        }
    };
    let r = (|| -> Result<(), String> {
        let root = project_root()?;
        let dest = root.join(".faston").join("packages").join(&name);
        if dest.exists() {
            fs::remove_dir_all(&dest).map_err(|e| format!("can't remove {}: {}", dest.display(), e))?;
        }
        let path = root.join(MANIFEST);
        let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
        fs::write(&path, remove_key(&text, "dependencies", &name)).map_err(|e| e.to_string())?;
        Ok(())
    })();
    match r {
        Ok(()) => {
            println!("removed {}", name);
            0
        }
        Err(e) => {
            eprintln!("faston: {}", e);
            1
        }
    }
}
