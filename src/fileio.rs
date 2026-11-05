//! Files: `open()`, file objects, and the standard streams (`sys.stdout`, ...).
//!
//! Files are UTF-8 text. Reading translates "\r\n" and "\r" to "\n" (like
//! Python's universal newlines); writing writes exactly what you give it, so a
//! program produces the same bytes on every OS.

use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::rc::Rc;

use crate::value::*;
use crate::vm::Vm;

type R = Result<Value, String>;

pub enum Stream {
    Reader(BufReader<File>),
    Writer(BufWriter<File>),
    /// Read and write ("r+", "w+", "a+"), unbuffered.
    Update(File),
    Stdin,
    Stdout,
    Stderr,
    Closed,
}

pub struct FileObj {
    pub name: String,
    pub mode: String,
    pub stream: Rc<RefCell<Stream>>,
    /// Opened with 'b': reads give bytes, writes take bytes.
    pub binary: bool,
}

pub static FILE_TYPE: HandleType = HandleType {
    name: "file",
    method: file_method,
    attr: file_attr,
    repr: file_repr,
};

pub fn flush_stream(s: &mut Stream) {
    match s {
        Stream::Writer(w) => {
            let _ = w.flush();
        }
        Stream::Update(f) => {
            let _ = f.flush();
        }
        _ => {}
    }
}

/// Python-style OSError subclass + message for an I/O error.
pub fn os_error(e: &io::Error, path: &str) -> String {
    use io::ErrorKind::*;
    let (cls, errno, msg) = match e.kind() {
        NotFound => ("FileNotFoundError", 2, "No such file or directory".to_string()),
        PermissionDenied => ("PermissionError", 13, "Permission denied".to_string()),
        AlreadyExists => ("FileExistsError", 17, "File exists".to_string()),
        IsADirectory => ("IsADirectoryError", 21, "Is a directory".to_string()),
        NotADirectory => ("NotADirectoryError", 20, "Not a directory".to_string()),
        DirectoryNotEmpty => ("OSError", 39, "Directory not empty".to_string()),
        TimedOut => ("TimeoutError", 110, "Timed out".to_string()),
        _ => ("OSError", e.raw_os_error().unwrap_or(0), e.to_string()),
    };
    if path.is_empty() {
        format!("{}: [Errno {}] {}", cls, errno, msg)
    } else {
        format!("{}: [Errno {}] {}: '{}'", cls, errno, msg, path)
    }
}

fn closed() -> String {
    "ValueError: I/O operation on closed file.".into()
}

fn not_readable() -> String {
    "OSError: not readable".into()
}

fn not_writable() -> String {
    "OSError: not writable".into()
}

fn text(bytes: Vec<u8>) -> Result<String, String> {
    let s = String::from_utf8(bytes).map_err(|_| {
        "UnicodeDecodeError: the file is not valid UTF-8 text (open it in binary mode, 'rb', to read bytes)".to_string()
    })?;
    Ok(if s.contains('\r') { s.replace("\r\n", "\n").replace('\r', "\n") } else { s })
}

pub fn new_file(vm: &mut Vm, name: &str, mode: &str, stream: Stream) -> Value {
    let rc = Rc::new(RefCell::new(stream));
    if matches!(*rc.borrow(), Stream::Writer(_) | Stream::Update(_)) {
        vm.open_files.push(Rc::downgrade(&rc));
    }
    let f = FileObj { name: name.to_string(), mode: mode.to_string(), stream: rc, binary: mode.contains('b') };
    Value::Obj(vm.heap.alloc(Obj::Handle(Box::new(Handle { ty: &FILE_TYPE, data: Box::new(f) }))))
}

/// `open(file, mode="r", encoding="utf-8")`
pub fn open_native(vm: &mut Vm, args: &[Value]) -> R {
    if args.is_empty() || args.len() > 2 {
        return Err(format!("TypeError: open() takes 1 or 2 positional arguments ({} given)", args.len()));
    }
    let path = args[0]
        .as_str()
        .ok_or_else(|| format!("TypeError: open() file must be a str path, not '{}'", args[0].type_name()))?
        .to_string();
    let mode = match vm.take_kw(sym::MODE).or(args.get(1).copied()) {
        None => "r".to_string(),
        Some(v) => v
            .as_str()
            .ok_or_else(|| format!("TypeError: open() mode must be a str, not '{}'", v.type_name()))?
            .to_string(),
    };
    if let Some(enc) = vm.take_kw(sym::ENCODING) {
        if let Some(e) = enc.as_str() {
            let e = e.to_ascii_lowercase().replace(['-', '_'], "");
            if e != "utf8" && e != "utf8sig" && e != "ascii" {
                return Err(format!("ValueError: unsupported encoding '{}' (Faston files are UTF-8)", enc.to_str()));
            }
        }
    }
    let bad_mode = || format!("ValueError: invalid mode: '{}'", mode);
    if mode.chars().any(|c| !"rwxatb+".contains(c)) || (mode.contains('b') && mode.contains('t')) {
        return Err(bad_mode());
    }
    let primary: Vec<char> = mode.chars().filter(|c| "rwxa".contains(*c)).collect();
    if primary.len() != 1 {
        return Err(bad_mode());
    }
    let plus = mode.contains('+');
    let mut oo = OpenOptions::new();
    match primary[0] {
        'r' => oo.read(true).write(plus),
        'w' => oo.write(true).create(true).truncate(true).read(plus),
        'x' => oo.write(true).create_new(true).read(plus),
        _ => oo.append(true).create(true).read(plus),
    };
    let f = oo.open(&path).map_err(|e| os_error(&e, &path))?;
    if f.metadata().map(|m| m.is_dir()).unwrap_or(false) {
        return Err(format!("IsADirectoryError: [Errno 21] Is a directory: '{}'", path));
    }
    let stream = if plus {
        Stream::Update(f)
    } else if primary[0] == 'r' {
        Stream::Reader(BufReader::new(f))
    } else {
        Stream::Writer(BufWriter::new(f))
    };
    Ok(new_file(vm, &path, &mode, stream))
}

fn file_of<'a>(g: Gc) -> &'a FileObj {
    match g.get() {
        Obj::Handle(h) => h.get::<FileObj>().expect("file handle"),
        _ => unreachable!(),
    }
}

fn file_repr(h: &Handle) -> String {
    match h.get::<FileObj>() {
        Some(f) => format!("<file '{}' mode '{}'{}>", f.name, f.mode, if matches!(*f.stream.borrow(), Stream::Closed) { " closed" } else { "" }),
        None => "<file>".into(),
    }
}

fn file_attr(vm: &mut Vm, g: Gc, name: &str) -> Option<R> {
    let f = file_of(g);
    Some(Ok(match name {
        "name" => vm.heap.string(f.name.clone()),
        "mode" => vm.heap.string(f.mode.clone()),
        "closed" => Value::Bool(matches!(*f.stream.borrow(), Stream::Closed)),
        _ => return None,
    }))
}

fn want_str_arg<'a>(v: Value, what: &str) -> Result<&'a str, String> {
    v.as_str().ok_or_else(|| format!("TypeError: {} must be str, not '{}'", what, v.type_name()))
}

fn nargs(name: &str, args: &[Value], min: usize, max: usize) -> Result<(), String> {
    if args.len() < min || args.len() > max {
        return Err(format!("TypeError: file.{}() takes {} to {} arguments ({} given)", name, min, max, args.len()));
    }
    Ok(())
}

fn read_all(s: &mut Stream) -> Result<Vec<u8>, String> {
    let mut v = Vec::new();
    let r = match s {
        Stream::Reader(r) => r.read_to_end(&mut v),
        Stream::Update(f) => f.read_to_end(&mut v),
        Stream::Stdin => io::stdin().lock().read_to_end(&mut v),
        Stream::Closed => return Err(closed()),
        _ => return Err(not_readable()),
    };
    r.map_err(|e| os_error(&e, ""))?;
    Ok(v)
}

fn read_line(s: &mut Stream) -> Result<Vec<u8>, String> {
    let mut v = Vec::new();
    let r = match s {
        Stream::Reader(r) => r.read_until(b'\n', &mut v),
        Stream::Stdin => io::stdin().lock().read_until(b'\n', &mut v),
        Stream::Update(f) => {
            let mut b = [0u8; 1];
            loop {
                match f.read(&mut b) {
                    Ok(0) => break Ok(v.len()),
                    Ok(_) => {
                        v.push(b[0]);
                        if b[0] == b'\n' {
                            break Ok(v.len());
                        }
                    }
                    Err(e) => break Err(e),
                }
            }
        }
        Stream::Closed => return Err(closed()),
        _ => return Err(not_readable()),
    };
    r.map_err(|e| os_error(&e, ""))?;
    Ok(v)
}

/// Reads up to `n` characters (not bytes).
fn read_chars(s: &mut Stream, n: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let byte = |s: &mut Stream| -> Result<Option<u8>, String> {
        let mut b = [0u8; 1];
        let r = match s {
            Stream::Reader(r) => r.read(&mut b),
            Stream::Update(f) => f.read(&mut b),
            Stream::Stdin => io::stdin().lock().read(&mut b),
            Stream::Closed => return Err(closed()),
            _ => return Err(not_readable()),
        };
        match r.map_err(|e| os_error(&e, ""))? {
            0 => Ok(None),
            _ => Ok(Some(b[0])),
        }
    };
    for _ in 0..n {
        let lead = match byte(s)? {
            Some(b) => b,
            None => break,
        };
        out.push(lead);
        let extra = if lead >= 0xF0 { 3 } else if lead >= 0xE0 { 2 } else if lead >= 0xC0 { 1 } else { 0 };
        for _ in 0..extra {
            match byte(s)? {
                Some(b) => out.push(b),
                None => break,
            }
        }
    }
    Ok(out)
}

/// Writes text to a stream (stdout goes through the VM's buffer so it stays in
/// order with print()).
pub fn write_text(vm: &mut Vm, s: &mut Stream, t: &str) -> Result<(), String> {
    let r = match s {
        Stream::Writer(w) => w.write_all(t.as_bytes()),
        Stream::Update(f) => f.write_all(t.as_bytes()),
        Stream::Stdout => {
            let r = vm.out.write_all(t.as_bytes());
            if vm.tty {
                let _ = vm.out.flush();
            }
            r
        }
        Stream::Stderr => {
            let _ = vm.out.flush();
            io::stderr().write_all(t.as_bytes())
        }
        Stream::Closed => return Err(closed()),
        Stream::Reader(_) | Stream::Stdin => return Err(not_writable()),
    };
    r.map_err(|e| os_error(&e, ""))
}

fn lines_of(vm: &mut Vm, stream: &Rc<RefCell<Stream>>, binary: bool) -> R {
    let all = read_all(&mut stream.borrow_mut())?;
    if binary {
        let lines: Vec<Value> = all.split_inclusive(|&b| b == b'\n').map(|l| vm.heap.bytes(l.to_vec())).collect();
        return Ok(vm.heap.list(lines));
    }
    let t = text(all)?;
    let lines: Vec<Value> = t.split_inclusive('\n').map(|l| vm.heap.string(l)).collect();
    Ok(vm.heap.list(lines))
}

const FILE_METHODS: &[&str] = &[
    "read", "readline", "readlines", "__iter__", "write", "writelines", "flush", "close",
    "__enter__", "__exit__", "seek", "tell", "readable", "writable", "isatty",
];

fn file_method(vm: &mut Vm, g: Gc, name: &str, args: &[Value]) -> Option<R> {
    if !FILE_METHODS.contains(&name) {
        return None;
    }
    let f = file_of(g);
    let stream = f.stream.clone();
    let binary = f.binary;
    Some((|| -> R {
        match name {
            "read" => {
                nargs(name, args, 0, 1)?;
                let n = match args.first() {
                    None | Some(Value::None) => -1,
                    Some(Value::Int(n)) => *n,
                    Some(v) => return Err(format!("TypeError: read() size must be an int, not '{}'", v.type_name())),
                };
                if binary {
                    let mut v = Vec::new();
                    let mut s = stream.borrow_mut();
                    if n < 0 {
                        v = read_all(&mut s)?;
                    } else {
                        let r = match &mut *s {
                            Stream::Reader(r) => Read::by_ref(r).take(n as u64).read_to_end(&mut v),
                            Stream::Update(f) => Read::by_ref(f).take(n as u64).read_to_end(&mut v),
                            Stream::Closed => return Err(closed()),
                            _ => return Err(not_readable()),
                        };
                        r.map_err(|e| os_error(&e, ""))?;
                    }
                    return Ok(vm.heap.bytes(v));
                }
                let bytes = if n < 0 { read_all(&mut stream.borrow_mut())? } else { read_chars(&mut stream.borrow_mut(), n as usize)? };
                let t = text(bytes)?;
                Ok(vm.heap.string(t))
            }
            "readline" => {
                nargs(name, args, 0, 0)?;
                let line = read_line(&mut stream.borrow_mut())?;
                if binary {
                    return Ok(vm.heap.bytes(line));
                }
                let t = text(line)?;
                Ok(vm.heap.string(t))
            }
            "readlines" | "__iter__" => {
                nargs(name, args, 0, 0)?;
                lines_of(vm, &stream, binary)
            }
            "write" if binary => {
                nargs(name, args, 1, 1)?;
                let b = args[0].as_bytes().ok_or_else(|| {
                    format!("TypeError: a bytes-like object is required, not '{}'", args[0].type_name())
                })?;
                let r = match &mut *stream.borrow_mut() {
                    Stream::Writer(w) => w.write_all(b),
                    Stream::Update(f) => f.write_all(b),
                    Stream::Closed => return Err(closed()),
                    _ => return Err(not_writable()),
                };
                r.map_err(|e| os_error(&e, ""))?;
                Ok(Value::Int(b.len() as i64))
            }
            "write" => {
                nargs(name, args, 1, 1)?;
                if args[0].as_bytes().is_some() {
                    return Err("TypeError: write() argument must be str, not bytes (open the file in binary mode, 'wb')".into());
                }
                let t = want_str_arg(args[0], "write() argument")?;
                write_text(vm, &mut stream.borrow_mut(), t)?;
                Ok(Value::Int(t.chars().count() as i64))
            }
            "writelines" => {
                nargs(name, args, 1, 1)?;
                let items = vm.to_vec(args[0])?;
                for it in items {
                    let t = want_str_arg(it, "writelines() item")?;
                    write_text(vm, &mut stream.borrow_mut(), t)?;
                }
                Ok(Value::None)
            }
            "flush" => {
                nargs(name, args, 0, 0)?;
                let mut s = stream.borrow_mut();
                match &mut *s {
                    Stream::Closed => return Err(closed()),
                    Stream::Stdout => {
                        let _ = vm.out.flush();
                    }
                    other => flush_stream(other),
                }
                Ok(Value::None)
            }
            "close" => {
                nargs(name, args, 0, 0)?;
                let mut s = stream.borrow_mut();
                match &mut *s {
                    Stream::Stdin | Stream::Stdout | Stream::Stderr => {}
                    other => {
                        flush_stream(other);
                        *s = Stream::Closed;
                    }
                }
                Ok(Value::None)
            }
            "__enter__" => Ok(Value::Obj(g)),
            "__exit__" => {
                let mut s = stream.borrow_mut();
                if !matches!(*s, Stream::Stdin | Stream::Stdout | Stream::Stderr) {
                    flush_stream(&mut s);
                    *s = Stream::Closed;
                }
                Ok(Value::Bool(false))
            }
            "seek" => {
                nargs(name, args, 1, 2)?;
                let off = match args[0] {
                    Value::Int(n) => n,
                    v => return Err(format!("TypeError: seek() offset must be an int, not '{}'", v.type_name())),
                };
                let whence = match args.get(1) {
                    None => 0,
                    Some(Value::Int(w)) => *w,
                    Some(v) => return Err(format!("TypeError: seek() whence must be an int, not '{}'", v.type_name())),
                };
                let pos = match whence {
                    0 if off >= 0 => SeekFrom::Start(off as u64),
                    0 => return Err(format!("ValueError: negative seek position {}", off)),
                    1 => SeekFrom::Current(off),
                    2 => SeekFrom::End(off),
                    _ => return Err(format!("ValueError: invalid whence ({}, should be 0, 1 or 2)", whence)),
                };
                let r = match &mut *stream.borrow_mut() {
                    Stream::Reader(r) => r.seek(pos),
                    Stream::Writer(w) => w.seek(pos),
                    Stream::Update(f) => f.seek(pos),
                    Stream::Closed => return Err(closed()),
                    _ => return Err("OSError: underlying stream is not seekable".into()),
                };
                Ok(Value::Int(r.map_err(|e| os_error(&e, ""))? as i64))
            }
            "tell" => {
                nargs(name, args, 0, 0)?;
                let r = match &mut *stream.borrow_mut() {
                    Stream::Reader(r) => r.stream_position(),
                    Stream::Writer(w) => w.stream_position(),
                    Stream::Update(f) => f.stream_position(),
                    Stream::Closed => return Err(closed()),
                    _ => return Err("OSError: underlying stream is not seekable".into()),
                };
                Ok(Value::Int(r.map_err(|e| os_error(&e, ""))? as i64))
            }
            "readable" => Ok(Value::Bool(matches!(*stream.borrow(), Stream::Reader(_) | Stream::Update(_) | Stream::Stdin))),
            "writable" => Ok(Value::Bool(matches!(
                *stream.borrow(),
                Stream::Writer(_) | Stream::Update(_) | Stream::Stdout | Stream::Stderr
            ))),
            "isatty" => Ok(Value::Bool(match *stream.borrow() {
                Stream::Stdout => vm.tty,
                Stream::Stdin => std::io::IsTerminal::is_terminal(&io::stdin()),
                Stream::Stderr => std::io::IsTerminal::is_terminal(&io::stderr()),
                _ => false,
            })),
            _ => unreachable!("listed in FILE_METHODS"),
        }
    })())
}
