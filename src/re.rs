//! The `re` module: Python-style regular expressions (backed by fancy-regex,
//! which supports look-around and backreferences like Python's engine).
//! Positions are character indices, like Python's.

use std::rc::Rc;

use fancy_regex::Regex;

use crate::value::*;
use crate::vm::Vm;

type R = Result<Value, String>;

pub const IGNORECASE: i64 = 2;
pub const MULTILINE: i64 = 8;
pub const DOTALL: i64 = 16;
pub const VERBOSE: i64 = 64;
pub const ASCII: i64 = 256;

struct PatternObj {
    regex: Regex,
    /// `\A(?:...)\z`, made on first `fullmatch`.
    full: Option<Regex>,
    pattern: String,
    flags: i64,
    names: Rc<Vec<Option<String>>>,
}

struct MatchObj {
    string: Rc<str>,
    /// Byte spans of the groups (0 = whole match).
    spans: Vec<Option<(usize, usize)>>,
    names: Rc<Vec<Option<String>>>,
}

static PATTERN_TYPE: HandleType = HandleType { name: "re.Pattern", method: pattern_method, attr: pattern_attr, repr: pattern_repr };
static MATCH_TYPE: HandleType = HandleType { name: "re.Match", method: match_method, attr: match_attr, repr: match_repr };

pub fn register(vm: &mut Vm) {
    let flags = [sym_kw(vm, "flags")];
    let flags: &'static [u32] = Box::leak(flags.to_vec().into_boxed_slice());
    let count_flags: &'static [u32] = Box::leak(vec![sym_kw(vm, "count"), sym_kw(vm, "flags")].into_boxed_slice());
    let split_flags: &'static [u32] = Box::leak(vec![sym::MAXSPLIT, sym_kw(vm, "flags")].into_boxed_slice());
    let mk = |vm: &mut Vm, name: &'static str, f: NativeFn, kw: &'static [u32]| {
        Value::Obj(vm.heap.alloc(Obj::Native(Native { name, func: f, kwargs: kw })))
    };
    let mut m = vec![
        ("compile", mk(vm, "compile", re_compile, flags)),
        ("search", mk(vm, "search", re_search, flags)),
        ("match", mk(vm, "match", re_match, flags)),
        ("fullmatch", mk(vm, "fullmatch", re_fullmatch, flags)),
        ("findall", mk(vm, "findall", re_findall, flags)),
        ("finditer", mk(vm, "finditer", re_finditer, flags)),
        ("sub", mk(vm, "sub", re_sub, count_flags)),
        ("subn", mk(vm, "subn", re_subn, count_flags)),
        ("split", mk(vm, "split", re_split, split_flags)),
        ("escape", mk(vm, "escape", re_escape, &[])),
    ];
    for (n, v) in [
        ("I", IGNORECASE),
        ("IGNORECASE", IGNORECASE),
        ("M", MULTILINE),
        ("MULTILINE", MULTILINE),
        ("S", DOTALL),
        ("DOTALL", DOTALL),
        ("X", VERBOSE),
        ("VERBOSE", VERBOSE),
        ("A", ASCII),
        ("ASCII", ASCII),
    ] {
        m.push((n, Value::Int(v)));
    }
    vm.define_module("re", m);
}

fn sym_kw(vm: &mut Vm, name: &str) -> u32 {
    vm.syms.intern(name)
}

fn kw_int(vm: &mut Vm, name: &str, default: i64) -> Result<i64, String> {
    let s = vm.syms.intern(name);
    match vm.take_kw(s) {
        None => Ok(default),
        Some(Value::Int(i)) => Ok(i),
        Some(Value::Bool(b)) => Ok(b as i64),
        Some(v) => Err(format!("TypeError: {} must be an int, not '{}'", name, v.type_name())),
    }
}

fn want_str<'a>(v: Value, what: &str) -> Result<&'a str, String> {
    v.as_str().ok_or_else(|| format!("TypeError: {} must be a str, not '{}'", what, v.type_name()))
}

fn nargs(name: &str, args: &[Value], min: usize, max: usize) -> Result<(), String> {
    if args.len() < min || args.len() > max {
        return Err(format!("TypeError: {}() takes {} to {} arguments ({} given)", name, min, max, args.len()));
    }
    Ok(())
}

fn char_idx(s: &str, byte: usize) -> i64 {
    s[..byte].chars().count() as i64
}

fn byte_of(s: &str, ch: i64) -> usize {
    if ch <= 0 {
        return 0;
    }
    s.char_indices().nth(ch as usize).map_or(s.len(), |(b, _)| b)
}

/// Python syntax -> fancy-regex syntax (`(?P=name)`, `\Z`) plus inline flags.
fn translate(pat: &str, flags: i64) -> String {
    let mut out = String::with_capacity(pat.len() + 8);
    let mut fl = String::new();
    if flags & IGNORECASE != 0 {
        fl.push('i');
    }
    if flags & MULTILINE != 0 {
        fl.push('m');
    }
    if flags & DOTALL != 0 {
        fl.push('s');
    }
    if flags & VERBOSE != 0 {
        fl.push('x');
    }
    if !fl.is_empty() {
        out.push_str(&format!("(?{})", fl));
    }
    let cs: Vec<char> = pat.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c == '\\' && i + 1 < cs.len() {
            if cs[i + 1] == 'Z' {
                out.push_str("\\z");
            } else {
                out.push(c);
                out.push(cs[i + 1]);
            }
            i += 2;
            continue;
        }
        if c == '(' && cs[i..].starts_with(&['(', '?', 'P', '=']) {
            let rest: String = cs[i + 4..].iter().collect();
            if let Some(end) = rest.find(')') {
                out.push_str(&format!("\\k<{}>", &rest[..end]));
                i += 4 + rest[..end].chars().count() + 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

fn compile_pattern(pat: &str, flags: i64) -> Result<PatternObj, String> {
    let regex = Regex::new(&translate(pat, flags)).map_err(|e| format!("ValueError: bad regular expression {:?}: {}", pat, e))?;
    let names: Vec<Option<String>> = regex.capture_names().map(|n| n.map(String::from)).collect();
    Ok(PatternObj { regex, full: None, pattern: pat.to_string(), flags, names: Rc::new(names) })
}

fn pattern_value(vm: &mut Vm, p: PatternObj) -> Value {
    Value::Obj(vm.heap.alloc(Obj::Handle(Box::new(Handle { ty: &PATTERN_TYPE, data: Box::new(p) }))))
}

/// The pattern behind a `re` function's first argument (a string or a compiled Pattern).
fn pattern_arg(vm: &mut Vm, v: Value, flags: i64) -> Result<Value, String> {
    if let Value::Obj(g) = v {
        if let Obj::Handle(h) = g.get() {
            if h.get::<PatternObj>().is_some() {
                if flags != 0 {
                    return Err("ValueError: cannot process flags argument with a compiled pattern".into());
                }
                return Ok(v);
            }
        }
    }
    let s = want_str(v, "pattern")?;
    let p = compile_pattern(s, flags)?;
    Ok(pattern_value(vm, p))
}

fn pat_of<'a>(v: Value) -> &'a mut PatternObj {
    match v {
        Value::Obj(g) => match g.get_mut() {
            Obj::Handle(h) => h.get_mut::<PatternObj>().expect("pattern"),
            _ => unreachable!(),
        },
        _ => unreachable!(),
    }
}

fn caps_to_match(vm: &mut Vm, string: &Rc<str>, caps: &fancy_regex::Captures, names: &Rc<Vec<Option<String>>>) -> Value {
    let spans = (0..caps.len()).map(|i| caps.get(i).map(|m| (m.start(), m.end()))).collect();
    let m = MatchObj { string: string.clone(), spans, names: names.clone() };
    Value::Obj(vm.heap.alloc(Obj::Handle(Box::new(Handle { ty: &MATCH_TYPE, data: Box::new(m) }))))
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Search,
    Match,
    Full,
}

fn do_search(vm: &mut Vm, pv: Value, s: &str, pos: i64, mode: Mode) -> R {
    let p = pat_of(pv);
    let start = byte_of(s, pos);
    if mode == Mode::Full && p.full.is_none() {
        let full = Regex::new(&format!("\\A(?:{})\\z", translate(&p.pattern, p.flags)))
            .map_err(|e| format!("ValueError: bad regular expression: {}", e))?;
        p.full = Some(full);
    }
    let re = if mode == Mode::Full { p.full.as_ref().unwrap() } else { &p.regex };
    let hay: Rc<str> = Rc::from(s);
    let caps = re.captures_from_pos(&hay, start).map_err(|e| format!("RuntimeError: regex error: {}", e))?;
    match caps {
        Some(c) if mode != Mode::Match || c.get(0).is_some_and(|m| m.start() == start) => {
            let names = p.names.clone();
            Ok(caps_to_match(vm, &hay, &c, &names))
        }
        _ => Ok(Value::None),
    }
}

/// All non-overlapping matches as (byte spans per group).
fn all_spans(p: &PatternObj, s: &str) -> Result<Vec<Vec<Option<(usize, usize)>>>, String> {
    let mut out = Vec::new();
    for c in p.regex.captures_iter(s) {
        let c = c.map_err(|e| format!("RuntimeError: regex error: {}", e))?;
        out.push((0..c.len()).map(|i| c.get(i).map(|m| (m.start(), m.end()))).collect());
    }
    Ok(out)
}

fn findall(vm: &mut Vm, pv: Value, s: &str) -> R {
    let p = pat_of(pv);
    let all = all_spans(p, s)?;
    let groups = p.names.len().saturating_sub(1);
    let mut items = Vec::with_capacity(all.len());
    for spans in all {
        let text = |vm: &mut Vm, sp: Option<(usize, usize)>| match sp {
            Some((a, b)) => vm.heap.string(&s[a..b]),
            None => vm.heap.string(""),
        };
        let v = match groups {
            0 => text(vm, spans[0]),
            1 => text(vm, spans[1]),
            _ => {
                let parts: Vec<Value> = spans[1..].iter().map(|sp| text(vm, *sp)).collect();
                vm.heap.tuple(parts)
            }
        };
        items.push(v);
    }
    Ok(vm.heap.list(items))
}

fn finditer(vm: &mut Vm, pv: Value, s: &str) -> R {
    let p = pat_of(pv);
    let names = p.names.clone();
    let hay: Rc<str> = Rc::from(s);
    let all = all_spans(p, &hay)?;
    let items: Vec<Value> = all
        .into_iter()
        .map(|spans| {
            let m = MatchObj { string: hay.clone(), spans, names: names.clone() };
            Value::Obj(vm.heap.alloc(Obj::Handle(Box::new(Handle { ty: &MATCH_TYPE, data: Box::new(m) }))))
        })
        .collect();
    Ok(vm.heap.list(items))
}

/// Expands a Python replacement template (`\1`, `\g<name>`, `\n`, ...).
fn expand(template: &str, s: &str, spans: &[Option<(usize, usize)>], names: &[Option<String>]) -> Result<String, String> {
    let group = |i: usize| -> Result<&str, String> {
        match spans.get(i) {
            Some(Some((a, b))) => Ok(&s[*a..*b]),
            Some(None) => Ok(""),
            None => Err(format!("re.error: invalid group reference {}", i)),
        }
    };
    let cs: Vec<char> = template.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c != '\\' || i + 1 >= cs.len() {
            out.push(c);
            i += 1;
            continue;
        }
        let n = cs[i + 1];
        match n {
            '0'..='9' => {
                let mut j = i + 1;
                let mut num = 0usize;
                while j < cs.len() && cs[j].is_ascii_digit() && j < i + 3 {
                    num = num * 10 + cs[j].to_digit(10).unwrap() as usize;
                    j += 1;
                }
                out.push_str(group(num)?);
                i = j;
            }
            'g' if cs.get(i + 2) == Some(&'<') => {
                let close = cs[i + 3..].iter().position(|&x| x == '>').ok_or("re.error: missing '>' in group reference")?;
                let name: String = cs[i + 3..i + 3 + close].iter().collect();
                let idx = match name.parse::<usize>() {
                    Ok(k) => k,
                    Err(_) => names
                        .iter()
                        .position(|x| x.as_deref() == Some(name.as_str()))
                        .ok_or_else(|| format!("IndexError: unknown group name '{}'", name))?,
                };
                out.push_str(group(idx)?);
                i += 4 + close;
            }
            'n' => {
                out.push('\n');
                i += 2;
            }
            't' => {
                out.push('\t');
                i += 2;
            }
            'r' => {
                out.push('\r');
                i += 2;
            }
            '\\' => {
                out.push('\\');
                i += 2;
            }
            other => {
                out.push('\\');
                out.push(other);
                i += 2;
            }
        }
    }
    Ok(out)
}

fn subn(vm: &mut Vm, pv: Value, repl: Value, s: &str, count: i64) -> Result<(String, i64), String> {
    let (all, names) = {
        let p = pat_of(pv);
        (all_spans(p, s)?, p.names.clone())
    };
    let hay: Rc<str> = Rc::from(s);
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    let mut n = 0i64;
    for spans in all {
        if count > 0 && n >= count {
            break;
        }
        let (a, b) = spans[0].unwrap();
        out.push_str(&s[last..a]);
        if let Some(t) = repl.as_str() {
            out.push_str(&expand(t, s, &spans, &names)?);
        } else {
            let m = MatchObj { string: hay.clone(), spans: spans.clone(), names: names.clone() };
            let mv = Value::Obj(vm.heap.alloc(Obj::Handle(Box::new(Handle { ty: &MATCH_TYPE, data: Box::new(m) }))));
            let r = vm.call(repl, &[mv])?;
            out.push_str(r.as_str().ok_or_else(|| format!("TypeError: the replacement function must return a str, not '{}'", r.type_name()))?);
        }
        last = b;
        n += 1;
    }
    out.push_str(&s[last..]);
    Ok((out, n))
}

fn split(vm: &mut Vm, pv: Value, s: &str, maxsplit: i64) -> R {
    let all = all_spans(pat_of(pv), s)?;
    let mut items = Vec::new();
    let mut last = 0;
    for (n, spans) in all.into_iter().enumerate() {
        if maxsplit > 0 && n as i64 >= maxsplit {
            break;
        }
        let (a, b) = spans[0].unwrap();
        items.push(vm.heap.string(&s[last..a]));
        for g in &spans[1..] {
            items.push(match g {
                Some((x, y)) => vm.heap.string(&s[*x..*y]),
                None => Value::None,
            });
        }
        last = b;
    }
    items.push(vm.heap.string(&s[last..]));
    Ok(vm.heap.list(items))
}

// ---------------------------------------------------------------- module functions

fn re_compile(vm: &mut Vm, args: &[Value]) -> R {
    nargs("compile", args, 1, 2)?;
    let flags = match args.get(1) {
        Some(Value::Int(f)) => *f,
        _ => kw_int(vm, "flags", 0)?,
    };
    pattern_arg(vm, args[0], flags)
}

fn re_find(vm: &mut Vm, args: &[Value], name: &str, mode: Mode) -> R {
    nargs(name, args, 2, 3)?;
    let flags = match args.get(2) {
        Some(Value::Int(f)) => *f,
        _ => kw_int(vm, "flags", 0)?,
    };
    let s = want_str(args[1], "string")?;
    let p = pattern_arg(vm, args[0], flags)?;
    do_search(vm, p, s, 0, mode)
}

fn re_search(vm: &mut Vm, args: &[Value]) -> R {
    re_find(vm, args, "search", Mode::Search)
}
fn re_match(vm: &mut Vm, args: &[Value]) -> R {
    re_find(vm, args, "match", Mode::Match)
}
fn re_fullmatch(vm: &mut Vm, args: &[Value]) -> R {
    re_find(vm, args, "fullmatch", Mode::Full)
}

fn re_findall(vm: &mut Vm, args: &[Value]) -> R {
    nargs("findall", args, 2, 3)?;
    let flags = match args.get(2) {
        Some(Value::Int(f)) => *f,
        _ => kw_int(vm, "flags", 0)?,
    };
    let s = want_str(args[1], "string")?;
    let p = pattern_arg(vm, args[0], flags)?;
    findall(vm, p, s)
}

fn re_finditer(vm: &mut Vm, args: &[Value]) -> R {
    nargs("finditer", args, 2, 3)?;
    let flags = match args.get(2) {
        Some(Value::Int(f)) => *f,
        _ => kw_int(vm, "flags", 0)?,
    };
    let s = want_str(args[1], "string")?;
    let p = pattern_arg(vm, args[0], flags)?;
    finditer(vm, p, s)
}

fn sub_common(vm: &mut Vm, args: &[Value], name: &str) -> Result<(String, i64), String> {
    nargs(name, args, 3, 5)?;
    let count = match args.get(3) {
        Some(Value::Int(c)) => *c,
        _ => kw_int(vm, "count", 0)?,
    };
    let flags = match args.get(4) {
        Some(Value::Int(f)) => *f,
        _ => kw_int(vm, "flags", 0)?,
    };
    let s = want_str(args[2], "string")?.to_string();
    let p = pattern_arg(vm, args[0], flags)?;
    let mark = vm.temp_mark();
    vm.push_temp(p)?;
    let r = subn(vm, p, args[1], &s, count);
    vm.temp_reset(mark);
    r
}

fn re_sub(vm: &mut Vm, args: &[Value]) -> R {
    let (s, _) = sub_common(vm, args, "sub")?;
    Ok(vm.heap.string(s))
}

fn re_subn(vm: &mut Vm, args: &[Value]) -> R {
    let (s, n) = sub_common(vm, args, "subn")?;
    let sv = vm.heap.string(s);
    Ok(vm.heap.tuple(vec![sv, Value::Int(n)]))
}

fn re_split(vm: &mut Vm, args: &[Value]) -> R {
    nargs("split", args, 2, 4)?;
    let maxsplit = match args.get(2) {
        Some(Value::Int(m)) => *m,
        _ => match vm.take_kw(sym::MAXSPLIT) {
            Some(Value::Int(m)) => m,
            _ => 0,
        },
    };
    let flags = match args.get(3) {
        Some(Value::Int(f)) => *f,
        _ => kw_int(vm, "flags", 0)?,
    };
    let s = want_str(args[1], "string")?;
    let p = pattern_arg(vm, args[0], flags)?;
    split(vm, p, s, maxsplit)
}

fn re_escape(vm: &mut Vm, args: &[Value]) -> R {
    nargs("escape", args, 1, 1)?;
    let s = want_str(args[0], "pattern")?;
    let mut out = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        if "()[]{}?*+-|^$\\.&~# \t\n\r\x0b\x0c".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    Ok(vm.heap.string(out))
}

// ---------------------------------------------------------------- Pattern objects

fn pattern_repr(h: &Handle) -> String {
    match h.get::<PatternObj>() {
        Some(p) => format!("re.compile({})", py_str_repr(&p.pattern)),
        None => "<re.Pattern>".into(),
    }
}

fn pattern_attr(vm: &mut Vm, g: Gc, name: &str) -> Option<R> {
    let p = pat_of(Value::Obj(g));
    Some(Ok(match name {
        "pattern" => vm.heap.string(p.pattern.clone()),
        "flags" => Value::Int(p.flags),
        "groups" => Value::Int(p.names.len() as i64 - 1),
        "groupindex" => {
            let mut d = Dict::default();
            for (i, n) in p.names.clone().iter().enumerate() {
                if let Some(n) = n {
                    let k = vm.heap.string(n.clone());
                    if let Ok(key) = to_key(k) {
                        d.insert(key, k, Value::Int(i as i64));
                    }
                }
            }
            vm.heap.dict(d)
        }
        _ => return None,
    }))
}

fn pattern_method(vm: &mut Vm, g: Gc, name: &str, args: &[Value]) -> Option<R> {
    let pv = Value::Obj(g);
    let r = (|| -> Option<R> {
        Some(match name {
            "search" | "match" | "fullmatch" => (|| {
                nargs(name, args, 1, 2)?;
                let s = want_str(args[0], "string")?;
                let pos = match args.get(1) {
                    Some(Value::Int(p)) => *p,
                    _ => kw_int(vm, "pos", 0)?,
                };
                let mode = match name {
                    "search" => Mode::Search,
                    "match" => Mode::Match,
                    _ => Mode::Full,
                };
                do_search(vm, pv, s, pos, mode)
            })(),
            "findall" => (|| {
                nargs(name, args, 1, 1)?;
                findall(vm, pv, want_str(args[0], "string")?)
            })(),
            "finditer" => (|| {
                nargs(name, args, 1, 1)?;
                finditer(vm, pv, want_str(args[0], "string")?)
            })(),
            "sub" | "subn" => (|| {
                nargs(name, args, 2, 3)?;
                let count = match args.get(2) {
                    Some(Value::Int(c)) => *c,
                    _ => kw_int(vm, "count", 0)?,
                };
                let s = want_str(args[1], "string")?.to_string();
                let (out, n) = subn(vm, pv, args[0], &s, count)?;
                let sv = vm.heap.string(out);
                Ok(if name == "sub" { sv } else { vm.heap.tuple(vec![sv, Value::Int(n)]) })
            })(),
            "split" => (|| {
                nargs(name, args, 1, 2)?;
                let maxsplit = match args.get(1) {
                    Some(Value::Int(m)) => *m,
                    _ => match vm.take_kw(sym::MAXSPLIT) {
                        Some(Value::Int(m)) => m,
                        _ => 0,
                    },
                };
                split(vm, pv, want_str(args[0], "string")?, maxsplit)
            })(),
            _ => return None,
        })
    })();
    r
}

// ---------------------------------------------------------------- Match objects

fn match_of<'a>(g: Gc) -> &'a MatchObj {
    match g.get() {
        Obj::Handle(h) => h.get::<MatchObj>().expect("match"),
        _ => unreachable!(),
    }
}

fn match_repr(h: &Handle) -> String {
    match h.get::<MatchObj>() {
        Some(m) => match m.spans[0] {
            Some((a, b)) => format!(
                "<re.Match object; span=({}, {}), match={}>",
                char_idx(&m.string, a),
                char_idx(&m.string, b),
                py_str_repr(&m.string[a..b])
            ),
            None => "<re.Match object>".into(),
        },
        None => "<re.Match object>".into(),
    }
}

fn py_str_repr(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(q);
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\\' => out.push_str("\\\\"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

/// Group index from an int or a group name.
fn group_index(m: &MatchObj, v: Value) -> Result<usize, String> {
    match v {
        Value::Int(i) if i >= 0 && (i as usize) < m.spans.len() => Ok(i as usize),
        Value::Int(_) => Err("IndexError: no such group".into()),
        _ => match v.as_str() {
            Some(n) => m.names.iter().position(|x| x.as_deref() == Some(n)).ok_or_else(|| "IndexError: no such group".into()),
            None => Err("IndexError: no such group".into()),
        },
    }
}

fn group_value(vm: &mut Vm, m: &MatchObj, i: usize, default: Value) -> Value {
    match m.spans[i] {
        Some((a, b)) => vm.heap.string(&m.string[a..b]),
        None => default,
    }
}

fn match_attr(vm: &mut Vm, g: Gc, name: &str) -> Option<R> {
    let m = match_of(g);
    Some(Ok(match name {
        "string" => vm.heap.string(&*m.string),
        "lastindex" => match (1..m.spans.len()).rev().find(|&i| m.spans[i].is_some()) {
            Some(i) => Value::Int(i as i64),
            None => Value::None,
        },
        _ => return None,
    }))
}

fn match_method(vm: &mut Vm, g: Gc, name: &str, args: &[Value]) -> Option<R> {
    let m = match_of(g);
    let r: R = match name {
        "group" | "__getitem__" => (|| {
            if args.is_empty() {
                return Ok(group_value(vm, m, 0, Value::None));
            }
            if args.len() == 1 {
                let i = group_index(m, args[0])?;
                return Ok(group_value(vm, m, i, Value::None));
            }
            let mut parts = Vec::new();
            for a in args {
                let i = group_index(m, *a)?;
                parts.push(group_value(vm, m, i, Value::None));
            }
            Ok(vm.heap.tuple(parts))
        })(),
        "groups" => {
            let default = args.first().copied().or_else(|| vm.take_kw(sym::DEFAULT)).unwrap_or(Value::None);
            let parts: Vec<Value> = (1..m.spans.len()).map(|i| group_value(vm, m, i, default)).collect();
            Ok(vm.heap.tuple(parts))
        }
        "groupdict" => {
            let default = args.first().copied().or_else(|| vm.take_kw(sym::DEFAULT)).unwrap_or(Value::None);
            let mut d = Dict::default();
            let mut err = None;
            for (i, n) in m.names.iter().enumerate() {
                if let Some(n) = n {
                    let k = vm.heap.string(n.clone());
                    let v = group_value(vm, m, i, default);
                    match to_key(k) {
                        Ok(key) => d.insert(key, k, v),
                        Err(e) => err = Some(e),
                    }
                }
            }
            match err {
                Some(e) => Err(e),
                None => Ok(vm.heap.dict(d)),
            }
        }
        "start" | "end" | "span" => (|| {
            let i = match args.first() {
                Some(v) => group_index(m, *v)?,
                None => 0,
            };
            let (a, b) = match m.spans[i] {
                Some((a, b)) => (char_idx(&m.string, a), char_idx(&m.string, b)),
                None => (-1, -1),
            };
            Ok(match name {
                "start" => Value::Int(a),
                "end" => Value::Int(b),
                _ => vm.heap.tuple(vec![Value::Int(a), Value::Int(b)]),
            })
        })(),
        "expand" => (|| {
            let t = want_str(args.first().copied().unwrap_or(Value::None), "template")?;
            let s = expand(t, &m.string, &m.spans, &m.names)?;
            Ok(vm.heap.string(s))
        })(),
        _ => return None,
    };
    Some(r)
}
