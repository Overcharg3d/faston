//! The `json` module: `dumps` / `loads` / `dump` / `load`, matching Python's
//! output format and error messages.

use crate::value::*;
use crate::vm::Vm;

type R = Result<Value, String>;

pub fn register(vm: &mut Vm) {
    let ensure_ascii = vm.syms.intern("ensure_ascii");
    let separators = vm.syms.intern("separators");
    let kws: &'static [u32] = Box::leak(vec![sym::INDENT, sym::SORT_KEYS, ensure_ascii, separators].into_boxed_slice());
    let mk = |vm: &mut Vm, name: &'static str, f: NativeFn, kwargs: &'static [u32]| {
        Value::Obj(vm.heap.alloc(Obj::Native(Native { name, func: f, kwargs })))
    };
    let m = vec![
        ("dumps", mk(vm, "dumps", dumps_native, kws)),
        ("dump", mk(vm, "dump", dump_native, kws)),
        ("loads", mk(vm, "loads", loads_native, &[])),
        ("load", mk(vm, "load", load_native, &[])),
    ];
    vm.define_module("json", m);
}

struct Opts {
    indent: Option<String>,
    sort_keys: bool,
    ensure_ascii: bool,
    item_sep: String,
    key_sep: String,
}

fn opts(vm: &mut Vm) -> Result<Opts, String> {
    let indent = match vm.take_kw(sym::INDENT) {
        None | Some(Value::None) => None,
        Some(Value::Int(n)) => Some(" ".repeat(n.max(0) as usize)),
        Some(v) => match v.as_str() {
            Some(s) => Some(s.to_string()),
            None => return Err(format!("TypeError: indent must be an int or str, not '{}'", v.type_name())),
        },
    };
    let sort_keys = match vm.take_kw(sym::SORT_KEYS) {
        Some(v) => vm.truthy(v)?,
        None => false,
    };
    let ea = vm.syms.intern("ensure_ascii");
    let ensure_ascii = match vm.take_kw(ea) {
        Some(v) => vm.truthy(v)?,
        None => true,
    };
    let (mut item_sep, mut key_sep) = (if indent.is_some() { ",".to_string() } else { ", ".to_string() }, ": ".to_string());
    let seps = vm.syms.intern("separators");
    if let Some(v) = vm.take_kw(seps) {
        if !matches!(v, Value::None) {
            let parts = vm.to_vec(v)?;
            match (parts.first().and_then(|p| p.as_str()), parts.get(1).and_then(|p| p.as_str()), parts.len()) {
                (Some(a), Some(b), 2) => {
                    item_sep = a.to_string();
                    key_sep = b.to_string();
                }
                _ => return Err("TypeError: separators must be a (item_separator, key_separator) pair of strings".into()),
            }
        }
    }
    Ok(Opts { indent, sort_keys, ensure_ascii, item_sep, key_sep })
}

fn dumps_native(vm: &mut Vm, args: &[Value]) -> R {
    if args.len() != 1 {
        return Err(format!("TypeError: dumps() takes exactly 1 positional argument ({} given)", args.len()));
    }
    let o = opts(vm)?;
    let mut out = String::new();
    encode(vm, args[0], &o, 0, &mut out)?;
    Ok(vm.heap.string(out))
}

fn dump_native(vm: &mut Vm, args: &[Value]) -> R {
    if args.len() != 2 {
        return Err(format!("TypeError: dump() takes exactly 2 positional arguments ({} given)", args.len()));
    }
    let o = opts(vm)?;
    let mut out = String::new();
    encode(vm, args[0], &o, 0, &mut out)?;
    crate::osmod::print_to(vm, args[1], &out, false)?;
    Ok(Value::None)
}

fn loads_native(vm: &mut Vm, args: &[Value]) -> R {
    if args.len() != 1 {
        return Err(format!("TypeError: loads() takes exactly 1 argument ({} given)", args.len()));
    }
    let s = args[0]
        .as_str()
        .ok_or_else(|| format!("TypeError: the JSON object must be str, not '{}'", args[0].type_name()))?;
    parse(vm, s)
}

fn load_native(vm: &mut Vm, args: &[Value]) -> R {
    if args.len() != 1 {
        return Err(format!("TypeError: load() takes exactly 1 argument ({} given)", args.len()));
    }
    let read = vm.syms.intern("read");
    let text = crate::builtins::call_method(vm, args[0], read, &[]).or_else(|_| {
        let m = vm.get_attr(args[0], read)?;
        vm.call(m, &[])
    })?;
    let s = text.as_str().ok_or("TypeError: read() must return a str")?.to_string();
    parse(vm, &s)
}

// ---------------------------------------------------------------- encoding

fn type_err(v: Value) -> String {
    format!("TypeError: Object of type {} is not JSON serializable", v.type_name())
}

fn encode_str(s: &str, ascii: bool, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (ascii && (c as u32) > 0x7e) => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{:04x}", unit));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn encode_float(f: f64, out: &mut String) {
    if f.is_nan() {
        out.push_str("NaN");
    } else if f.is_infinite() {
        out.push_str(if f > 0.0 { "Infinity" } else { "-Infinity" });
    } else {
        out.push_str(&fmt_float(f));
    }
}

fn newline(o: &Opts, depth: usize, out: &mut String) {
    if let Some(ind) = &o.indent {
        out.push('\n');
        for _ in 0..depth {
            out.push_str(ind);
        }
    }
}

fn encode(vm: &mut Vm, v: Value, o: &Opts, depth: usize, out: &mut String) -> Result<(), String> {
    if depth > 200 {
        return Err("ValueError: Circular reference detected".into());
    }
    match v {
        Value::None => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Int(i) => out.push_str(&i.to_string()),
        Value::Float(f) => encode_float(f, out),
        Value::Undef => return Err(type_err(v)),
        Value::Obj(g) => match g.get() {
            Obj::Str(s) => encode_str(s, o.ensure_ascii, out),
            Obj::List(_) | Obj::Tuple(_) => {
                let items: Vec<Value> = match g.get() {
                    Obj::List(l) => l.clone(),
                    Obj::Tuple(t) => t.to_vec(),
                    _ => unreachable!(),
                };
                if items.is_empty() {
                    out.push_str("[]");
                    return Ok(());
                }
                out.push('[');
                for (i, it) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(&o.item_sep);
                    }
                    newline(o, depth + 1, out);
                    encode(vm, *it, o, depth + 1, out)?;
                }
                newline(o, depth, out);
                out.push(']');
            }
            Obj::Dict(d) => {
                let mut entries: Vec<(String, Value)> = Vec::with_capacity(d.entries.len());
                for (k, val) in d.entries.iter() {
                    let key = match k {
                        Value::Obj(kg) => match kg.get() {
                            Obj::Str(s) => s.clone(),
                            _ => return Err(format!("TypeError: keys must be str, int, float, bool or None, not {}", k.type_name())),
                        },
                        Value::Int(i) => i.to_string(),
                        Value::Float(f) => {
                            let mut t = String::new();
                            encode_float(*f, &mut t);
                            t
                        }
                        Value::Bool(b) => (if *b { "true" } else { "false" }).to_string(),
                        Value::None => "null".to_string(),
                        Value::Undef => return Err(type_err(*k)),
                    };
                    entries.push((key, *val));
                }
                if o.sort_keys {
                    entries.sort_by(|a, b| a.0.cmp(&b.0));
                }
                if entries.is_empty() {
                    out.push_str("{}");
                    return Ok(());
                }
                out.push('{');
                for (i, (k, val)) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push_str(&o.item_sep);
                    }
                    newline(o, depth + 1, out);
                    encode_str(k, o.ensure_ascii, out);
                    out.push_str(&o.key_sep);
                    encode(vm, *val, o, depth + 1, out)?;
                }
                newline(o, depth, out);
                out.push('}');
            }
            _ => return Err(type_err(v)),
        },
    }
    Ok(())
}

// ---------------------------------------------------------------- decoding

struct Parser<'a> {
    s: &'a str,
    b: &'a [u8],
    i: usize,
}

impl<'a> Parser<'a> {
    fn err(&self, msg: &str, at: usize) -> String {
        let before = &self.s[..at.min(self.s.len())];
        let line = before.matches('\n').count() + 1;
        let col = match before.rfind('\n') {
            Some(p) => before[p + 1..].chars().count() + 1,
            None => before.chars().count() + 1,
        };
        let ch = before.chars().count();
        format!("JSONDecodeError: {}: line {} column {} (char {})", msg, line, col, ch)
    }

    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn starts(&self, lit: &str) -> bool {
        self.s[self.i..].starts_with(lit)
    }

    fn value(&mut self, vm: &mut Vm, depth: usize) -> R {
        if depth > 500 {
            return Err(self.err("Nesting too deep", self.i));
        }
        self.ws();
        let at = self.i;
        let c = match self.b.get(self.i) {
            Some(c) => *c,
            None => return Err(self.err("Expecting value", at)),
        };
        match c {
            b'{' => self.object(vm, depth),
            b'[' => self.array(vm, depth),
            b'"' => {
                let s = self.string()?;
                Ok(vm.heap.string(s))
            }
            b't' if self.starts("true") => {
                self.i += 4;
                Ok(Value::Bool(true))
            }
            b'f' if self.starts("false") => {
                self.i += 5;
                Ok(Value::Bool(false))
            }
            b'n' if self.starts("null") => {
                self.i += 4;
                Ok(Value::None)
            }
            b'N' if self.starts("NaN") => {
                self.i += 3;
                Ok(Value::Float(f64::NAN))
            }
            b'I' if self.starts("Infinity") => {
                self.i += 8;
                Ok(Value::Float(f64::INFINITY))
            }
            b'-' if self.starts("-Infinity") => {
                self.i += 9;
                Ok(Value::Float(f64::NEG_INFINITY))
            }
            b'-' | b'0'..=b'9' => self.number(),
            _ => Err(self.err("Expecting value", at)),
        }
    }

    fn number(&mut self) -> R {
        let start = self.i;
        if self.b[self.i] == b'-' {
            self.i += 1;
        }
        let digits_start = self.i;
        while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
            self.i += 1;
        }
        if self.i == digits_start {
            return Err(self.err("Expecting value", start));
        }
        let mut float = false;
        if self.i < self.b.len() && self.b[self.i] == b'.' && self.b.get(self.i + 1).is_some_and(|c| c.is_ascii_digit()) {
            float = true;
            self.i += 1;
            while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
                self.i += 1;
            }
        }
        if self.i < self.b.len() && matches!(self.b[self.i], b'e' | b'E') {
            let save = self.i;
            self.i += 1;
            if self.i < self.b.len() && matches!(self.b[self.i], b'+' | b'-') {
                self.i += 1;
            }
            let es = self.i;
            while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
                self.i += 1;
            }
            if self.i == es {
                self.i = save;
            } else {
                float = true;
            }
        }
        let text = &self.s[start..self.i];
        if !float {
            if let Ok(n) = text.parse::<i64>() {
                return Ok(Value::Int(n));
            }
        }
        text.parse::<f64>().map(Value::Float).map_err(|_| self.err("Expecting value", start))
    }

    fn string(&mut self) -> Result<String, String> {
        let start = self.i;
        self.i += 1; // opening quote
        let mut out = String::new();
        loop {
            let c = match self.s[self.i..].chars().next() {
                Some(c) => c,
                None => return Err(self.err("Unterminated string starting at", start)),
            };
            match c {
                '"' => {
                    self.i += 1;
                    return Ok(out);
                }
                '\\' => {
                    let esc_at = self.i;
                    self.i += 1;
                    let e = match self.b.get(self.i) {
                        Some(e) => *e,
                        None => return Err(self.err("Unterminated string starting at", start)),
                    };
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4().ok_or_else(|| self.err("Invalid \\uXXXX escape", esc_at))?;
                            let cp = if (0xD800..0xDC00).contains(&hi) && self.starts("\\u") {
                                let save = self.i;
                                self.i += 2;
                                match self.hex4() {
                                    Some(lo) if (0xDC00..0xE000).contains(&lo) => 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00),
                                    _ => {
                                        self.i = save;
                                        hi
                                    }
                                }
                            } else {
                                hi
                            };
                            out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                        }
                        _ => return Err(self.err("Invalid \\escape", esc_at)),
                    }
                }
                c if (c as u32) < 0x20 => return Err(self.err("Invalid control character at", self.i)),
                c => {
                    out.push(c);
                    self.i += c.len_utf8();
                }
            }
        }
    }

    fn hex4(&mut self) -> Option<u32> {
        let h = self.s.get(self.i..self.i + 4)?;
        let v = u32::from_str_radix(h, 16).ok()?;
        self.i += 4;
        Some(v)
    }

    fn array(&mut self, vm: &mut Vm, depth: usize) -> R {
        self.i += 1;
        let list = vm.heap.list(Vec::new());
        let mark = vm.temp_mark();
        vm.push_temp(list)?;
        let r = (|| -> R {
            self.ws();
            if self.b.get(self.i) == Some(&b']') {
                self.i += 1;
                return Ok(list);
            }
            loop {
                let v = self.value(vm, depth + 1)?;
                if let Value::Obj(g) = list {
                    if let Obj::List(l) = g.get_mut() {
                        l.push(v);
                    }
                }
                self.ws();
                match self.b.get(self.i) {
                    Some(b',') => self.i += 1,
                    Some(b']') => {
                        self.i += 1;
                        return Ok(list);
                    }
                    _ => return Err(self.err("Expecting ',' delimiter", self.i)),
                }
            }
        })();
        vm.temp_reset(mark);
        r
    }

    fn object(&mut self, vm: &mut Vm, depth: usize) -> R {
        self.i += 1;
        let dict = vm.heap.dict(Dict::default());
        let mark = vm.temp_mark();
        vm.push_temp(dict)?;
        let r = (|| -> R {
            self.ws();
            if self.b.get(self.i) == Some(&b'}') {
                self.i += 1;
                return Ok(dict);
            }
            loop {
                self.ws();
                if self.b.get(self.i) != Some(&b'"') {
                    return Err(self.err("Expecting property name enclosed in double quotes", self.i));
                }
                let k = self.string()?;
                self.ws();
                if self.b.get(self.i) != Some(&b':') {
                    return Err(self.err("Expecting ':' delimiter", self.i));
                }
                self.i += 1;
                let key = vm.heap.string(k);
                let mark2 = vm.temp_mark();
                vm.push_temp(key)?;
                let v = self.value(vm, depth + 1);
                vm.temp_reset(mark2);
                let v = v?;
                if let Value::Obj(g) = dict {
                    if let Obj::Dict(d) = g.get_mut() {
                        d.insert(to_key(key)?, key, v);
                    }
                }
                self.ws();
                match self.b.get(self.i) {
                    Some(b',') => self.i += 1,
                    Some(b'}') => {
                        self.i += 1;
                        return Ok(dict);
                    }
                    _ => return Err(self.err("Expecting ',' delimiter", self.i)),
                }
            }
        })();
        vm.temp_reset(mark);
        r
    }
}

fn parse(vm: &mut Vm, s: &str) -> R {
    let mut p = Parser { s, b: s.as_bytes(), i: 0 };
    let v = p.value(vm, 0)?;
    p.ws();
    if p.i < p.b.len() {
        return Err(p.err("Extra data", p.i));
    }
    Ok(v)
}
