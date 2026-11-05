//! Native helpers behind standard-library modules written in Faston
//! (src/stdlib/*.fst): `_subprocess`, `_datetime`, `_faston`.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use chrono::{Datelike, Local, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc};

use crate::value::*;
use crate::vm::Vm;

type R = Result<Value, String>;

pub fn register(vm: &mut Vm) {
    let m = vec![("run", vm.native("run", subprocess_run))];
    vm.define_module("_subprocess", m);
    let fns: &[(&'static str, NativeFn)] = &[
        ("now", dt_now),
        ("utcnow", dt_utcnow),
        ("from_timestamp", dt_from_timestamp),
        ("to_timestamp", dt_to_timestamp),
        ("strftime", dt_strftime),
        ("strptime", dt_strptime),
    ];
    let m = fns.iter().map(|&(n, f)| (n, vm.native(n, f))).collect();
    vm.define_module("_datetime", m);
    let m = vec![("set_class_name", vm.native("set_class_name", set_class_name))];
    vm.define_module("_faston", m);
}

fn want_str<'a>(v: Value, what: &str) -> Result<&'a str, String> {
    v.as_str().ok_or_else(|| format!("TypeError: {} must be a str, not '{}'", what, v.type_name()))
}

fn int(v: Value, what: &str) -> Result<i64, String> {
    match v {
        Value::Int(i) => Ok(i),
        Value::Bool(b) => Ok(b as i64),
        _ => Err(format!("TypeError: {} must be an int, not '{}'", what, v.type_name())),
    }
}

// ---------------------------------------------------------------- _faston

/// set_class_name(cls, name): used by collections.namedtuple.
fn set_class_name(_: &mut Vm, args: &[Value]) -> R {
    match (args.first(), args.get(1).and_then(|v| v.as_str())) {
        (Some(Value::Obj(g)), Some(n)) if matches!(g.get(), Obj::Class(_)) => {
            g.as_class_mut().name = n.to_string();
            Ok(Value::None)
        }
        _ => Err("TypeError: set_class_name(cls, name)".into()),
    }
}

// ---------------------------------------------------------------- _subprocess

/// Splits a command line like a shell would (quotes and backslash escapes).
fn shell_split(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') if matches!(chars.peek(), Some('"') | Some('\\')) => cur.push(chars.next().unwrap()),
            (Some(_), c) => cur.push(c),
            (None, '"') | (None, '\'') => {
                quote = Some(c);
                in_word = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    out.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            (None, c) => {
                cur.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        out.push(cur);
    }
    out
}

fn text(bytes: Vec<u8>) -> String {
    let s = String::from_utf8_lossy(&bytes).into_owned();
    if s.contains('\r') {
        s.replace("\r\n", "\n")
    } else {
        s
    }
}

/// run(args, shell, cwd, input, capture_out, capture_err, merge_err, timeout, env, null_out, null_err)
/// -> (returncode, stdout or None, stderr or None, timed_out)
fn subprocess_run(vm: &mut Vm, args: &[Value]) -> R {
    if args.len() != 11 {
        return Err("TypeError: _subprocess.run takes 11 arguments".into());
    }
    let shell = vm.truthy(args[1])?;
    let mut cmd = if shell {
        let line = match args[0].as_str() {
            Some(s) => s.to_string(),
            None => {
                let parts = vm.to_vec(args[0])?;
                let mut v = Vec::new();
                for p in parts {
                    v.push(vm.str_of(p)?);
                }
                v.join(" ")
            }
        };
        if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.args(["/C", &line]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", &line]);
            c
        }
    } else {
        let parts: Vec<String> = match args[0].as_str() {
            Some(s) => shell_split(s),
            None => {
                let items = vm.to_vec(args[0])?;
                let mut v = Vec::new();
                for p in items {
                    v.push(vm.str_of(p)?);
                }
                v
            }
        };
        if parts.is_empty() {
            return Err("ValueError: empty command".into());
        }
        let mut c = Command::new(&parts[0]);
        c.args(&parts[1..]);
        c
    };
    if let Some(cwd) = args[2].as_str() {
        cmd.current_dir(cwd);
    }
    let input = args[3].as_str().map(|s| s.as_bytes().to_vec());
    let (cap_out, cap_err, merge) = (vm.truthy(args[4])?, vm.truthy(args[5])?, vm.truthy(args[6])?);
    let timeout = match args[7] {
        Value::None => None,
        Value::Int(i) => Some(Duration::from_secs_f64(i as f64)),
        Value::Float(f) => Some(Duration::from_secs_f64(f.max(0.0))),
        v => return Err(format!("TypeError: timeout must be a number, not '{}'", v.type_name())),
    };
    if let Value::Obj(g) = args[8] {
        if let Obj::Dict(d) = g.get() {
            cmd.env_clear();
            for (k, v) in d.entries.iter() {
                cmd.env(vm.str_of(*k)?, vm.str_of(*v)?);
            }
        }
    }
    let (null_out, null_err) = (vm.truthy(args[9])?, vm.truthy(args[10])?);
    cmd.stdin(if input.is_some() { Stdio::piped() } else { Stdio::inherit() });
    cmd.stdout(if cap_out || merge { Stdio::piped() } else if null_out { Stdio::null() } else { Stdio::inherit() });
    cmd.stderr(if cap_err || merge { Stdio::piped() } else if null_err { Stdio::null() } else { Stdio::inherit() });
    vm.flush_all();
    let program = format!("{:?}", cmd.get_program());
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!("FileNotFoundError: [Errno 2] No such file or directory: {}", program)
        } else {
            crate::fileio::os_error(&e, "")
        }
    })?;
    if let Some(data) = input {
        if let Some(mut stdin) = child.stdin.take() {
            std::thread::spawn(move || {
                let _ = stdin.write_all(&data);
            });
        }
    }
    let read_pipe = |p: Option<Box<dyn Read + Send>>| {
        p.map(|mut r| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = r.read_to_end(&mut buf);
                buf
            })
        })
    };
    let out_t = read_pipe(child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>));
    let err_t = read_pipe(child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>));
    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait().map_err(|e| crate::fileio::os_error(&e, ""))? {
            Some(st) => break Some(st),
            None => {
                if timeout.is_some_and(|t| start.elapsed() >= t) {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break None;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    };
    let out = out_t.map(|t| text(t.join().unwrap_or_default()));
    let err = err_t.map(|t| text(t.join().unwrap_or_default()));
    let (out, err) = if merge {
        (Some(format!("{}{}", out.unwrap_or_default(), err.unwrap_or_default())), None)
    } else {
        (out, err)
    };
    let code = status.and_then(|s| s.code()).unwrap_or(-1) as i64;
    let ov = match out {
        Some(s) if cap_out || merge => vm.heap.string(s),
        _ => Value::None,
    };
    let ev = match err {
        Some(s) if cap_err => vm.heap.string(s),
        _ => Value::None,
    };
    Ok(vm.heap.tuple(vec![Value::Int(code), ov, ev, Value::Bool(timed_out)]))
}

// ---------------------------------------------------------------- _datetime

fn parts(vm: &mut Vm, dt: NaiveDateTime) -> Value {
    let v = vec![
        Value::Int(dt.year() as i64),
        Value::Int(dt.month() as i64),
        Value::Int(dt.day() as i64),
        Value::Int(dt.hour() as i64),
        Value::Int(dt.minute() as i64),
        Value::Int(dt.second() as i64),
        Value::Int((dt.nanosecond() / 1000).min(999_999) as i64),
    ];
    vm.heap.tuple(v)
}

fn dt_now(vm: &mut Vm, _: &[Value]) -> R {
    Ok(parts(vm, Local::now().naive_local()))
}

fn dt_utcnow(vm: &mut Vm, _: &[Value]) -> R {
    Ok(parts(vm, Utc::now().naive_utc()))
}

/// from_timestamp(ts, utc) -> parts
fn dt_from_timestamp(vm: &mut Vm, args: &[Value]) -> R {
    let ts = match args.first() {
        Some(Value::Int(i)) => *i as f64,
        Some(Value::Float(f)) => *f,
        _ => return Err("TypeError: timestamp must be a number".into()),
    };
    let utc = args.get(1).is_some_and(|v| v.truthy());
    let secs = ts.floor() as i64;
    let micros = ((ts - ts.floor()) * 1e6).round() as u32;
    let u = Utc.timestamp_opt(secs, micros.min(999_999) * 1000).single().ok_or("OverflowError: timestamp out of range")?;
    Ok(parts(vm, if utc { u.naive_utc() } else { u.with_timezone(&Local).naive_local() }))
}

fn naive(args: &[Value]) -> Result<NaiveDateTime, String> {
    let g = |i: usize| -> Result<i64, String> { int(args.get(i).copied().unwrap_or(Value::Int(0)), "date part") };
    let d = NaiveDate::from_ymd_opt(g(0)? as i32, g(1)? as u32, g(2)? as u32).ok_or("ValueError: invalid date")?;
    d.and_hms_micro_opt(g(3)? as u32, g(4)? as u32, g(5)? as u32, g(6)? as u32).ok_or_else(|| "ValueError: invalid time".into())
}

/// to_timestamp(y, mo, d, h, mi, s, us): local time -> seconds since the epoch
fn dt_to_timestamp(_: &mut Vm, args: &[Value]) -> R {
    let n = naive(args)?;
    let local = Local.from_local_datetime(&n).earliest().ok_or("ValueError: that local time doesn't exist")?;
    Ok(Value::Float(local.timestamp() as f64 + local.timestamp_subsec_micros() as f64 / 1e6))
}

const DAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
const MONTHS: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December",
];

/// strftime(fmt, y, mo, d, h, mi, s, us) with Python's directives.
fn dt_strftime(vm: &mut Vm, args: &[Value]) -> R {
    let fmt = want_str(args.first().copied().unwrap_or(Value::None), "format")?;
    let n = naive(&args[1..])?;
    let wd = n.weekday().num_days_from_monday() as usize;
    let mut out = String::new();
    let mut it = fmt.chars();
    while let Some(c) = it.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let d = match it.next() {
            Some(d) => d,
            None => {
                out.push('%');
                break;
            }
        };
        let h12 = if n.hour() % 12 == 0 { 12 } else { n.hour() % 12 };
        let s = match d {
            'Y' => format!("{:04}", n.year()),
            'y' => format!("{:02}", n.year() % 100),
            'm' => format!("{:02}", n.month()),
            'd' => format!("{:02}", n.day()),
            'H' => format!("{:02}", n.hour()),
            'I' => format!("{:02}", h12),
            'M' => format!("{:02}", n.minute()),
            'S' => format!("{:02}", n.second()),
            'f' => format!("{:06}", n.nanosecond() / 1000),
            'p' => (if n.hour() < 12 { "AM" } else { "PM" }).to_string(),
            'a' => DAYS[wd][..3].to_string(),
            'A' => DAYS[wd].to_string(),
            'w' => ((wd + 1) % 7).to_string(),
            'u' => (wd + 1).to_string(),
            'b' | 'h' => MONTHS[n.month0() as usize][..3].to_string(),
            'B' => MONTHS[n.month0() as usize].to_string(),
            'j' => format!("{:03}", n.ordinal()),
            'U' => format!("{:02}", (n.ordinal() as i64 + 6 - ((wd as i64 + 1) % 7)) / 7),
            'W' => format!("{:02}", (n.ordinal() as i64 + 6 - wd as i64) / 7),
            'c' => format!("{} {} {:2} {:02}:{:02}:{:02} {:04}", &DAYS[wd][..3], &MONTHS[n.month0() as usize][..3], n.day(), n.hour(), n.minute(), n.second(), n.year()),
            'x' => format!("{:02}/{:02}/{:02}", n.month(), n.day(), n.year() % 100),
            'X' => format!("{:02}:{:02}:{:02}", n.hour(), n.minute(), n.second()),
            'z' | 'Z' => String::new(),
            '%' => "%".to_string(),
            other => format!("%{}", other),
        };
        out.push_str(&s);
    }
    Ok(vm.heap.string(out))
}

/// strptime(text, fmt) -> parts (raises ValueError like Python on mismatch)
fn dt_strptime(vm: &mut Vm, args: &[Value]) -> R {
    let s = want_str(args.first().copied().unwrap_or(Value::None), "string")?;
    let fmt = want_str(args.get(1).copied().unwrap_or(Value::None), "format")?;
    let bad = || format!("ValueError: time data {:?} does not match format {:?}", s, fmt);
    let sc: Vec<char> = s.chars().collect();
    let mut i = 0;
    let (mut y, mut mo, mut d, mut h, mut mi, mut se, mut us) = (1900i64, 1i64, 1i64, 0i64, 0i64, 0i64, 0i64);
    let mut pm: Option<bool> = None;
    let num = |i: &mut usize, max: usize| -> Option<i64> {
        let start = *i;
        while *i < sc.len() && *i - start < max && sc[*i].is_ascii_digit() {
            *i += 1;
        }
        if *i == start {
            return None;
        }
        sc[start..*i].iter().collect::<String>().parse().ok()
    };
    let name = |i: &mut usize, names: &[&str], short: bool| -> Option<i64> {
        let rest: String = sc[*i..].iter().collect::<String>().to_lowercase();
        for (k, n) in names.iter().enumerate() {
            let cand = if short { n[..3].to_lowercase() } else { n.to_lowercase() };
            if rest.starts_with(&cand) {
                *i += cand.chars().count();
                return Some(k as i64);
            }
        }
        None
    };
    let mut f = fmt.chars().peekable();
    while let Some(c) = f.next() {
        if c == '%' {
            let dch = f.next().ok_or_else(bad)?;
            match dch {
                'Y' => y = num(&mut i, 4).ok_or_else(bad)?,
                'y' => {
                    let v = num(&mut i, 2).ok_or_else(bad)?;
                    y = if v < 69 { 2000 + v } else { 1900 + v };
                }
                'm' => mo = num(&mut i, 2).ok_or_else(bad)?,
                'd' => d = num(&mut i, 2).ok_or_else(bad)?,
                'H' | 'I' => h = num(&mut i, 2).ok_or_else(bad)?,
                'M' => mi = num(&mut i, 2).ok_or_else(bad)?,
                'S' => se = num(&mut i, 2).ok_or_else(bad)?,
                'f' => {
                    let start = i;
                    let v = num(&mut i, 6).ok_or_else(bad)?;
                    us = v * 10i64.pow(6 - (i - start) as u32);
                }
                'p' => {
                    let rest: String = sc[i..].iter().take(2).collect::<String>().to_uppercase();
                    pm = match rest.as_str() {
                        "AM" => Some(false),
                        "PM" => Some(true),
                        _ => return Err(bad()),
                    };
                    i += 2;
                }
                'b' | 'h' => mo = name(&mut i, &MONTHS, true).ok_or_else(bad)? + 1,
                'B' => mo = name(&mut i, &MONTHS, false).ok_or_else(bad)? + 1,
                'a' => {
                    name(&mut i, &DAYS, true).ok_or_else(bad)?;
                }
                'A' => {
                    name(&mut i, &DAYS, false).ok_or_else(bad)?;
                }
                'j' => {
                    let doy = num(&mut i, 3).ok_or_else(bad)?;
                    let date = NaiveDate::from_yo_opt(y as i32, doy as u32).ok_or_else(bad)?;
                    mo = date.month() as i64;
                    d = date.day() as i64;
                }
                '%' => {
                    if sc.get(i) != Some(&'%') {
                        return Err(bad());
                    }
                    i += 1;
                }
                _ => return Err(format!("ValueError: unsupported strptime directive '%{}'", dch)),
            }
        } else if c.is_whitespace() {
            while i < sc.len() && sc[i].is_whitespace() {
                i += 1;
            }
        } else {
            if sc.get(i) != Some(&c) {
                return Err(bad());
            }
            i += 1;
        }
    }
    if i != sc.len() {
        return Err(format!("ValueError: unconverted data remains: {}", sc[i..].iter().collect::<String>()));
    }
    if let Some(p) = pm {
        if h == 12 {
            h = 0;
        }
        if p {
            h += 12;
        }
    }
    let n = naive(&[Value::Int(y), Value::Int(mo), Value::Int(d), Value::Int(h), Value::Int(mi), Value::Int(se), Value::Int(us)])
        .map_err(|_| bad())?;
    Ok(parts(vm, n))
}
