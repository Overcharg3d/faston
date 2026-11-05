//! Slow-path operator semantics (the VM handles int/float fast paths inline).
//! Semantics follow Python: `/` is true division, `//` floors, `%` takes the
//! divisor's sign, ints and floats compare and hash equal.

use std::cmp::Ordering;

use crate::value::*;

type R = Result<Value, String>;

#[inline]
fn int_like(v: Value) -> Option<i64> {
    match v {
        Value::Int(i) => Some(i),
        Value::Bool(b) => Some(b as i64),
        _ => None,
    }
}

#[inline]
fn num(v: Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(i as f64),
        Value::Float(f) => Some(f),
        Value::Bool(b) => Some(b as i64 as f64),
        _ => None,
    }
}

fn list_of<'a>(v: Value) -> Option<&'a Vec<Value>> {
    match v {
        Value::Obj(g) => match g.get() {
            Obj::List(l) => Some(l),
            _ => None,
        },
        _ => None,
    }
}

fn tuple_of<'a>(v: Value) -> Option<&'a [Value]> {
    match v {
        Value::Obj(g) => match g.get() {
            Obj::Tuple(t) => Some(t),
            _ => None,
        },
        _ => None,
    }
}

/// Items of a list or tuple.
fn seq_of<'a>(v: Value) -> Option<&'a [Value]> {
    match v {
        Value::Obj(g) => match g.get() {
            Obj::List(l) => Some(l),
            Obj::Tuple(t) => Some(t),
            _ => None,
        },
        _ => None,
    }
}

/// Contents of a bytes / bytearray, and whether it is a bytearray.
fn bytes_of<'a>(v: Value) -> Option<(&'a [u8], bool)> {
    match v {
        Value::Obj(g) => match g.get() {
            Obj::Bytes(b) => Some((b, false)),
            Obj::ByteArray(b) => Some((b, true)),
            _ => None,
        },
        _ => None,
    }
}

/// A new bytes, or bytearray if `mutable`.
pub fn new_bytes(heap: &mut Heap, b: Vec<u8>, mutable: bool) -> Value {
    if mutable { heap.bytearray(b) } else { heap.bytes(b) }
}

/// The members of an enum class (each once, in definition order).
pub fn enum_members(v: Value) -> Option<Vec<Value>> {
    let Value::Obj(g) = v else { return None };
    let Obj::Class(c) = g.get() else { return None };
    let Value::Obj(d) = c.method(sym::D_MEMBERS)? else { return None };
    let Obj::Dict(d) = d.get() else { return None };
    let mut out: Vec<Value> = Vec::new();
    for (_, m) in d.entries.iter() {
        if !out.iter().any(|x| matches!((x, m), (Value::Obj(a), Value::Obj(b)) if a == b)) {
            out.push(*m);
        }
    }
    Some(out)
}

pub fn set_of<'a>(v: Value) -> Option<&'a Dict> {
    match v {
        Value::Obj(g) => match g.get() {
            Obj::Set(d) => Some(d),
            _ => None,
        },
        _ => None,
    }
}

/// Builds a set from any iterable.
pub fn make_set(heap: &mut Heap, items: Vec<Value>) -> R {
    let mut d = Dict::default();
    for x in items {
        d.insert(to_key(x)?, x, Value::None);
    }
    Ok(heap.set(d))
}

fn unsupported(op: &str, a: Value, b: Value) -> String {
    format!(
        "TypeError: unsupported operand type(s) for {}: '{}' and '{}'",
        op,
        a.type_name(),
        b.type_name()
    )
}

pub fn overflow() -> String {
    "OverflowError: integer result out of 64-bit range".into()
}

fn zero_div() -> String {
    "ZeroDivisionError: division by zero".into()
}

fn repeat_count(n: i64) -> usize {
    if n < 0 { 0 } else { n as usize }
}

#[inline(never)]
pub fn add(heap: &mut Heap, a: Value, b: Value) -> R {
    if let (Some(x), Some(y)) = (a.as_str(), b.as_str()) {
        let mut s = String::with_capacity(x.len() + y.len());
        s.push_str(x);
        s.push_str(y);
        return Ok(heap.string(s));
    }
    if let (Some(x), Some(y)) = (list_of(a), list_of(b)) {
        let mut v = Vec::with_capacity(x.len() + y.len());
        v.extend_from_slice(x);
        v.extend_from_slice(y);
        return Ok(heap.list(v));
    }
    if let (Some((x, m)), Some((y, _))) = (bytes_of(a), bytes_of(b)) {
        let mut v = Vec::with_capacity(x.len() + y.len());
        v.extend_from_slice(x);
        v.extend_from_slice(y);
        return Ok(new_bytes(heap, v, m));
    }
    if let (Some(x), Some(y)) = (tuple_of(a), tuple_of(b)) {
        let mut v = Vec::with_capacity(x.len() + y.len());
        v.extend_from_slice(x);
        v.extend_from_slice(y);
        return Ok(heap.tuple(v));
    }
    match (int_like(a), int_like(b)) {
        (Some(x), Some(y)) => x.checked_add(y).map(Value::Int).ok_or_else(overflow),
        _ => match (num(a), num(b)) {
            (Some(x), Some(y)) => Ok(Value::Float(x + y)),
            _ => Err(unsupported("+", a, b)),
        },
    }
}

#[inline(never)]
pub fn sub(heap: &mut Heap, a: Value, b: Value) -> R {
    if let (Some(x), Some(y)) = (set_of(a), set_of(b)) {
        let keep = x.entries.iter().map(|(k, _)| *k).filter(|k| !y.contains(&to_key(*k).unwrap()));
        return make_set(heap, keep.collect());
    }
    match (int_like(a), int_like(b)) {
        (Some(x), Some(y)) => x.checked_sub(y).map(Value::Int).ok_or_else(overflow),
        _ => match (num(a), num(b)) {
            (Some(x), Some(y)) => Ok(Value::Float(x - y)),
            _ => Err(unsupported("-", a, b)),
        },
    }
}

#[inline(never)]
pub fn mul(heap: &mut Heap, a: Value, b: Value) -> R {
    let (seq, n) = match (int_like(a), int_like(b)) {
        (Some(x), Some(y)) => return x.checked_mul(y).map(Value::Int).ok_or_else(overflow),
        (None, Some(n)) => (a, n),
        (Some(n), None) => (b, n),
        (None, None) => {
            return match (num(a), num(b)) {
                (Some(x), Some(y)) => Ok(Value::Float(x * y)),
                _ => Err(unsupported("*", a, b)),
            }
        }
    };
    if let Some(s) = seq.as_str() {
        return Ok(heap.string(s.repeat(repeat_count(n))));
    }
    if let Some((b, m)) = bytes_of(seq) {
        return Ok(new_bytes(heap, b.repeat(repeat_count(n)), m));
    }
    if let Some(l) = list_of(seq) {
        let times = repeat_count(n);
        let mut v = Vec::with_capacity(l.len() * times);
        for _ in 0..times {
            v.extend_from_slice(l);
        }
        return Ok(heap.list(v));
    }
    if let Some(t) = tuple_of(seq) {
        let times = repeat_count(n);
        let mut v = Vec::with_capacity(t.len() * times);
        for _ in 0..times {
            v.extend_from_slice(t);
        }
        return Ok(heap.tuple(v));
    }
    match (num(a), num(b)) {
        (Some(x), Some(y)) => Ok(Value::Float(x * y)),
        _ => Err(unsupported("*", a, b)),
    }
}

/// `&`, `|`, `^` on ints (bitwise) and sets (intersection, union, symmetric difference).
#[inline(never)]
pub fn bitwise(heap: &mut Heap, a: Value, b: Value, op: char) -> R {
    if let (Some(x), Some(y)) = (int_like(a), int_like(b)) {
        let r = match op {
            '&' => x & y,
            '|' => x | y,
            _ => x ^ y,
        };
        return Ok(if matches!((a, b), (Value::Bool(_), Value::Bool(_))) {
            Value::Bool(r != 0)
        } else {
            Value::Int(r)
        });
    }
    if let (Some(x), Some(y)) = (set_of(a), set_of(b)) {
        let has = |d: &Dict, k: Value| d.contains(&to_key(k).unwrap());
        let items: Vec<Value> = match op {
            '&' => x.entries.iter().map(|(k, _)| *k).filter(|k| has(y, *k)).collect(),
            '|' => x.entries.iter().chain(y.entries.iter()).map(|(k, _)| *k).collect(),
            _ => x
                .entries
                .iter()
                .map(|(k, _)| *k)
                .filter(|k| !has(y, *k))
                .chain(y.entries.iter().map(|(k, _)| *k).filter(|k| !has(x, *k)))
                .collect(),
        };
        return make_set(heap, items);
    }
    Err(unsupported(&op.to_string(), a, b))
}

#[inline(never)]
pub fn shift(a: Value, b: Value, left: bool) -> R {
    match (int_like(a), int_like(b)) {
        (Some(_), Some(n)) if n < 0 => Err("ValueError: negative shift count".into()),
        (Some(x), Some(n)) => {
            if left {
                if n >= 64 || (x != 0 && (x.unsigned_abs().leading_zeros() as i64) <= n) {
                    if x == 0 {
                        return Ok(Value::Int(0));
                    }
                    return Err(overflow());
                }
                Ok(Value::Int(x << n))
            } else {
                Ok(Value::Int(if n >= 64 { if x < 0 { -1 } else { 0 } } else { x >> n }))
            }
        }
        _ => Err(unsupported(if left { "<<" } else { ">>" }, a, b)),
    }
}

#[inline(never)]
pub fn invert(a: Value) -> R {
    match int_like(a) {
        Some(x) => Ok(Value::Int(!x)),
        None => Err(format!("TypeError: bad operand type for unary ~: '{}'", a.type_name())),
    }
}

#[inline(never)]
pub fn div(_: &mut Heap, a: Value, b: Value) -> R {
    match (num(a), num(b)) {
        (Some(_), Some(y)) if y == 0.0 => Err(zero_div()),
        (Some(x), Some(y)) => Ok(Value::Float(x / y)),
        _ => Err(unsupported("/", a, b)),
    }
}

#[inline]
pub fn int_floordiv(x: i64, y: i64) -> Option<i64> {
    if y == 0 {
        return None;
    }
    let q = x.checked_div(y)?;
    if (x % y != 0) && ((x < 0) != (y < 0)) {
        Some(q - 1)
    } else {
        Some(q)
    }
}

#[inline]
pub fn int_mod(x: i64, y: i64) -> Option<i64> {
    if y == 0 {
        return None;
    }
    let r = x.checked_rem(y)?;
    if r != 0 && ((r < 0) != (y < 0)) {
        Some(r + y)
    } else {
        Some(r)
    }
}

/// Python's float `%`: result takes the divisor's sign; an exact zero is +0.0/-0.0
/// matching the divisor.
#[inline]
pub fn float_mod(x: f64, y: f64) -> f64 {
    let r = x % y;
    if r != 0.0 {
        if (y < 0.0) != (r < 0.0) {
            r + y
        } else {
            r
        }
    } else {
        0.0f64.copysign(y)
    }
}

/// Python's float `//` (CPython's float_floor_div algorithm, so results agree exactly).
pub fn float_floordiv(x: f64, y: f64) -> f64 {
    let mut m = x % y;
    let mut div = (x - m) / y;
    if m != 0.0 && ((y < 0.0) != (m < 0.0)) {
        m += y;
        div -= 1.0;
    }
    let _ = m;
    if div != 0.0 {
        let mut fd = div.floor();
        if div - fd > 0.5 {
            fd += 1.0;
        }
        fd
    } else {
        0.0f64.copysign(x / y)
    }
}

#[inline(never)]
pub fn floordiv(_: &mut Heap, a: Value, b: Value) -> R {
    match (int_like(a), int_like(b)) {
        (Some(_), Some(0)) => Err(zero_div()),
        (Some(x), Some(y)) => int_floordiv(x, y).map(Value::Int).ok_or_else(overflow),
        _ => match (num(a), num(b)) {
            (Some(_), Some(y)) if y == 0.0 => Err(zero_div()),
            (Some(x), Some(y)) => Ok(Value::Float(float_floordiv(x, y))),
            _ => Err(unsupported("//", a, b)),
        },
    }
}

#[inline(never)]
pub fn modulo(_: &mut Heap, a: Value, b: Value) -> R {
    match (int_like(a), int_like(b)) {
        (Some(_), Some(0)) => Err("ZeroDivisionError: modulo by zero".into()),
        (Some(x), Some(y)) => int_mod(x, y).map(Value::Int).ok_or_else(overflow),
        _ => match (num(a), num(b)) {
            (Some(_), Some(y)) if y == 0.0 => Err("ZeroDivisionError: modulo by zero".into()),
            (Some(x), Some(y)) => Ok(Value::Float(float_mod(x, y))),
            _ => Err(unsupported("%", a, b)),
        },
    }
}

#[inline(never)]
pub fn pow(_: &mut Heap, a: Value, b: Value) -> R {
    match (int_like(a), int_like(b)) {
        (Some(x), Some(y)) if y >= 0 => {
            if y > u32::MAX as i64 {
                return Err(overflow());
            }
            x.checked_pow(y as u32).map(Value::Int).ok_or_else(overflow)
        }
        (Some(0), Some(_)) => {
            Err("ZeroDivisionError: 0 cannot be raised to a negative power".into())
        }
        _ => match (num(a), num(b)) {
            (Some(x), Some(y)) => Ok(Value::Float(x.powf(y))),
            _ => Err(unsupported("**", a, b)),
        },
    }
}

#[inline(never)]
pub fn neg(a: Value) -> R {
    match a {
        Value::Int(i) => i.checked_neg().map(Value::Int).ok_or_else(overflow),
        Value::Float(f) => Ok(Value::Float(-f)),
        Value::Bool(b) => Ok(Value::Int(-(b as i64))),
        _ => Err(format!("TypeError: bad operand type for unary -: '{}'", a.type_name())),
    }
}

/// Ordering for `<`, `<=`, `>`, `>=`. `Ok(None)` means unordered (NaN).
#[inline(never)]
pub fn compare(a: Value, b: Value, op: &str) -> Result<Option<Ordering>, String> {
    if let (Some(x), Some(y)) = (a.as_str(), b.as_str()) {
        return Ok(Some(x.cmp(y)));
    }
    if let (Some((x, _)), Some((y, _))) = (bytes_of(a), bytes_of(b)) {
        return Ok(Some(x.cmp(y)));
    }
    let same_kind = matches!((list_of(a), list_of(b)), (Some(_), Some(_)))
        || matches!((tuple_of(a), tuple_of(b)), (Some(_), Some(_)));
    if same_kind {
        let (x, y) = (seq_of(a).unwrap(), seq_of(b).unwrap());
        for (p, q) in x.iter().zip(y.iter()) {
            if !equals(*p, *q) {
                return compare(*p, *q, op);
            }
        }
        return Ok(Some(x.len().cmp(&y.len())));
    }
    if let (Some(x), Some(y)) = (set_of(a), set_of(b)) {
        // subset ordering: a <= b means a is a subset of b
        let sub = x.entries.iter().all(|(k, _)| y.contains(&to_key(*k).unwrap()));
        let sup = y.entries.iter().all(|(k, _)| x.contains(&to_key(*k).unwrap()));
        return Ok(match (sub, sup) {
            (true, true) => Some(Ordering::Equal),
            (true, false) => Some(Ordering::Less),
            (false, true) => Some(Ordering::Greater),
            _ => None,
        });
    }
    match (int_like(a), int_like(b)) {
        (Some(x), Some(y)) => Ok(Some(x.cmp(&y))),
        _ => match (num(a), num(b)) {
            (Some(x), Some(y)) => Ok(x.partial_cmp(&y)),
            _ => Err(format!(
                "TypeError: '{}' not supported between instances of '{}' and '{}'",
                op,
                a.type_name(),
                b.type_name()
            )),
        },
    }
}

#[inline(never)]
pub fn equals(a: Value, b: Value) -> bool {
    match (a, b) {
        (Value::None, Value::None) => true,
        (Value::Obj(x), Value::Obj(y)) => {
            if x == y {
                return true;
            }
            match (x.get(), y.get()) {
                (Obj::Str(p), Obj::Str(q)) => p == q,
                (Obj::List(p), Obj::List(q)) => {
                    p.len() == q.len() && p.iter().zip(q.iter()).all(|(m, n)| equals(*m, *n))
                }
                (Obj::Tuple(p), Obj::Tuple(q)) => {
                    p.len() == q.len() && p.iter().zip(q.iter()).all(|(m, n)| equals(*m, *n))
                }
                (Obj::Set(p), Obj::Set(q)) => {
                    p.len() == q.len() && p.entries.iter().all(|(k, _)| q.contains(&to_key(*k).unwrap()))
                }
                (Obj::Dict(p), Obj::Dict(q)) => {
                    p.len() == q.len()
                        && p.entries.iter().all(|(k, v)| match to_key(*k) {
                            Ok(key) => q.get(&key).map_or(false, |w| equals(*v, w)),
                            Err(_) => false,
                        })
                }
                (Obj::Range(p), Obj::Range(q)) => p == q,
                (Obj::Bytes(p) | Obj::ByteArray(p), Obj::Bytes(q) | Obj::ByteArray(q)) => p == q,
                (Obj::Bound(p), Obj::Bound(q)) => equals(p.recv, q.recv) && p.method == q.method,
                _ => false,
            }
        }
        _ => match (int_like(a), int_like(b)) {
            (Some(x), Some(y)) => x == y,
            _ => match (num(a), num(b)) {
                (Some(x), Some(y)) => x == y,
                _ => false,
            },
        },
    }
}

/// `item in container`
#[inline(never)]
pub fn contains(container: Value, item: Value) -> Result<bool, String> {
    if let Value::Obj(g) = container {
        match g.get() {
            Obj::List(l) => return Ok(l.iter().any(|v| equals(*v, item))),
            Obj::Tuple(t) => return Ok(t.iter().any(|v| equals(*v, item))),
            Obj::Set(d) => return Ok(d.contains(&to_key(item)?)),
            Obj::Str(s) => {
                return match item.as_str() {
                    Some(sub) => Ok(s.contains(sub)),
                    None => Err(format!(
                        "TypeError: 'in <str>' requires a string, not '{}'",
                        item.type_name()
                    )),
                }
            }
            Obj::Dict(d) => return Ok(d.contains(&to_key(item)?)),
            Obj::Class(_) if enum_members(container).is_some() => {
                return Ok(enum_members(container).unwrap().iter().any(|m| equals(*m, item)));
            }
            Obj::Bytes(b) | Obj::ByteArray(b) => {
                if let Some(i) = int_like(item) {
                    if !(0..=255).contains(&i) {
                        return Err("ValueError: byte must be in range(0, 256)".into());
                    }
                    return Ok(b.contains(&(i as u8)));
                }
                return match item.as_bytes() {
                    Some(sub) => Ok(sub.is_empty() || b.windows(sub.len()).any(|w| w == sub)),
                    None => Err(format!(
                        "TypeError: a bytes-like object is required, not '{}'",
                        item.type_name()
                    )),
                };
            }
            Obj::Range(r) => {
                return Ok(match int_like(item) {
                    Some(i) => {
                        let inside = if r.step > 0 {
                            i >= r.start && i < r.stop
                        } else {
                            i <= r.start && i > r.stop
                        };
                        inside && (i - r.start) % r.step == 0
                    }
                    None => false,
                })
            }
            _ => {}
        }
    }
    Err(format!(
        "TypeError: argument of type '{}' is not iterable",
        container.type_name()
    ))
}

fn norm_index(i: i64, len: usize) -> Option<usize> {
    let i = if i < 0 { i + len as i64 } else { i };
    if i >= 0 && (i as usize) < len {
        Some(i as usize)
    } else {
        None
    }
}

fn char_at(s: &str, i: i64) -> Option<String> {
    if s.is_ascii() {
        let idx = norm_index(i, s.len())?;
        Some(s[idx..idx + 1].to_string())
    } else {
        let count = s.chars().count();
        let idx = norm_index(i, count)?;
        s.chars().nth(idx).map(|c| c.to_string())
    }
}

fn index_int(idx: Value, what: &str) -> Result<i64, String> {
    int_like(idx).ok_or_else(|| {
        format!("TypeError: {} indices must be integers, not '{}'", what, idx.type_name())
    })
}

#[inline(never)]
pub fn get_index(heap: &mut Heap, obj: Value, idx: Value) -> R {
    if let Value::Obj(g) = obj {
        match g.get() {
            Obj::List(l) => {
                let i = index_int(idx, "list")?;
                return norm_index(i, l.len())
                    .map(|i| l[i])
                    .ok_or_else(|| "IndexError: list index out of range".into());
            }
            Obj::Tuple(t) => {
                let i = index_int(idx, "tuple")?;
                return norm_index(i, t.len())
                    .map(|i| t[i])
                    .ok_or_else(|| "IndexError: tuple index out of range".into());
            }
            Obj::Str(s) => {
                let i = index_int(idx, "string")?;
                return match char_at(s, i) {
                    Some(c) => Ok(heap.string(c)),
                    None => Err("IndexError: string index out of range".into()),
                };
            }
            Obj::Dict(d) => {
                let key = to_key(idx)?;
                return d.get(&key).ok_or_else(|| format!("KeyError: {}", idx.repr()));
            }
            Obj::Bytes(b) | Obj::ByteArray(b) => {
                let i = index_int(idx, "byte")?;
                return norm_index(i, b.len())
                    .map(|i| Value::Int(b[i] as i64))
                    .ok_or_else(|| "IndexError: index out of range".into());
            }
            Obj::Range(r) => {
                let i = index_int(idx, "range")?;
                return norm_index(i, r.len() as usize)
                    .map(|i| Value::Int(r.start + i as i64 * r.step))
                    .ok_or_else(|| "IndexError: range object index out of range".into());
            }
            _ => {}
        }
    }
    Err(format!("TypeError: '{}' object is not subscriptable", obj.type_name()))
}

#[inline(never)]
pub fn set_index(obj: Value, idx: Value, val: Value) -> Result<(), String> {
    if let Value::Obj(g) = obj {
        match g.get_mut() {
            Obj::List(l) => {
                let i = index_int(idx, "list")?;
                return match norm_index(i, l.len()) {
                    Some(i) => {
                        l[i] = val;
                        Ok(())
                    }
                    None => Err("IndexError: list assignment index out of range".into()),
                };
            }
            Obj::Dict(d) => {
                let key = to_key(idx)?;
                d.insert(key, idx, val);
                return Ok(());
            }
            Obj::ByteArray(b) => {
                let i = index_int(idx, "bytearray")?;
                let v = match int_like(val) {
                    Some(v) if (0..=255).contains(&v) => v as u8,
                    Some(_) => return Err("ValueError: byte must be in range(0, 256)".into()),
                    None => {
                        return Err(format!(
                            "TypeError: '{}' object cannot be interpreted as an integer",
                            val.type_name()
                        ))
                    }
                };
                return match norm_index(i, b.len()) {
                    Some(i) => {
                        b[i] = v;
                        Ok(())
                    }
                    None => Err("IndexError: bytearray index out of range".into()),
                };
            }
            _ => {}
        }
    }
    Err(format!(
        "TypeError: '{}' object does not support item assignment",
        obj.type_name()
    ))
}

/// Collects any iterable into a Vec (strings yield 1-char strings, dicts their keys).
#[inline(never)]
pub fn to_vec(heap: &mut Heap, v: Value) -> Result<Vec<Value>, String> {
    if let Value::Obj(g) = v {
        match g.get() {
            Obj::List(l) => return Ok(l.clone()),
            Obj::Tuple(t) => return Ok(t.to_vec()),
            Obj::Set(d) => return Ok(d.entries.iter().map(|(k, _)| *k).collect()),
            Obj::Range(r) => {
                let mut out = Vec::with_capacity(r.len().max(0) as usize);
                let mut i = r.start;
                while (r.step > 0 && i < r.stop) || (r.step < 0 && i > r.stop) {
                    out.push(Value::Int(i));
                    i += r.step;
                }
                return Ok(out);
            }
            Obj::Str(s) => {
                let chars: Vec<char> = s.chars().collect();
                return Ok(chars.into_iter().map(|c| heap.string(c.to_string())).collect());
            }
            Obj::Dict(d) => return Ok(d.entries.iter().map(|(k, _)| *k).collect()),
            Obj::Bytes(b) | Obj::ByteArray(b) => return Ok(b.iter().map(|&x| Value::Int(x as i64)).collect()),
            Obj::Class(_) => {
                if let Some(m) = enum_members(v) {
                    return Ok(m);
                }
            }
            _ => {}
        }
    }
    Err(format!("TypeError: '{}' object is not iterable", v.type_name()))
}

pub fn sort_values(v: &mut [Value]) -> Result<(), String> {
    let mut err: Option<String> = None;
    v.sort_by(|a, b| match compare(*a, *b, "<") {
        Ok(Some(o)) => o,
        Ok(None) => Ordering::Equal,
        Err(e) => {
            if err.is_none() {
                err = Some(e);
            }
            Ordering::Equal
        }
    });
    match err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Python slice semantics: `obj[start:stop:step]` for lists and strings.
#[inline(never)]
pub fn slice(heap: &mut Heap, obj: Value, start: Value, stop: Value, step: Value) -> R {
    let opt = |v: Value, what: &str| -> Result<Option<i64>, String> {
        match v {
            Value::None => Ok(None),
            _ => int_like(v).map(Some).ok_or_else(|| {
                format!("TypeError: slice {} must be an integer or None, not '{}'", what, v.type_name())
            }),
        }
    };
    let step = opt(step, "step")?.unwrap_or(1);
    if step == 0 {
        return Err("ValueError: slice step cannot be zero".into());
    }
    let (start, stop) = (opt(start, "start")?, opt(stop, "stop")?);
    let indices = |len: usize| -> Vec<usize> {
        let len = len as i64;
        let clamp = |i: i64, lo: i64, hi: i64| i.max(lo).min(hi);
        let norm = |i: i64| if i < 0 { i + len } else { i };
        let mut out = Vec::new();
        if step > 0 {
            let a = clamp(start.map_or(0, norm), 0, len);
            let b = clamp(stop.map_or(len, norm), 0, len);
            let mut i = a;
            while i < b {
                out.push(i as usize);
                i += step;
            }
        } else {
            let a = clamp(start.map_or(len - 1, norm), -1, len - 1);
            let b = clamp(stop.map_or(-1, norm), -1, len - 1);
            let b = if stop.is_none() { -1 } else { b };
            let mut i = a;
            while i > b {
                out.push(i as usize);
                i += step;
            }
        }
        out
    };
    if let Value::Obj(g) = obj {
        match g.get() {
            Obj::List(l) => {
                let v = indices(l.len()).into_iter().map(|i| l[i]).collect();
                return Ok(heap.list(v));
            }
            Obj::Tuple(t) => {
                let v = indices(t.len()).into_iter().map(|i| t[i]).collect();
                return Ok(heap.tuple(v));
            }
            Obj::Bytes(b) | Obj::ByteArray(b) => {
                let v: Vec<u8> = indices(b.len()).into_iter().map(|i| b[i]).collect();
                let m = matches!(g.get(), Obj::ByteArray(_));
                return Ok(new_bytes(heap, v, m));
            }
            Obj::Str(s) => {
                if s.is_ascii() && step == 1 {
                    let idx = indices(s.len());
                    let out = match (idx.first(), idx.last()) {
                        (Some(&a), Some(&b)) => s[a..=b].to_string(),
                        _ => String::new(),
                    };
                    return Ok(heap.string(out));
                }
                let chars: Vec<char> = s.chars().collect();
                let out: String = indices(chars.len()).into_iter().map(|i| chars[i]).collect();
                return Ok(heap.string(out));
            }
            _ => {}
        }
    }
    Err(format!("TypeError: '{}' object is not sliceable", obj.type_name()))
}
