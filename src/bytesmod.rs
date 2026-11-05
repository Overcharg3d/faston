//! `bytes` and `bytearray`: constructors, methods, text encodings, and the
//! int <-> bytes conversions (`int.to_bytes`, `int.from_bytes`).

use crate::ops;
use crate::value::*;
use crate::vm::Vm;

type R = Result<Value, String>;

pub fn register(vm: &mut Vm) {
    vm.define_native_kw("bytes", bytes_new, &[sym::ENCODING, sym::ERRORS]);
    vm.define_native_kw("bytearray", bytearray_new, &[sym::ENCODING, sym::ERRORS]);
}

fn bytes_new(vm: &mut Vm, args: &[Value]) -> R {
    let b = construct(vm, args, "bytes")?;
    Ok(vm.heap.bytes(b))
}

fn bytearray_new(vm: &mut Vm, args: &[Value]) -> R {
    let b = construct(vm, args, "bytearray")?;
    Ok(vm.heap.bytearray(b))
}

/// `bytes(x)`: from nothing, a size, a str and an encoding, bytes, or ints.
fn construct(vm: &mut Vm, args: &[Value], what: &str) -> Result<Vec<u8>, String> {
    if args.len() > 3 {
        return Err(format!("TypeError: {}() takes at most 3 arguments ({} given)", what, args.len()));
    }
    let encoding = vm.take_kw(sym::ENCODING).or(args.get(1).copied());
    let errors = vm.take_kw(sym::ERRORS).or(args.get(2).copied());
    let Some(&v) = args.first() else { return Ok(Vec::new()) };
    if let Some(s) = v.as_str() {
        let enc = match encoding {
            Some(e) => want_str(e, "encoding")?,
            None => return Err("TypeError: string argument without an encoding".into()),
        };
        let errors = match errors {
            Some(e) => want_str(e, "errors")?,
            None => "strict",
        };
        return encode(s, enc, errors);
    }
    if encoding.is_some() {
        return Err("TypeError: encoding without a string argument".into());
    }
    match v {
        Value::Int(n) if n < 0 => Err("ValueError: negative count".into()),
        Value::Int(n) => Ok(vec![0u8; n as usize]),
        _ => match v.as_bytes() {
            Some(b) => Ok(b.to_vec()),
            None => {
                let items = vm.to_vec(v)?;
                items.into_iter().map(byte_value).collect()
            }
        },
    }
}

/// An int in range(0, 256).
fn byte_value(v: Value) -> Result<u8, String> {
    match v {
        Value::Int(i) if (0..=255).contains(&i) => Ok(i as u8),
        Value::Bool(b) => Ok(b as u8),
        Value::Int(_) => Err("ValueError: byte must be in range(0, 256)".into()),
        _ => Err(format!("TypeError: '{}' object cannot be interpreted as an integer", v.type_name())),
    }
}

fn want_str<'a>(v: Value, what: &str) -> Result<&'a str, String> {
    v.as_str().ok_or_else(|| format!("TypeError: {} must be a str, not '{}'", what, v.type_name()))
}

fn want_int(v: Value, what: &str) -> Result<i64, String> {
    match v {
        Value::Int(i) => Ok(i),
        Value::Bool(b) => Ok(b as i64),
        _ => Err(format!("TypeError: {} must be an int, not '{}'", what, v.type_name())),
    }
}

/// A bytes-like argument (an int also counts, as a single byte, where Python allows it).
fn want_bytes<'a>(v: Value, allow_int: bool) -> Result<std::borrow::Cow<'a, [u8]>, String> {
    if let Some(b) = v.as_bytes() {
        return Ok(std::borrow::Cow::Borrowed(b));
    }
    if allow_int {
        if let Value::Int(_) = v {
            return Ok(std::borrow::Cow::Owned(vec![byte_value(v)?]));
        }
    }
    Err(format!("TypeError: a bytes-like object is required, not '{}'", v.type_name()))
}

fn norm_encoding(enc: &str) -> String {
    enc.trim().to_ascii_lowercase().replace('_', "-")
}

/// `str.encode` / `bytes(s, encoding)`.
pub fn encode(s: &str, encoding: &str, errors: &str) -> Result<Vec<u8>, String> {
    let limit = match norm_encoding(encoding).as_str() {
        "utf-8" | "utf8" => return Ok(s.as_bytes().to_vec()),
        "ascii" | "us-ascii" => 128u32,
        "latin-1" | "latin1" | "iso-8859-1" | "iso8859-1" => 256,
        _ => return Err(format!("LookupError: unknown encoding: {}", encoding)),
    };
    let name = if limit == 128 { "ascii" } else { "latin-1" };
    let mut out = Vec::with_capacity(s.len());
    for (i, c) in s.chars().enumerate() {
        if (c as u32) < limit {
            out.push(c as u32 as u8);
            continue;
        }
        match errors {
            "ignore" => {}
            "replace" => out.push(b'?'),
            _ => {
                return Err(format!(
                    "UnicodeEncodeError: '{}' codec can't encode character {} in position {}: ordinal not in range({})",
                    name,
                    match c as u32 {
                        n if n < 0x100 => format!("'\\x{:02x}'", n),
                        n if n < 0x10000 => format!("'\\u{:04x}'", n),
                        n => format!("'\\U{:08x}'", n),
                    },
                    i,
                    limit
                ))
            }
        }
    }
    Ok(out)
}

/// `bytes.decode`.
pub fn decode(b: &[u8], encoding: &str, errors: &str) -> Result<String, String> {
    match norm_encoding(encoding).as_str() {
        "utf-8" | "utf8" => match std::str::from_utf8(b) {
            Ok(s) => Ok(s.to_string()),
            Err(e) => match errors {
                "replace" | "ignore" => {
                    let mut out = String::with_capacity(b.len());
                    for chunk in b.utf8_chunks() {
                        out.push_str(chunk.valid());
                        if !chunk.invalid().is_empty() && errors == "replace" {
                            out.push('\u{fffd}');
                        }
                    }
                    Ok(out)
                }
                _ => {
                    let pos = e.valid_up_to();
                    let why = if e.error_len().is_none() { "unexpected end of data" } else { "invalid start byte" };
                    Err(format!(
                        "UnicodeDecodeError: 'utf-8' codec can't decode byte 0x{:02x} in position {}: {}",
                        b[pos], pos, why
                    ))
                }
            },
        },
        "ascii" | "us-ascii" => {
            let mut out = String::with_capacity(b.len());
            for (i, &c) in b.iter().enumerate() {
                if c < 128 {
                    out.push(c as char);
                } else {
                    match errors {
                        "ignore" => {}
                        "replace" => out.push('\u{fffd}'),
                        _ => {
                            return Err(format!(
                                "UnicodeDecodeError: 'ascii' codec can't decode byte 0x{:02x} in position {}: ordinal not in range(128)",
                                c, i
                            ))
                        }
                    }
                }
            }
            Ok(out)
        }
        "latin-1" | "latin1" | "iso-8859-1" | "iso8859-1" => Ok(b.iter().map(|&c| c as char).collect()),
        _ => Err(format!("LookupError: unknown encoding: {}", encoding)),
    }
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from > hay.len() {
        return None;
    }
    if needle.is_empty() {
        return Some(from);
    }
    hay[from..].windows(needle.len()).position(|w| w == needle).map(|i| i + from)
}

fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(hay.len());
    }
    hay.windows(needle.len()).rposition(|w| w == needle)
}

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

fn arity(args: &[Value], min: usize, max: usize, what: &str) -> Result<(), String> {
    if args.len() < min || args.len() > max {
        let expected = if min == max { format!("{}", min) } else { format!("{} to {}", min, max) };
        return Err(format!(
            "TypeError: {}() takes {} argument{} ({} given)",
            what,
            expected,
            if max == 1 { "" } else { "s" },
            args.len()
        ));
    }
    Ok(())
}

/// Methods of `bytes` / `bytearray` objects.
pub fn bytes_method(vm: &mut Vm, g: Gc, name: u32, args: &[Value]) -> Option<R> {
    let (data, mutable): (&[u8], bool) = match g.get() {
        Obj::Bytes(b) => (b, false),
        Obj::ByteArray(b) => (b, true),
        _ => return None,
    };
    let ty = if mutable { "bytearray" } else { "bytes" };
    let mname = vm.syms.name(name).to_string();
    let what = format!("{}.{}", ty, mname);
    let c = |min, max| arity(args, min, max, &what);
    let r = (|| -> R {
        Ok(match mname.as_str() {
            "decode" => {
                c(0, 2)?;
                let enc = vm.take_kw(sym::ENCODING).or(args.first().copied());
                let err = vm.take_kw(sym::ERRORS).or(args.get(1).copied());
                let enc = match enc {
                    Some(e) => want_str(e, "encoding")?,
                    None => "utf-8",
                };
                let err = match err {
                    Some(e) => want_str(e, "errors")?,
                    None => "strict",
                };
                let s = decode(data, enc, err)?;
                vm.heap.string(s)
            }
            "hex" => {
                c(0, 0)?;
                let mut s = String::with_capacity(data.len() * 2);
                for b in data {
                    s.push_str(&format!("{:02x}", b));
                }
                vm.heap.string(s)
            }
            "count" => {
                c(1, 1)?;
                let n = want_bytes(args[0], true)?;
                if n.is_empty() {
                    return Ok(Value::Int(data.len() as i64 + 1));
                }
                let (mut i, mut k) = (0, 0);
                while let Some(p) = find(data, &n, i) {
                    k += 1;
                    i = p + n.len();
                }
                Value::Int(k)
            }
            "find" | "index" => {
                c(1, 2)?;
                let n = want_bytes(args[0], true)?;
                let start = match args.get(1) {
                    Some(&s) => {
                        let s = want_int(s, "start")?;
                        if s < 0 { (s + data.len() as i64).max(0) as usize } else { s as usize }
                    }
                    None => 0,
                };
                match find(data, &n, start) {
                    Some(i) => Value::Int(i as i64),
                    None if mname == "find" => Value::Int(-1),
                    None => return Err("ValueError: subsection not found".into()),
                }
            }
            "rfind" | "rindex" => {
                c(1, 1)?;
                let n = want_bytes(args[0], true)?;
                match rfind(data, &n) {
                    Some(i) => Value::Int(i as i64),
                    None if mname == "rfind" => Value::Int(-1),
                    None => return Err("ValueError: subsection not found".into()),
                }
            }
            "startswith" | "endswith" => {
                c(1, 1)?;
                let prefixes: Vec<Value> = match args[0] {
                    Value::Obj(t) if matches!(t.get(), Obj::Tuple(_)) => vm.to_vec(args[0])?,
                    v => vec![v],
                };
                let mut hit = false;
                for p in prefixes {
                    let p = want_bytes(p, false)?;
                    hit |= if mname == "startswith" { data.starts_with(&p) } else { data.ends_with(&p) };
                }
                Value::Bool(hit)
            }
            "split" => {
                c(0, 2)?;
                let sep = vm.take_kw(sym::SEP).or(args.first().copied()).unwrap_or(Value::None);
                let maxsplit = match vm.take_kw(sym::MAXSPLIT).or(args.get(1).copied()) {
                    Some(v) => want_int(v, "maxsplit")?,
                    None => -1,
                };
                let mut parts: Vec<Vec<u8>> = Vec::new();
                match sep {
                    Value::None => {
                        let mut i = 0;
                        while i < data.len() {
                            while i < data.len() && is_space(data[i]) {
                                i += 1;
                            }
                            if i >= data.len() {
                                break;
                            }
                            if maxsplit >= 0 && parts.len() as i64 == maxsplit {
                                let mut rest = &data[i..];
                                while let Some((&last, init)) = rest.split_last() {
                                    if !is_space(last) {
                                        break;
                                    }
                                    rest = init;
                                }
                                parts.push(rest.to_vec());
                                break;
                            }
                            let st = i;
                            while i < data.len() && !is_space(data[i]) {
                                i += 1;
                            }
                            parts.push(data[st..i].to_vec());
                        }
                    }
                    s => {
                        let s = want_bytes(s, false)?;
                        if s.is_empty() {
                            return Err("ValueError: empty separator".into());
                        }
                        let mut i = 0;
                        while let Some(p) = find(data, &s, i) {
                            if maxsplit >= 0 && parts.len() as i64 == maxsplit {
                                break;
                            }
                            parts.push(data[i..p].to_vec());
                            i = p + s.len();
                        }
                        parts.push(data[i..].to_vec());
                    }
                }
                let items = parts.into_iter().map(|p| ops::new_bytes(&mut vm.heap, p, mutable)).collect();
                vm.heap.list(items)
            }
            "join" => {
                c(1, 1)?;
                let items = vm.to_vec(args[0])?;
                let mut out = Vec::new();
                for (i, it) in items.iter().enumerate() {
                    if i > 0 {
                        out.extend_from_slice(data);
                    }
                    match it.as_bytes() {
                        Some(b) => out.extend_from_slice(b),
                        None => {
                            return Err(format!(
                                "TypeError: sequence item {}: expected a bytes-like object, {} found",
                                i,
                                it.type_name()
                            ))
                        }
                    }
                }
                ops::new_bytes(&mut vm.heap, out, mutable)
            }
            "replace" => {
                c(2, 3)?;
                let old = want_bytes(args[0], false)?;
                let new = want_bytes(args[1], false)?;
                let mut count = match args.get(2) {
                    Some(&n) => want_int(n, "count")?,
                    None => -1,
                };
                let mut out = Vec::with_capacity(data.len());
                let mut i = 0;
                if old.is_empty() {
                    // insert `new` between all bytes (Python semantics)
                    for &b in data {
                        if count != 0 {
                            out.extend_from_slice(&new);
                            count -= 1;
                        }
                        out.push(b);
                    }
                    if count != 0 {
                        out.extend_from_slice(&new);
                    }
                } else {
                    while count != 0 {
                        match find(data, &old, i) {
                            Some(p) => {
                                out.extend_from_slice(&data[i..p]);
                                out.extend_from_slice(&new);
                                i = p + old.len();
                                count -= 1;
                            }
                            None => break,
                        }
                    }
                    out.extend_from_slice(&data[i..]);
                }
                ops::new_bytes(&mut vm.heap, out, mutable)
            }
            "strip" | "lstrip" | "rstrip" => {
                c(0, 1)?;
                let set: Option<Vec<u8>> = match args.first() {
                    None | Some(Value::None) => None,
                    Some(&v) => Some(want_bytes(v, false)?.into_owned()),
                };
                let drop = |b: u8| match &set {
                    Some(s) => s.contains(&b),
                    None => is_space(b),
                };
                let mut a = 0;
                let mut z = data.len();
                if mname != "rstrip" {
                    while a < z && drop(data[a]) {
                        a += 1;
                    }
                }
                if mname != "lstrip" {
                    while z > a && drop(data[z - 1]) {
                        z -= 1;
                    }
                }
                ops::new_bytes(&mut vm.heap, data[a..z].to_vec(), mutable)
            }
            "upper" | "lower" => {
                c(0, 0)?;
                let out =
                    if mname == "upper" { data.to_ascii_uppercase() } else { data.to_ascii_lowercase() };
                ops::new_bytes(&mut vm.heap, out, mutable)
            }
            "isdigit" | "isalpha" | "isalnum" | "isspace" => {
                c(0, 0)?;
                let f = |b: &u8| match mname.as_str() {
                    "isdigit" => b.is_ascii_digit(),
                    "isalpha" => b.is_ascii_alphabetic(),
                    "isalnum" => b.is_ascii_alphanumeric(),
                    _ => is_space(*b),
                };
                Value::Bool(!data.is_empty() && data.iter().all(f))
            }
            "copy" if mutable => {
                c(0, 0)?;
                vm.heap.bytearray(data.to_vec())
            }
            "append" | "extend" | "pop" | "insert" | "remove" | "clear" | "reverse" if mutable => {
                return bytearray_mutate(vm, g, &mname, args, &what);
            }
            _ => return Err(format!("AttributeError: '{}' object has no attribute '{}'", ty, mname)),
        })
    })();
    Some(r)
}

fn bytearray_mutate(vm: &mut Vm, g: Gc, name: &str, args: &[Value], what: &str) -> R {
    let c = |min, max| arity(args, min, max, what);
    // read what's needed before taking the mutable borrow
    let extra: Vec<u8> = match name {
        "extend" => {
            c(1, 1)?;
            match args[0].as_bytes() {
                Some(b) => b.to_vec(),
                None => {
                    let items = vm.to_vec(args[0])?;
                    items.into_iter().map(byte_value).collect::<Result<_, _>>()?
                }
            }
        }
        _ => Vec::new(),
    };
    let b = match g.get_mut() {
        Obj::ByteArray(b) => b,
        _ => unreachable!(),
    };
    Ok(match name {
        "append" => {
            c(1, 1)?;
            b.push(byte_value(args[0])?);
            Value::None
        }
        "extend" => {
            b.extend_from_slice(&extra);
            Value::None
        }
        "pop" => {
            c(0, 1)?;
            if b.is_empty() {
                return Err("IndexError: pop from empty bytearray".into());
            }
            let i = match args.first() {
                Some(&i) => want_int(i, "index")?,
                None => -1,
            };
            let n = b.len() as i64;
            let i = if i < 0 { i + n } else { i };
            if i < 0 || i >= n {
                return Err("IndexError: pop index out of range".into());
            }
            Value::Int(b.remove(i as usize) as i64)
        }
        "insert" => {
            c(2, 2)?;
            let n = b.len() as i64;
            let i = want_int(args[0], "index")?;
            let i = if i < 0 { (i + n).max(0) } else { i.min(n) };
            b.insert(i as usize, byte_value(args[1])?);
            Value::None
        }
        "remove" => {
            c(1, 1)?;
            let v = byte_value(args[0])?;
            match b.iter().position(|&x| x == v) {
                Some(i) => {
                    b.remove(i);
                    Value::None
                }
                None => return Err("ValueError: value not found in bytearray".into()),
            }
        }
        "clear" => {
            c(0, 0)?;
            b.clear();
            Value::None
        }
        _ => {
            c(0, 0)?;
            b.reverse();
            Value::None
        }
    })
}

/// `bytes.fromhex(s)` / `bytearray.fromhex(s)`.
fn fromhex(s: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    let digit = |c: u8| (c as char).to_digit(16);
    while i < b.len() {
        if is_space(b[i]) {
            i += 1;
            continue;
        }
        let hi = digit(b[i]);
        let lo = b.get(i + 1).and_then(|&c| digit(c));
        match (hi, lo) {
            (Some(h), Some(l)) => out.push((h * 16 + l) as u8),
            (None, _) => {
                return Err(format!("ValueError: non-hexadecimal number found in fromhex() arg at position {}", i))
            }
            (Some(_), None) => {
                return Err(format!(
                    "ValueError: non-hexadecimal number found in fromhex() arg at position {}",
                    i + 1
                ))
            }
        }
        i += 2;
    }
    Ok(out)
}

fn byteorder(vm: &mut Vm, v: Option<Value>) -> Result<bool, String> {
    let v = vm.take_kw(sym::BYTEORDER).or(v);
    match v {
        None => Ok(false),
        Some(v) => match v.as_str() {
            Some("big") => Ok(false),
            Some("little") => Ok(true),
            Some(_) => Err("ValueError: byteorder must be either 'little' or 'big'".into()),
            None => Err(format!("TypeError: byteorder must be str, not {}", v.type_name())),
        },
    }
}

/// Methods called on ints: `n.to_bytes(...)`, `n.bit_length()`.
pub fn int_method(vm: &mut Vm, i: i64, name: u32, args: &[Value]) -> Option<R> {
    let r = match name {
        sym::TO_BYTES => (|| -> R {
            arity(args, 0, 2, "int.to_bytes")?;
            let length = match vm.take_kw(sym::LENGTH).or(args.first().copied()) {
                Some(v) => want_int(v, "length")?,
                None => 1,
            };
            if length < 0 {
                return Err("ValueError: length argument must be non-negative".into());
            }
            let little = byteorder(vm, args.get(1).copied())?;
            let signed = vm.take_kw(sym::SIGNED).map(|v| v.truthy()).unwrap_or(false);
            if !signed && i < 0 {
                return Err("OverflowError: can't convert negative int to unsigned".into());
            }
            let bits = 8 * length as u32;
            let fits = if bits >= 64 {
                true
            } else if signed {
                let lim = 1i64 << (bits.max(1) - 1);
                bits > 0 && i >= -lim && i < lim || (bits == 0 && i == 0)
            } else {
                (i as u64) < (1u64 << bits)
            };
            if !fits {
                return Err("OverflowError: int too big to convert".into());
            }
            let fill = if i < 0 { 0xff } else { 0 };
            let le = (i as i128).to_le_bytes();
            let mut out: Vec<u8> = (0..length as usize).map(|k| le.get(k).copied().unwrap_or(fill)).collect();
            if !little {
                out.reverse();
            }
            Ok(vm.heap.bytes(out))
        })(),
        sym::BIT_LENGTH => {
            if let Err(e) = arity(args, 0, 0, "int.bit_length") {
                return Some(Err(e));
            }
            Ok(Value::Int(64 - i.unsigned_abs().leading_zeros() as i64))
        }
        _ => return None,
    };
    Some(r)
}

/// Methods called on the built-in type functions: `int.from_bytes`, `bytes.fromhex`.
pub fn type_method(vm: &mut Vm, ty: &str, name: u32, args: &[Value]) -> Option<R> {
    match (ty, name) {
        ("int", sym::FROM_BYTES) => Some((|| -> R {
            arity(args, 1, 2, "int.from_bytes")?;
            let data: Vec<u8> = match args[0].as_bytes() {
                Some(b) => b.to_vec(),
                None => {
                    let items = vm.to_vec(args[0])?;
                    items.into_iter().map(byte_value).collect::<Result<_, _>>()?
                }
            };
            let little = byteorder(vm, args.get(1).copied())?;
            let signed = vm.take_kw(sym::SIGNED).map(|v| v.truthy()).unwrap_or(false);
            let mut be = data;
            if little {
                be.reverse();
            }
            let negative = signed && be.first().is_some_and(|&b| b & 0x80 != 0);
            // drop sign-extension bytes beyond 16
            let ext = if negative { 0xff } else { 0 };
            while be.len() > 16 && be[0] == ext {
                be.remove(0);
            }
            if be.len() > 16 {
                return Err("OverflowError: int too large to convert".into());
            }
            let mut v: u128 = 0;
            for &b in &be {
                v = (v << 8) | b as u128;
            }
            let n: i128 = if negative && be.len() < 16 {
                v as i128 - (1i128 << (8 * be.len()))
            } else {
                v as i128
            };
            i64::try_from(n).map(Value::Int).map_err(|_| "OverflowError: int too large to convert".into())
        })()),
        ("bytes" | "bytearray", sym::FROMHEX) => Some((|| -> R {
            arity(args, 1, 1, &format!("{}.fromhex", ty))?;
            let b = fromhex(want_str(args[0], "fromhex() argument")?)?;
            Ok(ops::new_bytes(&mut vm.heap, b, ty == "bytearray"))
        })()),
        _ => None,
    }
}
