//! Built-in functions and methods on str / list / dict.
//! Natives may allocate but never trigger a collection, so values they hold in
//! Rust locals stay valid for the whole call.

use std::io::{self, BufRead, Write};

use crate::ops;
use crate::value::*;
use crate::vm::{BinOp, CmpOp, Vm};

type R = Result<Value, String>;

fn check_args(name: &str, args: &[Value], min: usize, max: usize) -> Result<(), String> {
    if args.len() < min || args.len() > max {
        let expected = if min == max {
            format!("exactly {}", min)
        } else if max == usize::MAX {
            format!("at least {}", min)
        } else {
            format!("{} to {}", min, max)
        };
        return Err(format!(
            "TypeError: {}() takes {} argument{} ({} given)",
            name,
            expected,
            if max == 1 { "" } else { "s" },
            args.len()
        ));
    }
    Ok(())
}

/// Arity check that only builds the method name when it fails.
fn arity(args: &[Value], min: usize, max: usize, name: impl FnOnce() -> String) -> Result<(), String> {
    if args.len() < min || args.len() > max {
        return check_args(&name(), args, min, max);
    }
    Ok(())
}

fn want_int(v: Value, what: &str) -> Result<i64, String> {
    match v {
        Value::Int(i) => Ok(i),
        Value::Bool(b) => Ok(b as i64),
        _ => Err(format!("TypeError: {} must be an int, not '{}'", what, v.type_name())),
    }
}

fn want_str<'a>(v: Value, what: &str) -> Result<&'a str, String> {
    v.as_str()
        .ok_or_else(|| format!("TypeError: {} must be a str, not '{}'", what, v.type_name()))
}

pub fn register(vm: &mut Vm) {
    vm.define_native_kw("print", print, &[sym::SEP, sym::END, sym::FILE, sym::FLUSH]);
    vm.define_native_kw("open", crate::fileio::open_native, &[sym::MODE, sym::ENCODING]);
    vm.define_native("input", input);
    vm.define_native("len", len);
    vm.define_native("range", range);
    vm.define_native("str", to_str);
    vm.define_native("repr", repr);
    vm.define_native("int", to_int);
    vm.define_native("float", to_float);
    vm.define_native("bool", to_bool);
    vm.define_native("list", to_list);
    vm.define_native("tuple", to_tuple);
    vm.define_native("set", to_set);
    vm.define_native("type", type_of);
    vm.define_native("isinstance", isinstance);
    vm.define_native("abs", abs);
    vm.define_native_kw("min", min, &[sym::KEY, sym::DEFAULT]);
    vm.define_native_kw("max", max, &[sym::KEY, sym::DEFAULT]);
    vm.define_native("sum", sum);
    vm.define_native("round", round);
    vm.define_native_kw("sorted", sorted, &[sym::KEY, sym::REVERSE]);
    vm.define_native("chr", chr);
    vm.define_native("ord", ord);
    vm.define_native("clock", clock);
    vm.define_native("format", format_value);
    vm.define_native("$format", format_value);
    vm.define_native("$fmt", fmt_concat);
    vm.define_native("$assert", assert_native);
    vm.define_native("exit", exit_native);
    vm.define_native("map", map_native);
    vm.define_native("filter", filter_native);
    vm.define_native_kw("enumerate", enumerate_native, &[sym::START]);
    vm.define_native("zip", zip_native);
    vm.define_native("any", any_native);
    vm.define_native("all", all_native);
    vm.define_native("callable", callable_native);
    vm.define_native_kw("dict", dict_native, sym::ANY_KW);
    vm.define_native("divmod", divmod_native);
    vm.define_native("pow", pow_native);
    vm.define_native("hex", hex_native);
    vm.define_native("oct", oct_native);
    vm.define_native("bin", bin_native);
    vm.define_native("reversed", reversed_native);
    vm.define_native("getattr", getattr_native);
    vm.define_native("setattr", setattr_native);
    vm.define_native("hasattr", hasattr_native);
    vm.define_native("id", id_native);
    vm.define_native("hash", hash_native);
    vm.define_native("issubclass", issubclass_native);
    vm.define_native("iter", iter_native);
    vm.define_native("next", next_native);
    vm.define_native("$classof", classof_native);
    register_modules(vm);
}

fn iter_native(vm: &mut Vm, args: &[Value]) -> R {
    check_args("iter", args, 1, 1)?;
    vm.make_iter(args[0])
}

fn next_native(vm: &mut Vm, args: &[Value]) -> R {
    check_args("next", args, 1, 2)?;
    let g = match args[0] {
        Value::Obj(g) if matches!(g.get(), Obj::Generator(_)) => g,
        v => {
            // an object with __next__
            let it = vm.make_iter(v)?;
            match (v, it) {
                (Value::Obj(a), Value::Obj(b)) if a != b && matches!(b.get(), Obj::Generator(gen) if matches!(gen.kind, GenKind::Next { .. })) => b,
                _ => return Err(format!("TypeError: '{}' object is not an iterator", v.type_name())),
            }
        }
    };
    match vm.resume_gen(g, Value::None)? {
        Some(v) => Ok(v),
        None => match args.get(1) {
            Some(&d) => Ok(d),
            None => Err("StopIteration".into()),
        },
    }
}

/// The class of an instance (None otherwise); used by `with` to call __exit__.
fn classof_native(_: &mut Vm, args: &[Value]) -> R {
    Ok(match args.first() {
        Some(Value::Obj(g)) => match g.get() {
            Obj::Instance(i) => Value::Obj(i.class),
            _ => Value::None,
        },
        _ => Value::None,
    })
}

fn generator_method(vm: &mut Vm, g: Gc, name: u32, args: &[Value]) -> Option<R> {
    let n = vm.syms.name(name).to_string();
    Some(match n.as_str() {
        "send" => (|| {
            check_args("send", args, 1, 1)?;
            match vm.resume_gen(g, args[0])? {
                Some(v) => Ok(v),
                None => Err("StopIteration".into()),
            }
        })(),
        "__next__" => (|| match vm.resume_gen(g, Value::None)? {
            Some(v) => Ok(v),
            None => Err("StopIteration".into()),
        })(),
        "__iter__" => Ok(Value::Obj(g)),
        "close" => {
            if let Obj::Generator(gen) = g.get_mut() {
                if gen.state == gen_state::RUNNING {
                    return Some(Err("ValueError: generator already executing".into()));
                }
                gen.state = gen_state::DONE;
                if let GenKind::Frame { saved, handlers, .. } = &mut gen.kind {
                    saved.clear();
                    handlers.clear();
                }
            }
            Ok(Value::None)
        }
        _ => return None,
    })
}

fn dict_native(vm: &mut Vm, args: &[Value]) -> R {
    check_args("dict", args, 0, 1)?;
    let mut d = Dict::default();
    if let Some(&a) = args.first() {
        for (k, v) in dict_pairs(vm, a, "dict()")? {
            d.insert(to_key(k)?, k, v);
        }
    }
    for (k, v) in vm.take_all_kw() {
        let key = vm.heap.string(vm.syms.name(k).to_string());
        d.insert(to_key(key)?, key, v);
    }
    Ok(vm.heap.dict(d))
}

fn divmod_native(vm: &mut Vm, args: &[Value]) -> R {
    check_args("divmod", args, 2, 2)?;
    let q = vm.binop(BinOp::FloorDiv, args[0], args[1])?;
    let r = vm.binop(BinOp::Mod, args[0], args[1])?;
    Ok(vm.heap.tuple(vec![q, r]))
}

fn pow_native(vm: &mut Vm, args: &[Value]) -> R {
    check_args("pow", args, 2, 3)?;
    match args.get(2) {
        None | Some(Value::None) => vm.binop(BinOp::Pow, args[0], args[1]),
        Some(&m) => {
            let (b, e, m) = (want_int(args[0], "pow() base")?, want_int(args[1], "pow() exponent")?, want_int(m, "pow() modulus")?);
            if m == 0 {
                return Err("ValueError: pow() 3rd argument cannot be 0".into());
            }
            if e < 0 {
                return Err("ValueError: pow() negative exponent with a modulus isn't supported".into());
            }
            let (mut result, mut base, mut e, m) = (1i128, (b as i128).rem_euclid(m as i128), e, m as i128);
            while e > 0 {
                if e & 1 == 1 {
                    result = result * base % m;
                }
                base = base * base % m;
                e >>= 1;
            }
            // Python: the result has the sign of the modulus
            let r = result.rem_euclid(m.abs());
            Ok(Value::Int(if m < 0 && r != 0 { (r + m) as i64 } else { r as i64 }))
        }
    }
}

fn radix_text(args: &[Value], name: &str, prefix: &str, radix: u32) -> Result<String, String> {
    check_args(name, args, 1, 1)?;
    let n = want_int(args[0], &format!("{}() argument", name))?;
    let mag = n.unsigned_abs();
    let digits = match radix {
        16 => format!("{:x}", mag),
        8 => format!("{:o}", mag),
        _ => format!("{:b}", mag),
    };
    Ok(format!("{}{}{}", if n < 0 { "-" } else { "" }, prefix, digits))
}

fn hex_native(vm: &mut Vm, args: &[Value]) -> R {
    let s = radix_text(args, "hex", "0x", 16)?;
    Ok(vm.heap.string(s))
}

fn oct_native(vm: &mut Vm, args: &[Value]) -> R {
    let s = radix_text(args, "oct", "0o", 8)?;
    Ok(vm.heap.string(s))
}

fn bin_native(vm: &mut Vm, args: &[Value]) -> R {
    let s = radix_text(args, "bin", "0b", 2)?;
    Ok(vm.heap.string(s))
}

fn reversed_native(vm: &mut Vm, args: &[Value]) -> R {
    check_args("reversed", args, 1, 1)?;
    let mut items = vm.to_vec(args[0])?;
    items.reverse();
    Ok(vm.heap.list(items))
}

fn attr_name(vm: &mut Vm, v: Value, f: &str) -> Result<u32, String> {
    let s = v
        .as_str()
        .ok_or_else(|| format!("TypeError: {}(): attribute name must be a string, not '{}'", f, v.type_name()))?;
    Ok(vm.syms.intern(s))
}

fn getattr_native(vm: &mut Vm, args: &[Value]) -> R {
    check_args("getattr", args, 2, 3)?;
    let name = attr_name(vm, args[1], "getattr")?;
    match vm.get_attr(args[0], name) {
        Err(e) if e.starts_with("AttributeError") && args.len() == 3 => Ok(args[2]),
        r => r,
    }
}

fn hasattr_native(vm: &mut Vm, args: &[Value]) -> R {
    check_args("hasattr", args, 2, 2)?;
    let name = attr_name(vm, args[1], "hasattr")?;
    match vm.get_attr(args[0], name) {
        Ok(_) => Ok(Value::Bool(true)),
        Err(e) if e.starts_with("AttributeError") => Ok(Value::Bool(false)),
        Err(e) => Err(e),
    }
}

fn setattr_native(vm: &mut Vm, args: &[Value]) -> R {
    check_args("setattr", args, 3, 3)?;
    let name = attr_name(vm, args[1], "setattr")?;
    vm.set_attr(args[0], name, args[2])?;
    Ok(Value::None)
}

fn id_native(_: &mut Vm, args: &[Value]) -> R {
    check_args("id", args, 1, 1)?;
    Ok(Value::Int(match args[0] {
        Value::Obj(g) => g.addr() as i64,
        Value::Int(i) => i,
        Value::Float(f) => f.to_bits() as i64,
        Value::Bool(b) => b as i64,
        _ => 0,
    }))
}

fn hash_native(_: &mut Vm, args: &[Value]) -> R {
    use std::hash::{Hash, Hasher};
    check_args("hash", args, 1, 1)?;
    let key = to_key(args[0])?;
    if let Value::Int(i) = args[0] {
        return Ok(Value::Int(i));
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut h);
    Ok(Value::Int(h.finish() as i64))
}

fn issubclass_native(vm: &mut Vm, args: &[Value]) -> R {
    check_args("issubclass", args, 2, 2)?;
    let cls = match args[0] {
        Value::Obj(g) if matches!(g.get(), Obj::Class(_)) => g,
        v => return Err(format!("TypeError: issubclass() arg 1 must be a class, not '{}'", v.type_name())),
    };
    let targets: Vec<Value> = match args[1] {
        Value::Obj(g) if matches!(g.get(), Obj::Tuple(_)) => vm.to_vec(args[1])?,
        v => vec![v],
    };
    for t in targets {
        let t = match t {
            Value::Obj(g) if matches!(g.get(), Obj::Class(_)) => g,
            v => return Err(format!("TypeError: issubclass() arg 2 must be a class, not '{}'", v.type_name())),
        };
        let mut c = Some(cls);
        while let Some(k) = c {
            if k == t {
                return Ok(Value::Bool(true));
            }
            c = k.as_class().parent;
        }
    }
    Ok(Value::Bool(false))
}

/// Positions in `items` equal to `x` (uses `__eq__` when an instance is involved).
fn positions_equal(vm: &mut Vm, items: &[Value], x: Value) -> Result<Vec<usize>, String> {
    let mut out = Vec::new();
    for (i, v) in items.iter().enumerate() {
        if vm.compare(CmpOp::Eq, *v, x)? {
            out.push(i);
        }
    }
    Ok(out)
}

/// Like `vm.to_vec`, but keeps the items alive (in a list on the VM stack) while
/// Faston callbacks run. Restore with `vm.temp_reset(mark)`.
fn rooted_items(vm: &mut Vm, v: Value) -> Result<(usize, Vec<Value>), String> {
    let mark = vm.temp_mark();
    let items = vm.to_vec(v)?;
    let holder = vm.heap.list(items.clone());
    vm.push_temp(holder)?;
    Ok((mark, items))
}

fn assert_native(vm: &mut Vm, args: &[Value]) -> R {
    if vm.truthy(args[0])? {
        return Ok(Value::None);
    }
    let _ = vm.out.flush();
    Err(match args[1] {
        Value::None => "AssertionError".to_string(),
        msg => format!("AssertionError: {}", msg.to_str()),
    })
}

fn exit_native(vm: &mut Vm, args: &[Value]) -> R {
    check_args("exit", args, 0, 1)?;
    vm.flush_all();
    let code = match args.first() {
        Some(Value::Int(c)) => *c as i32,
        Some(v) if v.truthy() => {
            eprintln!("{}", v.to_str());
            1
        }
        _ => 0,
    };
    std::process::exit(code)
}

// ---------------------------------------------------------------- modules

fn register_modules(vm: &mut Vm) {
    // math
    let mut m: Vec<(&str, Value)> = vec![
        ("pi", Value::Float(std::f64::consts::PI)),
        ("e", Value::Float(std::f64::consts::E)),
        ("tau", Value::Float(std::f64::consts::TAU)),
        ("inf", Value::Float(f64::INFINITY)),
        ("nan", Value::Float(f64::NAN)),
    ];
    let fns: [(&'static str, NativeFn); 22] = [
        ("sqrt", math_sqrt),
        ("sin", math_sin),
        ("cos", math_cos),
        ("tan", math_tan),
        ("asin", math_asin),
        ("acos", math_acos),
        ("atan", math_atan),
        ("atan2", math_atan2),
        ("exp", math_exp),
        ("log", math_log),
        ("log10", math_log10),
        ("log2", math_log2),
        ("pow", math_pow),
        ("floor", math_floor),
        ("ceil", math_ceil),
        ("trunc", math_trunc),
        ("hypot", math_hypot),
        ("degrees", math_degrees),
        ("radians", math_radians),
        ("isnan", math_isnan),
        ("isinf", math_isinf),
        ("fabs", math_fabs),
    ];
    for (name, f) in fns {
        let v = vm.native(name, f);
        m.push((name, v));
    }
    vm.define_module("math", m);

    // random
    let fns: [(&'static str, NativeFn); 6] = [
        ("random", rand_random),
        ("randint", rand_randint),
        ("uniform", rand_uniform),
        ("choice", rand_choice),
        ("shuffle", rand_shuffle),
        ("seed", rand_seed),
    ];
    let m = fns.iter().map(|&(name, f)| (name, vm.native(name, f))).collect();
    vm.define_module("random", m);

    // time
    let fns: [(&'static str, NativeFn); 3] = [
        ("time", time_time),
        ("perf_counter", time_perf_counter),
        ("sleep", time_sleep),
    ];
    let m = fns.iter().map(|&(name, f)| (name, vm.native(name, f))).collect();
    vm.define_module("time", m);

    crate::osmod::register(vm);
    crate::json::register(vm);
}

fn want_num(v: Value, what: &str) -> Result<f64, String> {
    match v {
        Value::Int(i) => Ok(i as f64),
        Value::Float(f) => Ok(f),
        Value::Bool(b) => Ok(b as i64 as f64),
        _ => Err(format!("TypeError: {} must be a number, not '{}'", what, v.type_name())),
    }
}

macro_rules! math1 {
    ($name:ident, $label:expr, |$x:ident| $body:expr) => {
        fn $name(_: &mut Vm, args: &[Value]) -> R {
            check_args($label, args, 1, 1)?;
            let $x = want_num(args[0], concat!($label, "() argument"))?;
            $body
        }
    };
}

fn domain(name: &str) -> String {
    format!("ValueError: math domain error in {}()", name)
}

math1!(math_sqrt, "sqrt", |x| if x < 0.0 { Err(domain("sqrt")) } else { Ok(Value::Float(x.sqrt())) });
math1!(math_sin, "sin", |x| Ok(Value::Float(x.sin())));
math1!(math_cos, "cos", |x| Ok(Value::Float(x.cos())));
math1!(math_tan, "tan", |x| Ok(Value::Float(x.tan())));
math1!(math_asin, "asin", |x| if !(-1.0..=1.0).contains(&x) { Err(domain("asin")) } else { Ok(Value::Float(x.asin())) });
math1!(math_acos, "acos", |x| if !(-1.0..=1.0).contains(&x) { Err(domain("acos")) } else { Ok(Value::Float(x.acos())) });
math1!(math_atan, "atan", |x| Ok(Value::Float(x.atan())));
math1!(math_exp, "exp", |x| Ok(Value::Float(x.exp())));
math1!(math_log10, "log10", |x| if x <= 0.0 { Err(domain("log10")) } else { Ok(Value::Float(x.log10())) });
math1!(math_log2, "log2", |x| if x <= 0.0 { Err(domain("log2")) } else { Ok(Value::Float(x.log2())) });
math1!(math_degrees, "degrees", |x| Ok(Value::Float(x.to_degrees())));
math1!(math_radians, "radians", |x| Ok(Value::Float(x.to_radians())));
math1!(math_isnan, "isnan", |x| Ok(Value::Bool(x.is_nan())));
math1!(math_isinf, "isinf", |x| Ok(Value::Bool(x.is_infinite())));
math1!(math_fabs, "fabs", |x| Ok(Value::Float(x.abs())));

fn float_to_int(x: f64, name: &str) -> R {
    if !x.is_finite() || x.abs() >= 9.2e18 {
        return Err(format!("OverflowError: cannot convert {} to int in {}()", fmt_float(x), name));
    }
    Ok(Value::Int(x as i64))
}

fn math_floor(_: &mut Vm, args: &[Value]) -> R {
    check_args("floor", args, 1, 1)?;
    if let Value::Int(i) = args[0] {
        return Ok(Value::Int(i));
    }
    float_to_int(want_num(args[0], "floor() argument")?.floor(), "floor")
}

fn math_ceil(_: &mut Vm, args: &[Value]) -> R {
    check_args("ceil", args, 1, 1)?;
    if let Value::Int(i) = args[0] {
        return Ok(Value::Int(i));
    }
    float_to_int(want_num(args[0], "ceil() argument")?.ceil(), "ceil")
}

fn math_trunc(_: &mut Vm, args: &[Value]) -> R {
    check_args("trunc", args, 1, 1)?;
    if let Value::Int(i) = args[0] {
        return Ok(Value::Int(i));
    }
    float_to_int(want_num(args[0], "trunc() argument")?.trunc(), "trunc")
}

fn math_atan2(_: &mut Vm, args: &[Value]) -> R {
    check_args("atan2", args, 2, 2)?;
    let y = want_num(args[0], "atan2() y")?;
    let x = want_num(args[1], "atan2() x")?;
    Ok(Value::Float(y.atan2(x)))
}

fn math_log(_: &mut Vm, args: &[Value]) -> R {
    check_args("log", args, 1, 2)?;
    let x = want_num(args[0], "log() argument")?;
    if x <= 0.0 {
        return Err(domain("log"));
    }
    match args.get(1) {
        None => Ok(Value::Float(x.ln())),
        Some(b) => {
            let b = want_num(*b, "log() base")?;
            if b <= 0.0 || b == 1.0 {
                return Err(domain("log"));
            }
            Ok(Value::Float(x.ln() / b.ln()))
        }
    }
}

fn math_pow(_: &mut Vm, args: &[Value]) -> R {
    check_args("pow", args, 2, 2)?;
    let x = want_num(args[0], "pow() base")?;
    let y = want_num(args[1], "pow() exponent")?;
    Ok(Value::Float(x.powf(y)))
}

fn math_hypot(_: &mut Vm, args: &[Value]) -> R {
    let mut sum = 0.0;
    for a in args {
        let v = want_num(*a, "hypot() argument")?;
        sum += v * v;
    }
    Ok(Value::Float(sum.sqrt()))
}

/// xorshift64* — fast, decent quality, not for cryptography.
fn next_u64(vm: &mut Vm) -> u64 {
    let mut x = vm.rng;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    vm.rng = x;
    x.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

fn next_f64(vm: &mut Vm) -> f64 {
    (next_u64(vm) >> 11) as f64 / (1u64 << 53) as f64
}

/// Uniform integer in [0, n) without modulo bias.
fn below(vm: &mut Vm, n: u64) -> u64 {
    let zone = u64::MAX - (u64::MAX % n);
    loop {
        let r = next_u64(vm);
        if r < zone {
            return r % n;
        }
    }
}

fn rand_random(vm: &mut Vm, args: &[Value]) -> R {
    check_args("random", args, 0, 0)?;
    Ok(Value::Float(next_f64(vm)))
}

fn rand_randint(vm: &mut Vm, args: &[Value]) -> R {
    check_args("randint", args, 2, 2)?;
    let a = want_int(args[0], "randint() a")?;
    let b = want_int(args[1], "randint() b")?;
    if b < a {
        return Err("ValueError: randint() needs a <= b".into());
    }
    let span = (b as i128 - a as i128 + 1) as u128;
    if span > u64::MAX as u128 {
        return Ok(Value::Int(next_u64(vm) as i64));
    }
    Ok(Value::Int((a as i128 + below(vm, span as u64) as i128) as i64))
}

fn rand_uniform(vm: &mut Vm, args: &[Value]) -> R {
    check_args("uniform", args, 2, 2)?;
    let a = want_num(args[0], "uniform() a")?;
    let b = want_num(args[1], "uniform() b")?;
    Ok(Value::Float(a + (b - a) * next_f64(vm)))
}

fn rand_choice(vm: &mut Vm, args: &[Value]) -> R {
    check_args("choice", args, 1, 1)?;
    let items = vm.to_vec(args[0])?;
    if items.is_empty() {
        return Err("IndexError: cannot choose from an empty sequence".into());
    }
    let i = below(vm, items.len() as u64) as usize;
    Ok(items[i])
}

fn rand_shuffle(vm: &mut Vm, args: &[Value]) -> R {
    check_args("shuffle", args, 1, 1)?;
    let g = match args[0] {
        Value::Obj(g) if matches!(g.get(), Obj::List(_)) => g,
        v => return Err(format!("TypeError: shuffle() needs a list, not '{}'", v.type_name())),
    };
    let n = list_mut(g).len();
    for i in (1..n).rev() {
        let j = below(vm, i as u64 + 1) as usize;
        list_mut(g).swap(i, j);
    }
    Ok(Value::None)
}

fn rand_seed(vm: &mut Vm, args: &[Value]) -> R {
    check_args("seed", args, 1, 1)?;
    let s = want_int(args[0], "seed() argument")? as u64;
    // splitmix the seed so small seeds still give well-mixed state
    let mut z = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    vm.rng = (z ^ (z >> 31)) | 1;
    Ok(Value::None)
}

fn time_time(_: &mut Vm, args: &[Value]) -> R {
    check_args("time", args, 0, 0)?;
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    Ok(Value::Float(t))
}

fn time_perf_counter(vm: &mut Vm, args: &[Value]) -> R {
    check_args("perf_counter", args, 0, 0)?;
    Ok(Value::Float(vm.start.elapsed().as_secs_f64()))
}

fn time_sleep(vm: &mut Vm, args: &[Value]) -> R {
    check_args("sleep", args, 1, 1)?;
    let secs = want_num(args[0], "sleep() argument")?;
    if secs < 0.0 {
        return Err("ValueError: sleep length must be non-negative".into());
    }
    let _ = vm.out.flush();
    std::thread::sleep(std::time::Duration::from_secs_f64(secs));
    Ok(Value::None)
}

// ---------------------------------------------------------------- functions

fn kw_text(vm: &mut Vm, name: u32, default: &str) -> Result<String, String> {
    match vm.take_kw(name) {
        None | Some(Value::None) => Ok(default.to_string()),
        Some(v) => match v.as_str() {
            Some(s) => Ok(s.to_string()),
            None => Err(format!(
                "TypeError: print() {} must be None or a string, not '{}'",
                if name == sym::SEP { "sep" } else { "end" },
                v.type_name()
            )),
        },
    }
}

fn print(vm: &mut Vm, args: &[Value]) -> R {
    let sep = kw_text(vm, sym::SEP, " ")?;
    let end = kw_text(vm, sym::END, "\n")?;
    let flush = match vm.take_kw(sym::FLUSH) {
        Some(v) => vm.truthy(v)?,
        None => false,
    };
    match vm.take_kw(sym::FILE) {
        None | Some(Value::None) => {}
        Some(file) => {
            let mut text = String::new();
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    text.push_str(&sep);
                }
                text.push_str(&vm.str_of(*a)?);
            }
            text.push_str(&end);
            crate::osmod::print_to(vm, file, &text, flush)?;
            return Ok(Value::None);
        }
    }
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            let _ = vm.out.write_all(sep.as_bytes());
        }
        let _ = match (a.as_str(), a) {
            (Some(s), _) => vm.out.write_all(s.as_bytes()),
            (None, Value::Int(i)) => {
                let mut tmp = String::new();
                push_int(&mut tmp, *i);
                vm.out.write_all(tmp.as_bytes())
            }
            (None, _) => {
                let s = vm.str_of(*a)?;
                vm.out.write_all(s.as_bytes())
            }
        };
    }
    let _ = vm.out.write_all(end.as_bytes());
    if vm.tty || flush {
        let _ = vm.out.flush();
    }
    Ok(Value::None)
}

fn input(vm: &mut Vm, args: &[Value]) -> R {
    check_args("input", args, 0, 1)?;
    if let Some(p) = args.first() {
        let _ = vm.out.write_all(p.to_str().as_bytes());
    }
    let _ = vm.out.flush();
    let mut line = String::new();
    let n = io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| format!("IOError: {}", e))?;
    if n == 0 {
        return Err("EOFError: EOF when reading a line".into());
    }
    while line.ends_with('\n') || line.ends_with('\r') {
        line.pop();
    }
    Ok(vm.heap.string(line))
}

fn len(vm: &mut Vm, args: &[Value]) -> R {
    check_args("len", args, 1, 1)?;
    let n = match args[0] {
        Value::Obj(g) => match g.get() {
            Obj::Str(s) => {
                if s.is_ascii() {
                    s.len()
                } else {
                    s.chars().count()
                }
            }
            Obj::List(l) => l.len(),
            Obj::Tuple(t) => t.len(),
            Obj::Dict(d) | Obj::Set(d) => d.len(),
            Obj::Range(r) => r.len() as usize,
            Obj::Instance(_) => match vm.len_of(args[0])? {
                Some(n) => n as usize,
                None => usize::MAX,
            },
            _ => usize::MAX,
        },
        _ => usize::MAX,
    };
    if n == usize::MAX {
        return Err(format!(
            "TypeError: object of type '{}' has no len()",
            args[0].type_name()
        ));
    }
    Ok(Value::Int(n as i64))
}

fn range(vm: &mut Vm, args: &[Value]) -> R {
    check_args("range", args, 1, 3)?;
    let w = |v: Value| want_int(v, "range() argument");
    let (start, stop, step) = match args.len() {
        1 => (0, w(args[0])?, 1),
        2 => (w(args[0])?, w(args[1])?, 1),
        _ => (w(args[0])?, w(args[1])?, w(args[2])?),
    };
    if step == 0 {
        return Err("ValueError: range() step must not be zero".into());
    }
    Ok(Value::Obj(vm.heap.alloc(Obj::Range(Range { start, stop, step }))))
}

fn to_str(vm: &mut Vm, args: &[Value]) -> R {
    check_args("str", args, 0, 1)?;
    Ok(match args.first() {
        None => vm.heap.string(""),
        Some(v) if v.as_str().is_some() => *v,
        Some(v) => {
            let s = vm.str_of(*v)?;
            vm.heap.string(s)
        }
    })
}

fn repr(vm: &mut Vm, args: &[Value]) -> R {
    check_args("repr", args, 1, 1)?;
    let s = vm.repr_of(args[0])?;
    Ok(vm.heap.string(s))
}

/// `int("ff", 16)`, `int("0x1f", 0)`: Python's rules for an explicit base.
fn parse_int_base(text: &str, base: i64) -> Result<i64, String> {
    let bad = || format!("ValueError: invalid literal for int() with base {}: '{}'", base, text);
    let t = text.trim().replace('_', "");
    let (neg, mut digits) = match t.strip_prefix('-') {
        Some(r) => (true, r.to_string()),
        None => (false, t.strip_prefix('+').unwrap_or(&t).to_string()),
    };
    let lower = digits.to_ascii_lowercase();
    let prefixed = |p: &str| lower.starts_with(p);
    let mut base = base;
    if base == 0 {
        base = if prefixed("0x") { 16 } else if prefixed("0o") { 8 } else if prefixed("0b") { 2 } else { 10 };
    }
    if (base == 16 && prefixed("0x")) || (base == 8 && prefixed("0o")) || (base == 2 && prefixed("0b")) {
        digits = digits[2..].to_string();
    }
    if !(2..=36).contains(&base) {
        return Err("ValueError: int() base must be >= 2 and <= 36, or 0".into());
    }
    if digits.is_empty() {
        return Err(bad());
    }
    let mag = u64::from_str_radix(&digits, base as u32).map_err(|_| bad())?;
    if neg {
        if mag > i64::MAX as u64 + 1 {
            return Err(crate::ops::overflow());
        }
        Ok((mag as i64).wrapping_neg())
    } else {
        i64::try_from(mag).map_err(|_| crate::ops::overflow())
    }
}

fn to_int(_: &mut Vm, args: &[Value]) -> R {
    check_args("int", args, 0, 2)?;
    let v = match args.first() {
        None => return Ok(Value::Int(0)),
        Some(v) => *v,
    };
    if let Some(&b) = args.get(1) {
        let base = want_int(b, "int() base")?;
        let text = v
            .as_str()
            .ok_or("TypeError: int() can't convert non-string with explicit base")?;
        return parse_int_base(text, base).map(Value::Int);
    }
    match v {
        Value::Int(i) => Ok(Value::Int(i)),
        Value::Bool(b) => Ok(Value::Int(b as i64)),
        Value::Float(f) => {
            if !f.is_finite() || f.abs() >= 9.2e18 {
                return Err(format!("OverflowError: cannot convert float {} to int", fmt_float(f)));
            }
            Ok(Value::Int(f.trunc() as i64))
        }
        _ => match v.as_str() {
            Some(s) => s
                .trim()
                .replace('_', "")
                .parse::<i64>()
                .map(Value::Int)
                .map_err(|_| format!("ValueError: invalid literal for int(): {}", v.repr())),
            None => Err(format!("TypeError: int() can't convert '{}'", v.type_name())),
        },
    }
}

fn to_float(_: &mut Vm, args: &[Value]) -> R {
    check_args("float", args, 0, 1)?;
    let v = match args.first() {
        None => return Ok(Value::Float(0.0)),
        Some(v) => *v,
    };
    match v {
        Value::Int(i) => Ok(Value::Float(i as f64)),
        Value::Bool(b) => Ok(Value::Float(b as i64 as f64)),
        Value::Float(f) => Ok(Value::Float(f)),
        _ => match v.as_str() {
            Some(s) => {
                let t = s.trim().to_lowercase();
                let parsed = match t.as_str() {
                    "inf" | "+inf" | "infinity" => Ok(f64::INFINITY),
                    "-inf" | "-infinity" => Ok(f64::NEG_INFINITY),
                    "nan" => Ok(f64::NAN),
                    _ => t.parse::<f64>(),
                };
                parsed.map(Value::Float).map_err(|_| {
                    format!("ValueError: could not convert string to float: {}", v.repr())
                })
            }
            None => Err(format!("TypeError: float() can't convert '{}'", v.type_name())),
        },
    }
}

fn to_bool(vm: &mut Vm, args: &[Value]) -> R {
    check_args("bool", args, 0, 1)?;
    match args.first() {
        None => Ok(Value::Bool(false)),
        Some(v) => Ok(Value::Bool(vm.truthy(*v)?)),
    }
}

fn to_list(vm: &mut Vm, args: &[Value]) -> R {
    check_args("list", args, 0, 1)?;
    let items = match args.first() {
        None => Vec::new(),
        Some(v) => vm.to_vec(*v)?,
    };
    Ok(vm.heap.list(items))
}

fn to_tuple(vm: &mut Vm, args: &[Value]) -> R {
    check_args("tuple", args, 0, 1)?;
    let items = match args.first() {
        None => Vec::new(),
        Some(v) => vm.to_vec(*v)?,
    };
    Ok(vm.heap.tuple(items))
}

fn to_set(vm: &mut Vm, args: &[Value]) -> R {
    check_args("set", args, 0, 1)?;
    let items = match args.first() {
        None => Vec::new(),
        Some(v) => vm.to_vec(*v)?,
    };
    ops::make_set(&mut vm.heap, items)
}

fn type_of(vm: &mut Vm, args: &[Value]) -> R {
    check_args("type", args, 1, 1)?;
    let n = args[0].type_name();
    Ok(vm.heap.string(n))
}

fn isinstance(vm: &mut Vm, args: &[Value]) -> R {
    check_args("isinstance", args, 2, 2)?;
    if let Some(name) = args[1].as_str() {
        return Ok(Value::Bool(args[0].type_name() == name));
    }
    if let Value::Obj(g) = args[1] {
        if let Obj::Tuple(targets) = g.get() {
            for t in targets.iter() {
                if isinstance(vm, &[args[0], *t])?.truthy() {
                    return Ok(Value::Bool(true));
                }
            }
            return Ok(Value::Bool(false));
        }
    }
    let target = match args[1] {
        Value::Obj(g) if matches!(g.get(), Obj::Class(_)) => g,
        other => {
            return Err(format!(
                "TypeError: isinstance() arg 2 must be a class, type name, or tuple of them, not '{}'",
                other.type_name()
            ))
        }
    };
    let mut cls = match args[0] {
        Value::Obj(g) => match g.get() {
            Obj::Instance(i) => Some(i.class),
            _ => None,
        },
        _ => None,
    };
    while let Some(c) = cls {
        if c == target {
            return Ok(Value::Bool(true));
        }
        cls = c.as_class().parent;
    }
    Ok(Value::Bool(false))
}

fn abs(_: &mut Vm, args: &[Value]) -> R {
    check_args("abs", args, 1, 1)?;
    match args[0] {
        Value::Int(i) => i.checked_abs().map(Value::Int).ok_or_else(ops::overflow),
        Value::Float(f) => Ok(Value::Float(f.abs())),
        Value::Bool(b) => Ok(Value::Int(b as i64)),
        v => Err(format!("TypeError: bad operand type for abs(): '{}'", v.type_name())),
    }
}

/// `min` / `max`, with optional `key=` and `default=`.
fn extreme(vm: &mut Vm, name: &str, args: &[Value], want: std::cmp::Ordering) -> R {
    check_args(name, args, 1, usize::MAX)?;
    let key = vm.take_kw(sym::KEY).unwrap_or(Value::None);
    let default = vm.take_kw(sym::DEFAULT);
    let (mark, items) = if args.len() == 1 {
        rooted_items(vm, args[0])?
    } else {
        (vm.temp_mark(), args.to_vec())
    };
    let r = (|| -> R {
        if items.is_empty() {
            return default.ok_or_else(|| format!("ValueError: {}() arg is an empty sequence", name));
        }
        let op = if want == std::cmp::Ordering::Less { CmpOp::Lt } else { CmpOp::Gt };
        let keyed = !matches!(key, Value::None);
        let mut best = items[0];
        let mut best_key = if keyed { vm.call(key, &[best])? } else { best };
        let slot = vm.temp_mark();
        vm.push_temp(best_key)?; // the current best key must survive later key() calls
        for &v in &items[1..] {
            let k = if keyed { vm.call(key, &[v])? } else { v };
            if vm.compare(op, k, best_key)? {
                best = v;
                best_key = k;
                vm.set_temp(slot, k);
            }
        }
        Ok(best)
    })();
    vm.temp_reset(mark);
    r
}

fn min(vm: &mut Vm, args: &[Value]) -> R {
    extreme(vm, "min", args, std::cmp::Ordering::Less)
}

fn max(vm: &mut Vm, args: &[Value]) -> R {
    extreme(vm, "max", args, std::cmp::Ordering::Greater)
}

fn sum(vm: &mut Vm, args: &[Value]) -> R {
    check_args("sum", args, 1, 2)?;
    let total = args.get(1).copied().unwrap_or(Value::Int(0));
    let (mark, items) = rooted_items(vm, args[0])?;
    let slot = vm.temp_mark();
    vm.push_temp(total)?;
    let mut acc = total;
    for v in items {
        acc = match vm.binop(BinOp::Add, acc, v) {
            Ok(r) => r,
            Err(e) => {
                vm.temp_reset(mark);
                return Err(e);
            }
        };
        vm.set_temp(slot, acc); // keep the running total visible to the GC
    }
    vm.temp_reset(mark);
    Ok(acc)
}

fn round(_: &mut Vm, args: &[Value]) -> R {
    check_args("round", args, 1, 2)?;
    let x = match args[0] {
        Value::Int(i) if args.len() == 1 => return Ok(Value::Int(i)),
        Value::Int(i) => i as f64,
        Value::Float(f) => f,
        v => return Err(format!("TypeError: round() needs a number, not '{}'", v.type_name())),
    };
    // Python rounds half to even.
    fn round_half_even(x: f64) -> f64 {
        if (x - x.trunc()).abs() == 0.5 {
            2.0 * (x / 2.0).round()
        } else {
            x.round()
        }
    }
    match args.get(1) {
        None => {
            if !x.is_finite() {
                return Err("OverflowError: cannot round a non-finite float to int".into());
            }
            Ok(Value::Int(round_half_even(x) as i64))
        }
        Some(n) => {
            let n = want_int(*n, "round() ndigits")?;
            if !x.is_finite() {
                return Ok(Value::Float(x));
            }
            if n >= 0 {
                // Rust's float formatting is correctly rounded on the exact binary
                // value, which is what Python does (round(2.675, 2) == 2.67).
                let s = format!("{:.*}", n.min(300) as usize, x);
                return Ok(Value::Float(s.parse().unwrap_or(x)));
            }
            let p = 10f64.powi((-n).min(308) as i32);
            Ok(Value::Float(round_half_even(x / p) * p))
        }
    }
}

/// `sorted(xs)` / `sorted(xs, key)`; the sort is stable.
/// `key=` and `reverse=` keyword arguments for sorting (key may also be positional).
fn sort_options(vm: &mut Vm, positional_key: Option<Value>) -> Result<(Value, bool), String> {
    let key = vm.take_kw(sym::KEY).or(positional_key).unwrap_or(Value::None);
    let reverse = match vm.take_kw(sym::REVERSE) {
        Some(v) => vm.truthy(v)?,
        None => false,
    };
    Ok((key, reverse))
}

fn sorted(vm: &mut Vm, args: &[Value]) -> R {
    check_args("sorted", args, 1, 2)?;
    let (key, reverse) = sort_options(vm, args.get(1).copied())?;
    let v = vm.to_vec(args[0])?;
    let list = vm.heap.list(v);
    let g = match list {
        Value::Obj(g) => g,
        _ => unreachable!(),
    };
    let mark = vm.temp_mark();
    vm.push_temp(list)?;
    let r = sort_by_key(vm, g, key, reverse);
    vm.temp_reset(mark);
    r?;
    Ok(list)
}

/// Stable sort of list `g` by `key(item)`. Keys and a snapshot of the items are kept
/// on the VM stack (in heap lists) so the GC sees them while `key` runs.
fn sort_by_key(vm: &mut Vm, g: Gc, key: Value, reverse: bool) -> Result<(), String> {
    let mark = vm.temp_mark();
    let mut items = list_mut(g).clone();
    if reverse {
        // reverse + stable sort + reverse = stable descending sort (like Python)
        items.reverse();
    }
    let snapshot = vm.heap.list(items);
    vm.push_temp(snapshot)?;
    let keys_val = vm.heap.list(Vec::new());
    vm.push_temp(keys_val)?;
    let (snap, keys) = match (snapshot, keys_val) {
        (Value::Obj(a), Value::Obj(b)) => (a, b),
        _ => unreachable!(),
    };
    let n = list_mut(snap).len();
    if matches!(key, Value::None) {
        *list_mut(keys) = list_mut(snap).clone();
    } else {
        for i in 0..n {
            let item = list_mut(snap)[i];
            let k = match vm.call(key, &[item]) {
                Ok(k) => k,
                Err(e) => {
                    vm.temp_reset(mark);
                    return Err(e);
                }
            };
            list_mut(keys).push(k);
        }
    }
    let ks = list_mut(keys).clone();
    let order = stable_order(vm, &ks);
    let items = list_mut(snap).clone();
    vm.temp_reset(mark);
    let mut sorted: Vec<Value> = order?.into_iter().map(|i| items[i]).collect();
    if reverse {
        sorted.reverse();
    }
    *list_mut(g) = sorted;
    Ok(())
}

/// Stable sorting permutation of `keys` (which the caller keeps rooted). Uses the
/// fast built-in comparison unless some key is an instance (then `__lt__` is called).
fn stable_order(vm: &mut Vm, keys: &[Value]) -> Result<Vec<usize>, String> {
    let n = keys.len();
    let mut idx: Vec<usize> = (0..n).collect();
    let has_instance = keys
        .iter()
        .any(|k| matches!(k, Value::Obj(g) if matches!(g.get(), Obj::Instance(_))));
    if !has_instance {
        let mut err = None;
        idx.sort_by(|&a, &b| match ops::compare(keys[a], keys[b], "<") {
            Ok(Some(o)) => o,
            Ok(None) => std::cmp::Ordering::Equal,
            Err(e) => {
                err.get_or_insert(e);
                std::cmp::Ordering::Equal
            }
        });
        return match err {
            Some(e) => Err(e),
            None => Ok(idx),
        };
    }
    // Bottom-up merge sort with a fallible comparison (it may run Faston code).
    let mut buf = idx.clone();
    let mut width = 1;
    while width < n {
        let mut i = 0;
        while i < n {
            let mid = (i + width).min(n);
            let hi = (i + 2 * width).min(n);
            let (mut l, mut r, mut k) = (i, mid, i);
            while l < mid && r < hi {
                if vm.compare(CmpOp::Lt, keys[idx[r]], keys[idx[l]])? {
                    buf[k] = idx[r];
                    r += 1;
                } else {
                    buf[k] = idx[l];
                    l += 1;
                }
                k += 1;
            }
            buf[k..k + (mid - l)].copy_from_slice(&idx[l..mid]);
            k += mid - l;
            buf[k..k + (hi - r)].copy_from_slice(&idx[r..hi]);
            i += 2 * width;
        }
        std::mem::swap(&mut idx, &mut buf);
        width *= 2;
    }
    Ok(idx)
}

/// Runs `f` over every item of `xs`, collecting results into a new list.
/// `keep_item` = filter mode (keep items whose result is truthy).
fn map_impl(vm: &mut Vm, f: Value, xs: Value, keep_item: bool) -> R {
    let mark = vm.temp_mark();
    let items = vm.to_vec(xs)?;
    let snapshot = vm.heap.list(items);
    let out = vm.heap.list(Vec::new());
    vm.push_temp(snapshot)?;
    vm.push_temp(out)?;
    let (snap, outg) = match (snapshot, out) {
        (Value::Obj(a), Value::Obj(b)) => (a, b),
        _ => unreachable!(),
    };
    let n = list_mut(snap).len();
    for i in 0..n {
        let item = list_mut(snap)[i];
        let r = if keep_item && matches!(f, Value::None) {
            item
        } else {
            match vm.call(f, &[item]) {
                Ok(r) => r,
                Err(e) => {
                    vm.temp_reset(mark);
                    return Err(e);
                }
            }
        };
        if !keep_item {
            list_mut(outg).push(r);
        } else if vm.truthy(r)? {
            list_mut(outg).push(item);
        }
    }
    vm.temp_reset(mark);
    Ok(out)
}

/// `map(f, xs)` — returns a list (not a lazy iterator).
fn map_native(vm: &mut Vm, args: &[Value]) -> R {
    check_args("map", args, 2, 2)?;
    map_impl(vm, args[0], args[1], false)
}

/// `filter(f, xs)` — returns a list; `filter(None, xs)` keeps truthy items.
fn filter_native(vm: &mut Vm, args: &[Value]) -> R {
    check_args("filter", args, 2, 2)?;
    map_impl(vm, args[0], args[1], true)
}

/// `enumerate(xs, start=0)` — list of [index, item] pairs.
fn enumerate_native(vm: &mut Vm, args: &[Value]) -> R {
    check_args("enumerate", args, 1, 2)?;
    let start_kw = vm.take_kw(sym::START);
    let start = match args.get(1).or(start_kw.as_ref()) {
        Some(v) => want_int(*v, "enumerate() start")?,
        None => 0,
    };
    let items = vm.to_vec(args[0])?;
    let pairs = items
        .into_iter()
        .enumerate()
        .map(|(i, x)| vm.heap.tuple(vec![Value::Int(start + i as i64), x]))
        .collect();
    Ok(vm.heap.list(pairs))
}

/// `zip(a, b, ...)` — list of lists, as long as the shortest input.
fn zip_native(vm: &mut Vm, args: &[Value]) -> R {
    let mut cols = Vec::with_capacity(args.len());
    for a in args {
        cols.push(vm.to_vec(*a)?);
    }
    let n = cols.iter().map(|c| c.len()).min().unwrap_or(0);
    let rows = (0..n)
        .map(|i| {
            let row = cols.iter().map(|c| c[i]).collect();
            vm.heap.tuple(row)
        })
        .collect();
    Ok(vm.heap.list(rows))
}

fn any_native(vm: &mut Vm, args: &[Value]) -> R {
    check_args("any", args, 1, 1)?;
    let (mark, items) = rooted_items(vm, args[0])?;
    let mut r = Ok(false);
    for v in items {
        match vm.truthy(v) {
            Ok(true) => {
                r = Ok(true);
                break;
            }
            Ok(false) => {}
            Err(e) => {
                r = Err(e);
                break;
            }
        }
    }
    vm.temp_reset(mark);
    Ok(Value::Bool(r?))
}

fn all_native(vm: &mut Vm, args: &[Value]) -> R {
    check_args("all", args, 1, 1)?;
    let (mark, items) = rooted_items(vm, args[0])?;
    let mut r = Ok(true);
    for v in items {
        match vm.truthy(v) {
            Ok(false) => {
                r = Ok(false);
                break;
            }
            Ok(true) => {}
            Err(e) => {
                r = Err(e);
                break;
            }
        }
    }
    vm.temp_reset(mark);
    Ok(Value::Bool(r?))
}

fn callable_native(_: &mut Vm, args: &[Value]) -> R {
    check_args("callable", args, 1, 1)?;
    Ok(Value::Bool(match args[0] {
        Value::Obj(g) => matches!(
            g.get(),
            Obj::Func(_) | Obj::Closure(_) | Obj::Native(_) | Obj::Class(_) | Obj::Bound(_)
        ),
        _ => false,
    }))
}

fn chr(vm: &mut Vm, args: &[Value]) -> R {
    check_args("chr", args, 1, 1)?;
    let i = want_int(args[0], "chr() argument")?;
    let c = u32::try_from(i)
        .ok()
        .and_then(char::from_u32)
        .ok_or_else(|| "ValueError: chr() arg not in range".to_string())?;
    Ok(vm.heap.string(c.to_string()))
}

fn ord(_: &mut Vm, args: &[Value]) -> R {
    check_args("ord", args, 1, 1)?;
    let s = want_str(args[0], "ord() argument")?;
    let mut cs = s.chars();
    match (cs.next(), cs.next()) {
        (Some(c), None) => Ok(Value::Int(c as i64)),
        _ => Err("TypeError: ord() expected a single character".into()),
    }
}

fn clock(vm: &mut Vm, args: &[Value]) -> R {
    check_args("clock", args, 0, 0)?;
    Ok(Value::Float(vm.start.elapsed().as_secs_f64()))
}

/// f-string assembly: concatenates the str() of every argument into one new string.
fn fmt_concat(vm: &mut Vm, args: &[Value]) -> R {
    if let [one] = args {
        if one.as_str().is_some() {
            return Ok(*one);
        }
    }
    let mut out = String::with_capacity(32);
    for a in args {
        match a {
            Value::Obj(g) if !matches!(g.get(), Obj::Str(_)) => vm.display(*a, false, &mut out, 0)?,
            _ => a.write_str(&mut out),
        }
    }
    Ok(vm.heap.string(out))
}

/// `format(value, spec)` with a Python-style spec: [[fill]align][sign][0][width][,][.precision][type]
fn format_value(vm: &mut Vm, args: &[Value]) -> R {
    check_args("format", args, 1, 2)?;
    let v = args[0];
    let spec = match args.get(1) {
        None => {
            if v.as_str().is_some() {
                return Ok(v);
            }
            let s = vm.str_of(v)?;
            return Ok(vm.heap.string(s));
        }
        Some(s) => want_str(*s, "format spec")?,
    };
    let s = apply_format_spec(v, spec)?;
    Ok(vm.heap.string(s))
}

fn apply_format_spec(v: Value, spec: &str) -> Result<String, String> {
    let bad = || format!("ValueError: invalid format spec '{}'", spec);
    let cs: Vec<char> = spec.chars().collect();
    let mut i = 0;
    let (mut fill, mut align) = (' ', None::<char>);
    if cs.len() >= 2 && matches!(cs[1], '<' | '>' | '^') {
        fill = cs[0];
        align = Some(cs[1]);
        i = 2;
    } else if !cs.is_empty() && matches!(cs[0], '<' | '>' | '^') {
        align = Some(cs[0]);
        i = 1;
    }
    let mut sign = '-';
    if i < cs.len() && matches!(cs[i], '+' | '-' | ' ') {
        sign = cs[i];
        i += 1;
    }
    let mut zero_pad = false;
    if i < cs.len() && cs[i] == '0' {
        zero_pad = true;
        i += 1;
    }
    let mut width = 0usize;
    while i < cs.len() && cs[i].is_ascii_digit() {
        width = width * 10 + cs[i].to_digit(10).unwrap() as usize;
        i += 1;
    }
    let mut grouping = false;
    if i < cs.len() && cs[i] == ',' {
        grouping = true;
        i += 1;
    }
    let mut precision: Option<usize> = None;
    if i < cs.len() && cs[i] == '.' {
        i += 1;
        let mut p = 0usize;
        let start = i;
        while i < cs.len() && cs[i].is_ascii_digit() {
            p = p * 10 + cs[i].to_digit(10).unwrap() as usize;
            i += 1;
        }
        if i == start {
            return Err(bad());
        }
        precision = Some(p);
    }
    let ty = if i < cs.len() { Some(cs[i]) } else { None };
    if i + 1 < cs.len() + usize::from(ty.is_none()) && ty.is_some() && i + 1 != cs.len() {
        return Err(bad());
    }

    let num = match v {
        Value::Int(n) => Some(n as f64),
        Value::Float(f) => Some(f),
        Value::Bool(b) => Some(b as i64 as f64),
        _ => None,
    };
    let group = |digits: &str| -> String {
        let (int_part, rest) = match digits.find('.') {
            Some(p) => (&digits[..p], &digits[p..]),
            None => (digits, ""),
        };
        let mut out = String::new();
        for (n, ch) in int_part.chars().enumerate() {
            if n > 0 && (int_part.len() - n) % 3 == 0 {
                out.push(',');
            }
            out.push(ch);
        }
        out + rest
    };

    let mut body = match ty {
        Some('f') | Some('F') | Some('%') | Some('e') | Some('E') | Some('g') => {
            let mut x = num.ok_or_else(|| {
                format!("ValueError: format '{}' needs a number, not '{}'", spec, v.type_name())
            })?;
            let p = precision.unwrap_or(6);
            if ty == Some('%') {
                x *= 100.0;
            }
            // Python writes exponents as e+05 / e-05 (sign, at least two digits).
            let py_exp = |s: String, e: char| -> String {
                match s.split_once(e) {
                    Some((m, ex)) => {
                        let n: i32 = ex.parse().unwrap_or(0);
                        format!("{}{}{}{:02}", m, e, if n < 0 { '-' } else { '+' }, n.abs())
                    }
                    None => s,
                }
            };
            let mut t = match ty {
                Some('e') => py_exp(format!("{:.*e}", p, x.abs()), 'e'),
                Some('E') => py_exp(format!("{:.*E}", p, x.abs()), 'E'),
                Some('g') => {
                    let s = format!("{}", x.abs());
                    s
                }
                _ => format!("{:.*}", p, x.abs()),
            };
            if grouping {
                t = group(&t);
            }
            if ty == Some('%') {
                t.push('%');
            }
            if x.is_sign_negative() && x != 0.0 {
                format!("-{}", t)
            } else if sign != '-' {
                format!("{}{}", sign, t)
            } else {
                t
            }
        }
        Some('d') | Some('x') | Some('X') | Some('b') | Some('o') => {
            let n = match v {
                Value::Int(n) => n,
                Value::Bool(b) => b as i64,
                _ => {
                    return Err(format!(
                        "ValueError: format '{}' needs an int, not '{}'",
                        spec,
                        v.type_name()
                    ))
                }
            };
            let mag = n.unsigned_abs();
            let mut t = match ty {
                Some('x') => format!("{:x}", mag),
                Some('X') => format!("{:X}", mag),
                Some('b') => format!("{:b}", mag),
                Some('o') => format!("{:o}", mag),
                _ => format!("{}", mag),
            };
            if grouping {
                t = group(&t);
            }
            if n < 0 {
                format!("-{}", t)
            } else if sign != '-' {
                format!("{}{}", sign, t)
            } else {
                t
            }
        }
        Some('s') | None => {
            let mut t = match (v, precision) {
                (Value::Float(f), Some(p)) => format!("{:.*}", p, f),
                _ => v.to_str(),
            };
            if let (Some(p), Some(_)) = (precision, v.as_str()) {
                t = t.chars().take(p).collect();
            }
            if grouping {
                if let Value::Int(n) = v {
                    t = if n < 0 { format!("-{}", group(&n.unsigned_abs().to_string())) } else { group(&t) };
                }
            }
            t
        }
        Some(_) => return Err(bad()),
    };

    let len = body.chars().count();
    if len < width {
        let pad = width - len;
        let numeric = num.is_some() && v.as_str().is_none();
        if zero_pad && align.is_none() && numeric {
            let (sign_part, digits) = if body.starts_with(['-', '+', ' ']) {
                body.split_at(1)
            } else {
                ("", body.as_str())
            };
            body = format!("{}{}{}", sign_part, "0".repeat(pad), digits);
        } else {
            let align = align.unwrap_or(if numeric { '>' } else { '<' });
            let fillstr = |n: usize| fill.to_string().repeat(n);
            body = match align {
                '>' => format!("{}{}", fillstr(pad), body),
                '^' => format!("{}{}{}", fillstr(pad / 2), body, fillstr(pad - pad / 2)),
                _ => format!("{}{}", body, fillstr(pad)),
            };
        }
    }
    Ok(body)
}

// ---------------------------------------------------------------- methods

pub fn call_method(vm: &mut Vm, recv: Value, name: u32, args: &[Value]) -> R {
    if let Value::Obj(g) = recv {
        let r = match g.get() {
            Obj::List(_) => list_method(vm, g, name, args),
            Obj::Str(s) => str_method(vm, s, name, args),
            Obj::Dict(_) => dict_method(vm, g, name, args),
            Obj::Set(_) => set_method(vm, g, name, args),
            Obj::Tuple(t) => tuple_method(vm, t, name, args),
            Obj::Handle(h) => {
                let method = h.ty.method;
                let n = vm.syms.name(name).to_string();
                method(vm, g, &n, args)
            }
            Obj::Generator(_) => generator_method(vm, g, name, args),
            _ => None,
        };
        if let Some(r) = r {
            return r;
        }
    }
    Err(format!(
        "AttributeError: '{}' object has no attribute '{}'",
        recv.type_name(),
        vm.syms.name(name)
    ))
}

fn list_mut<'a>(g: Gc) -> &'a mut Vec<Value> {
    match g.get_mut() {
        Obj::List(l) => l,
        _ => unreachable!(),
    }
}

fn list_method(vm: &mut Vm, g: Gc, name: u32, args: &[Value]) -> Option<R> {
    if name == sym::SORT && args.len() <= 1 {
        // list.sort(key) may call back into Faston code, so it needs the whole VM.
        let (key, reverse) = match sort_options(vm, args.first().copied()) {
            Ok(o) => o,
            Err(e) => return Some(Err(e)),
        };
        return Some(sort_by_key(vm, g, key, reverse).map(|_| Value::None));
    }
    let syms: *const Interner = &vm.syms;
    let mname = move || unsafe { (*syms).name(name).to_string() };
    let c = |min, max| arity(args, min, max, || format!("list.{}", mname()));
    Some((|| -> R {
        match name {
            sym::APPEND => {
                c(1, 1)?;
                list_mut(g).push(args[0]);
                Ok(Value::None)
            }
            sym::EXTEND => {
                c(1, 1)?;
                let items = vm.to_vec(args[0])?;
                list_mut(g).extend(items);
                Ok(Value::None)
            }
            sym::POP => {
                c(0, 1)?;
                let l = list_mut(g);
                if l.is_empty() {
                    return Err("IndexError: pop from empty list".into());
                }
                let len = l.len() as i64;
                let i = match args.first() {
                    Some(a) => want_int(*a, "index")?,
                    None => len - 1,
                };
                let i = if i < 0 { i + len } else { i };
                if i < 0 || i >= len {
                    return Err("IndexError: pop index out of range".into());
                }
                Ok(l.remove(i as usize))
            }
            sym::INSERT => {
                c(2, 2)?;
                let l = list_mut(g);
                let len = l.len() as i64;
                let i = want_int(args[0], "index")?;
                let i = if i < 0 { (i + len).max(0) } else { i.min(len) };
                l.insert(i as usize, args[1]);
                Ok(Value::None)
            }
            sym::REMOVE => {
                c(1, 1)?;
                let items = list_mut(g).clone();
                match positions_equal(vm, &items, args[0])?.first() {
                    Some(&i) => {
                        list_mut(g).remove(i);
                        Ok(Value::None)
                    }
                    None => Err("ValueError: list.remove(x): x not in list".into()),
                }
            }
            sym::INDEX => {
                c(1, 1)?;
                let items = list_mut(g).clone();
                positions_equal(vm, &items, args[0])?
                    .first()
                    .map(|&i| Value::Int(i as i64))
                    .ok_or_else(|| format!("ValueError: {} is not in list", args[0].repr()))
            }
            sym::COUNT => {
                c(1, 1)?;
                let items = list_mut(g).clone();
                Ok(Value::Int(positions_equal(vm, &items, args[0])?.len() as i64))
            }
            sym::CLEAR => {
                c(0, 0)?;
                list_mut(g).clear();
                Ok(Value::None)
            }
            sym::COPY => {
                c(0, 0)?;
                let v = list_mut(g).clone();
                Ok(vm.heap.list(v))
            }
            sym::REVERSE => {
                c(0, 0)?;
                list_mut(g).reverse();
                Ok(Value::None)
            }
            sym::SORT => {
                c(0, 1)?;
                let mut v = std::mem::take(list_mut(g));
                let r = ops::sort_values(&mut v);
                *list_mut(g) = v;
                r.map(|_| Value::None)
            }
            _ => Err(format!(
                "AttributeError: 'list' object has no attribute '{}'",
                mname()
            )),
        }
    })())
}

fn char_index(s: &str, byte: usize) -> i64 {
    s[..byte].chars().count() as i64
}

/// Byte offset of character `idx` (clamped to the end).
fn byte_of_char(s: &str, idx: usize) -> usize {
    s.char_indices().nth(idx).map_or(s.len(), |(b, _)| b)
}

/// `str.split` / `str.rsplit` with Python's rules (sep None = runs of whitespace).
fn split_str(s: &str, sep: Option<&str>, maxsplit: i64, rev: bool) -> Vec<String> {
    match sep {
        Some(sep) => {
            if maxsplit < 0 {
                s.split(sep).map(String::from).collect()
            } else if rev {
                let mut v: Vec<String> = s.rsplitn(maxsplit as usize + 1, sep).map(String::from).collect();
                v.reverse();
                v
            } else {
                s.splitn(maxsplit as usize + 1, sep).map(String::from).collect()
            }
        }
        None => {
            if maxsplit < 0 {
                return s.split_whitespace().map(String::from).collect();
            }
            let mut out = Vec::new();
            if !rev {
                let mut rest = s.trim_start();
                while !rest.is_empty() {
                    if out.len() as i64 == maxsplit {
                        out.push(rest.to_string());
                        break;
                    }
                    match rest.find(char::is_whitespace) {
                        Some(p) => {
                            out.push(rest[..p].to_string());
                            rest = rest[p..].trim_start();
                        }
                        None => {
                            out.push(rest.to_string());
                            break;
                        }
                    }
                }
            } else {
                let mut rest = s.trim_end();
                while !rest.is_empty() {
                    if out.len() as i64 == maxsplit {
                        out.push(rest.to_string());
                        break;
                    }
                    match rest.rfind(char::is_whitespace) {
                        Some(p) => {
                            let w = rest[p..].chars().next().map_or(1, char::len_utf8);
                            out.push(rest[p + w..].to_string());
                            rest = rest[..p].trim_end();
                        }
                        None => {
                            out.push(rest.to_string());
                            break;
                        }
                    }
                }
                out.reverse();
            }
            out
        }
    }
}

fn title_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_cased = false;
    for ch in s.chars() {
        if prev_cased {
            out.extend(ch.to_lowercase());
        } else {
            out.extend(ch.to_uppercase());
        }
        prev_cased = ch.is_uppercase() || ch.is_lowercase();
    }
    out
}

/// `find` / `rfind` within s[start:end] (character indices, Python slice rules).
fn find_in(s: &str, sub: &str, start: Option<i64>, end: Option<i64>, rev: bool) -> Option<i64> {
    let n = s.chars().count() as i64;
    let norm = |i: i64| if i < 0 { (i + n).max(0) } else { i.min(n) };
    let a = norm(start.unwrap_or(0));
    let b = norm(end.unwrap_or(n));
    if a > b {
        return None;
    }
    let (ba, bb) = (byte_of_char(s, a as usize), byte_of_char(s, b as usize));
    let hay = &s[ba..bb];
    let p = if rev { hay.rfind(sub) } else { hay.find(sub) }?;
    Some(a + char_index(hay, p))
}

/// `template.format(*args, **kwargs)`.
fn str_format(vm: &mut Vm, fmt: &str, args: &[Value]) -> Result<String, String> {
    let kws = vm.take_all_kw();
    let mut auto = 0usize;
    format_template(vm, fmt, args, &kws, &mut auto, 0)
}

fn format_template(
    vm: &mut Vm,
    fmt: &str,
    args: &[Value],
    kws: &[(u32, Value)],
    auto: &mut usize,
    depth: usize,
) -> Result<String, String> {
    let chars: Vec<char> = fmt.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if ch == '{' {
            if chars.get(i + 1) == Some(&'{') {
                out.push('{');
                i += 2;
                continue;
            }
            let mut level = 1;
            let mut j = i + 1;
            while j < chars.len() {
                match chars[j] {
                    '{' => level += 1,
                    '}' => {
                        level -= 1;
                        if level == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            if j >= chars.len() {
                return Err("ValueError: Single '{' encountered in format string".into());
            }
            let field: String = chars[i + 1..j].iter().collect();
            i = j + 1;
            // field_name[!conversion][:spec]
            let (head, spec) = match field.find(':') {
                Some(p) => (&field[..p], &field[p + 1..]),
                None => (&field[..], ""),
            };
            let (fname, conv) = match head.find('!') {
                Some(p) => (&head[..p], Some(&head[p + 1..])),
                None => (head, None),
            };
            let v = format_field(vm, fname, args, kws, auto)?;
            let spec = if spec.contains('{') && depth < 2 {
                format_template(vm, spec, args, kws, auto, depth + 1)?
            } else {
                spec.to_string()
            };
            let text = match conv {
                None => None,
                Some("r") | Some("a") => Some(vm.repr_of(v)?),
                Some("s") => Some(vm.str_of(v)?),
                Some(other) => return Err(format!("ValueError: Unknown conversion specifier {}", other)),
            };
            if spec.is_empty() {
                match text {
                    Some(t) => out.push_str(&t),
                    None => out.push_str(&vm.str_of(v)?),
                }
            } else {
                let target = match text {
                    Some(t) => vm.heap.string(t),
                    None if matches!(v, Value::Int(_) | Value::Float(_) | Value::Bool(_)) || v.as_str().is_some() => v,
                    None => {
                        let t = vm.str_of(v)?;
                        vm.heap.string(t)
                    }
                };
                out.push_str(&apply_format_spec(target, &spec)?);
            }
        } else if ch == '}' {
            if chars.get(i + 1) == Some(&'}') {
                out.push('}');
                i += 2;
                continue;
            }
            return Err("ValueError: Single '}' encountered in format string".into());
        } else {
            out.push(ch);
            i += 1;
        }
    }
    Ok(out)
}

/// Looks up `0`, `name`, `name.attr`, `name[key]` (or an automatic index for "").
fn format_field(vm: &mut Vm, field: &str, args: &[Value], kws: &[(u32, Value)], auto: &mut usize) -> R {
    let end = field.find(['.', '[']).unwrap_or(field.len());
    let (first, mut rest) = (&field[..end], &field[end..]);
    let mut v = if first.is_empty() {
        let i = *auto;
        *auto += 1;
        *args.get(i).ok_or_else(|| {
            format!("IndexError: Replacement index {} out of range for positional args tuple", i)
        })?
    } else if first.chars().all(|c| c.is_ascii_digit()) {
        let i: usize = first.parse().unwrap_or(usize::MAX);
        *args.get(i).ok_or_else(|| {
            format!("IndexError: Replacement index {} out of range for positional args tuple", i)
        })?
    } else {
        kws.iter()
            .find(|(k, _)| vm.syms.name(*k) == first)
            .map(|(_, v)| *v)
            .ok_or_else(|| format!("KeyError: '{}'", first))?
    };
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix('.') {
            let e = r.find(['.', '[']).unwrap_or(r.len());
            let name = vm.syms.intern(&r[..e]);
            v = vm.get_attr(v, name)?;
            rest = &r[e..];
        } else if let Some(r) = rest.strip_prefix('[') {
            let e = r.find(']').ok_or("ValueError: Missing ']' in format string")?;
            let key_text = &r[..e];
            let key = match key_text.parse::<i64>() {
                Ok(n) => Value::Int(n),
                Err(_) => vm.heap.string(key_text),
            };
            v = ops::get_index(&mut vm.heap, v, key)?;
            rest = &r[e + 1..];
        } else {
            return Err(format!("ValueError: invalid format field '{}'", field));
        }
    }
    Ok(v)
}

fn str_method(vm: &mut Vm, s: &str, name: u32, args: &[Value]) -> Option<R> {
    let syms: *const Interner = &vm.syms;
    let mname = move || unsafe { (*syms).name(name).to_string() };
    let c = |min, max| arity(args, min, max, || format!("str.{}", mname()));
    Some((|| -> R {
        match name {
            sym::UPPER => {
                c(0, 0)?;
                Ok(vm.heap.string(s.to_uppercase()))
            }
            sym::LOWER => {
                c(0, 0)?;
                Ok(vm.heap.string(s.to_lowercase()))
            }
            sym::STRIP | sym::LSTRIP | sym::RSTRIP => {
                c(0, 1)?;
                let r = match args.first() {
                    None => match name {
                        sym::STRIP => s.trim(),
                        sym::LSTRIP => s.trim_start(),
                        _ => s.trim_end(),
                    },
                    Some(chars) => {
                        let set: Vec<char> = want_str(*chars, "chars")?.chars().collect();
                        let f = |ch: char| set.contains(&ch);
                        match name {
                            sym::STRIP => s.trim_matches(f),
                            sym::LSTRIP => s.trim_start_matches(f),
                            _ => s.trim_end_matches(f),
                        }
                    }
                };
                Ok(vm.heap.string(r))
            }
            sym::SPLIT | sym::RSPLIT => {
                c(0, 2)?;
                let sep = vm.take_kw(sym::SEP).or(args.first().copied()).unwrap_or(Value::None);
                let maxsplit = match vm.take_kw(sym::MAXSPLIT).or(args.get(1).copied()) {
                    None => -1,
                    Some(v) => want_int(v, "maxsplit")?,
                };
                let sep = match sep {
                    Value::None => None,
                    v => {
                        let sep = want_str(v, "separator")?;
                        if sep.is_empty() {
                            return Err("ValueError: empty separator".into());
                        }
                        Some(sep)
                    }
                };
                let parts = split_str(s, sep, maxsplit, name == sym::RSPLIT);
                let vals = parts.into_iter().map(|p| vm.heap.string(p)).collect();
                Ok(vm.heap.list(vals))
            }
            sym::TITLE => {
                c(0, 0)?;
                Ok(vm.heap.string(title_case(s)))
            }
            sym::CAPITALIZE => {
                c(0, 0)?;
                let mut cs = s.chars();
                let r = match cs.next() {
                    Some(f) => f.to_uppercase().collect::<String>() + &cs.as_str().to_lowercase(),
                    None => String::new(),
                };
                Ok(vm.heap.string(r))
            }
            sym::SWAPCASE => {
                c(0, 0)?;
                let r: String = s
                    .chars()
                    .flat_map(|ch| -> Vec<char> {
                        if ch.is_uppercase() {
                            ch.to_lowercase().collect()
                        } else if ch.is_lowercase() {
                            ch.to_uppercase().collect()
                        } else {
                            vec![ch]
                        }
                    })
                    .collect();
                Ok(vm.heap.string(r))
            }
            sym::CASEFOLD => {
                c(0, 0)?;
                Ok(vm.heap.string(s.to_lowercase()))
            }
            sym::ZFILL => {
                c(1, 1)?;
                let width = want_int(args[0], "width")?;
                let n = s.chars().count() as i64;
                if n >= width {
                    return Ok(vm.heap.string(s));
                }
                let zeros = "0".repeat((width - n) as usize);
                let r = match s.chars().next() {
                    Some(sign @ ('+' | '-')) => format!("{}{}{}", sign, zeros, &s[1..]),
                    _ => format!("{}{}", zeros, s),
                };
                Ok(vm.heap.string(r))
            }
            sym::CENTER | sym::LJUST | sym::RJUST => {
                c(1, 2)?;
                let width = want_int(args[0], "width")?;
                let fill = match args.get(1) {
                    None => ' ',
                    Some(f) => {
                        let f = want_str(*f, "fill character")?;
                        let mut it = f.chars();
                        match (it.next(), it.next()) {
                            (Some(ch), None) => ch,
                            _ => return Err("TypeError: the fill character must be exactly one character long".into()),
                        }
                    }
                };
                let n = s.chars().count() as i64;
                if n >= width {
                    return Ok(vm.heap.string(s));
                }
                let marg = width - n;
                let (left, right) = match name {
                    sym::LJUST => (0, marg),
                    sym::RJUST => (marg, 0),
                    // CPython's rule for where the odd padding character goes
                    _ => {
                        let left = marg / 2 + (marg & width & 1);
                        (left, marg - left)
                    }
                };
                let f = |k: i64| fill.to_string().repeat(k as usize);
                Ok(vm.heap.string(format!("{}{}{}", f(left), s, f(right))))
            }
            sym::SPLITLINES => {
                c(0, 1)?;
                let keep = match vm.take_kw(sym::KEEPENDS).or(args.first().copied()) {
                    Some(v) => vm.truthy(v)?,
                    None => false,
                };
                let mut lines = Vec::new();
                let mut start = 0;
                let b = s.as_bytes();
                let mut i = 0;
                while i < b.len() {
                    let (is_break, len) = match b[i] {
                        b'\n' => (true, 1),
                        b'\r' => (true, if b.get(i + 1) == Some(&b'\n') { 2 } else { 1 }),
                        _ => (false, 1),
                    };
                    if is_break {
                        lines.push(&s[start..if keep { i + len } else { i }]);
                        start = i + len;
                    }
                    i += len;
                }
                if start < s.len() {
                    lines.push(&s[start..]);
                }
                let vals = lines.into_iter().map(|l| vm.heap.string(l)).collect();
                Ok(vm.heap.list(vals))
            }
            sym::PARTITION | sym::RPARTITION => {
                c(1, 1)?;
                let sep = want_str(args[0], "separator")?;
                if sep.is_empty() {
                    return Err("ValueError: empty separator".into());
                }
                let found = if name == sym::PARTITION { s.find(sep) } else { s.rfind(sep) };
                let (a, m, b) = match found {
                    Some(i) => (&s[..i], sep, &s[i + sep.len()..]),
                    None if name == sym::PARTITION => (s, "", ""),
                    None => ("", "", s),
                };
                let parts = vec![vm.heap.string(a), vm.heap.string(m), vm.heap.string(b)];
                Ok(vm.heap.tuple(parts))
            }
            sym::FIND | sym::RFIND | sym::INDEX | sym::RINDEX => {
                c(1, 3)?;
                let sub = want_str(args[0], "substring")?;
                let rev = name == sym::RFIND || name == sym::RINDEX;
                let start = match args.get(1) {
                    None | Some(Value::None) => None,
                    Some(v) => Some(want_int(*v, "start")?),
                };
                let end = match args.get(2) {
                    None | Some(Value::None) => None,
                    Some(v) => Some(want_int(*v, "end")?),
                };
                match find_in(s, sub, start, end, rev) {
                    Some(i) => Ok(Value::Int(i)),
                    None if name == sym::FIND || name == sym::RFIND => Ok(Value::Int(-1)),
                    None => Err("ValueError: substring not found".into()),
                }
            }
            sym::REMOVEPREFIX => {
                c(1, 1)?;
                let p = want_str(args[0], "prefix")?;
                Ok(vm.heap.string(s.strip_prefix(p).unwrap_or(s)))
            }
            sym::REMOVESUFFIX => {
                c(1, 1)?;
                let p = want_str(args[0], "suffix")?;
                Ok(vm.heap.string(s.strip_suffix(p).unwrap_or(s)))
            }
            sym::ISUPPER => {
                c(0, 0)?;
                let cased = s.chars().any(|ch| ch.is_uppercase() || ch.is_lowercase());
                Ok(Value::Bool(cased && !s.chars().any(char::is_lowercase)))
            }
            sym::ISLOWER => {
                c(0, 0)?;
                let cased = s.chars().any(|ch| ch.is_uppercase() || ch.is_lowercase());
                Ok(Value::Bool(cased && !s.chars().any(char::is_uppercase)))
            }
            sym::ISALNUM => {
                c(0, 0)?;
                Ok(Value::Bool(!s.is_empty() && s.chars().all(char::is_alphanumeric)))
            }
            sym::ISNUMERIC => {
                c(0, 0)?;
                Ok(Value::Bool(!s.is_empty() && s.chars().all(char::is_numeric)))
            }
            sym::ISDECIMAL => {
                c(0, 0)?;
                Ok(Value::Bool(!s.is_empty() && s.chars().all(|ch| ch.is_ascii_digit())))
            }
            sym::ISASCII => {
                c(0, 0)?;
                Ok(Value::Bool(s.is_ascii()))
            }
            sym::ISTITLE => {
                c(0, 0)?;
                let (mut cased, mut prev_cased) = (false, false);
                for ch in s.chars() {
                    if ch.is_uppercase() {
                        if prev_cased {
                            return Ok(Value::Bool(false));
                        }
                        prev_cased = true;
                        cased = true;
                    } else if ch.is_lowercase() {
                        if !prev_cased {
                            return Ok(Value::Bool(false));
                        }
                        prev_cased = true;
                        cased = true;
                    } else {
                        prev_cased = false;
                    }
                }
                Ok(Value::Bool(cased))
            }
            sym::ISIDENTIFIER => {
                c(0, 0)?;
                let mut it = s.chars();
                let ok = match it.next() {
                    Some(f) => (f.is_alphabetic() || f == '_') && it.all(|ch| ch.is_alphanumeric() || ch == '_'),
                    None => false,
                };
                Ok(Value::Bool(ok))
            }
            sym::EXPANDTABS => {
                c(0, 1)?;
                let size = match args.first() {
                    Some(v) => want_int(*v, "tabsize")?.max(0) as usize,
                    None => 8,
                };
                let mut out = String::new();
                let mut col = 0;
                for ch in s.chars() {
                    match ch {
                        '\t' => {
                            if size > 0 {
                                let n = size - col % size;
                                out.push_str(&" ".repeat(n));
                                col += n;
                            }
                        }
                        '\n' | '\r' => {
                            out.push(ch);
                            col = 0;
                        }
                        _ => {
                            out.push(ch);
                            col += 1;
                        }
                    }
                }
                Ok(vm.heap.string(out))
            }
            sym::FORMAT => {
                let out = str_format(vm, s, args)?;
                Ok(vm.heap.string(out))
            }
            sym::JOIN => {
                c(1, 1)?;
                let items = vm.to_vec(args[0])?;
                let mut out = String::new();
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(s);
                    }
                    match v.as_str() {
                        Some(p) => out.push_str(p),
                        None => {
                            return Err(format!(
                                "TypeError: join() item {} is '{}', not str",
                                i,
                                v.type_name()
                            ))
                        }
                    }
                }
                Ok(vm.heap.string(out))
            }
            sym::REPLACE => {
                c(2, 2)?;
                let from = want_str(args[0], "old")?;
                let to = want_str(args[1], "new")?;
                Ok(vm.heap.string(s.replace(from, to)))
            }
            sym::STARTSWITH | sym::ENDSWITH => {
                c(1, 1)?;
                let what = if name == sym::STARTSWITH { "prefix" } else { "suffix" };
                // a single string or a tuple of them
                let options: Vec<Value> = match args[0] {
                    Value::Obj(g) if matches!(g.get(), Obj::Tuple(_)) => match g.get() {
                        Obj::Tuple(t) => t.to_vec(),
                        _ => unreachable!(),
                    },
                    v => vec![v],
                };
                for o in options {
                    let p = want_str(o, what)?;
                    let hit = if name == sym::STARTSWITH { s.starts_with(p) } else { s.ends_with(p) };
                    if hit {
                        return Ok(Value::Bool(true));
                    }
                }
                Ok(Value::Bool(false))
            }
            sym::COUNT => {
                c(1, 1)?;
                let sub = want_str(args[0], "substring")?;
                if sub.is_empty() {
                    return Ok(Value::Int(s.chars().count() as i64 + 1));
                }
                Ok(Value::Int(s.matches(sub).count() as i64))
            }
            sym::ISDIGIT => {
                c(0, 0)?;
                Ok(Value::Bool(!s.is_empty() && s.chars().all(|ch| ch.is_ascii_digit())))
            }
            sym::ISALPHA => {
                c(0, 0)?;
                Ok(Value::Bool(!s.is_empty() && s.chars().all(char::is_alphabetic)))
            }
            sym::ISSPACE => {
                c(0, 0)?;
                Ok(Value::Bool(!s.is_empty() && s.chars().all(char::is_whitespace)))
            }
            _ => Err(format!(
                "AttributeError: 'str' object has no attribute '{}'",
                mname()
            )),
        }
    })())
}

/// Key/value pairs from a dict or an iterable of 2-item sequences.
fn dict_pairs(vm: &mut Vm, v: Value, what: &str) -> Result<Vec<(Value, Value)>, String> {
    if let Value::Obj(o) = v {
        if let Obj::Dict(d) = o.get() {
            return Ok(d.entries.clone());
        }
    }
    let (mark, items) = rooted_items(vm, v)
        .map_err(|_| format!("TypeError: {} needs a dict or pairs, not '{}'", what, v.type_name()))?;
    let mut out = Vec::with_capacity(items.len());
    let mut err = None;
    for (i, it) in items.into_iter().enumerate() {
        match vm.to_vec(it) {
            Ok(kv) if kv.len() == 2 => out.push((kv[0], kv[1])),
            Ok(kv) => {
                err = Some(format!(
                    "ValueError: {} sequence element #{} has length {}; 2 is required",
                    what,
                    i,
                    kv.len()
                ));
                break;
            }
            Err(e) => {
                err = Some(e);
                break;
            }
        }
    }
    vm.temp_reset(mark);
    match err {
        Some(e) => Err(e),
        None => Ok(out),
    }
}

fn dict_mut<'a>(g: Gc) -> &'a mut Dict {
    match g.get_mut() {
        Obj::Dict(d) => d,
        _ => unreachable!(),
    }
}

fn dict_method(vm: &mut Vm, g: Gc, name: u32, args: &[Value]) -> Option<R> {
    let syms: *const Interner = &vm.syms;
    let mname = move || unsafe { (*syms).name(name).to_string() };
    let c = |min, max| arity(args, min, max, || format!("dict.{}", mname()));
    Some((|| -> R {
        match name {
            sym::KEYS => {
                c(0, 0)?;
                let v = dict_mut(g).entries.iter().map(|(k, _)| *k).collect();
                Ok(vm.heap.list(v))
            }
            sym::VALUES => {
                c(0, 0)?;
                let v = dict_mut(g).entries.iter().map(|(_, v)| *v).collect();
                Ok(vm.heap.list(v))
            }
            sym::ITEMS => {
                c(0, 0)?;
                let pairs: Vec<(Value, Value)> = dict_mut(g).entries.clone();
                let v = pairs.into_iter().map(|(k, v)| vm.heap.tuple(vec![k, v])).collect();
                Ok(vm.heap.list(v))
            }
            sym::GET => {
                c(1, 2)?;
                let key = to_key(args[0])?;
                Ok(dict_mut(g)
                    .get(&key)
                    .unwrap_or_else(|| args.get(1).copied().unwrap_or(Value::None)))
            }
            sym::POP => {
                c(1, 2)?;
                let key = to_key(args[0])?;
                match (dict_mut(g).remove(&key), args.get(1)) {
                    (Some(v), _) => Ok(v),
                    (None, Some(default)) => Ok(*default),
                    (None, None) => Err(format!("KeyError: {}", args[0].repr())),
                }
            }
            sym::UPDATE => {
                c(0, 1)?;
                let mut pairs = match args.first() {
                    None => Vec::new(),
                    Some(&a) => dict_pairs(vm, a, "dict.update()")?,
                };
                for (k, v) in vm.take_all_kw() {
                    let key = vm.heap.string(vm.syms.name(k).to_string());
                    pairs.push((key, v));
                }
                let me = dict_mut(g);
                for (k, v) in pairs {
                    me.insert(to_key(k)?, k, v);
                }
                Ok(Value::None)
            }
            sym::SETDEFAULT => {
                c(1, 2)?;
                let key = to_key(args[0])?;
                let d = dict_mut(g);
                match d.get(&key) {
                    Some(v) => Ok(v),
                    None => {
                        let v = args.get(1).copied().unwrap_or(Value::None);
                        d.insert(key, args[0], v);
                        Ok(v)
                    }
                }
            }
            sym::POPITEM => {
                c(0, 0)?;
                let last = dict_mut(g).entries.last().copied();
                match last {
                    Some((k, v)) => {
                        dict_mut(g).remove(&to_key(k)?);
                        Ok(vm.heap.tuple(vec![k, v]))
                    }
                    None => Err("KeyError: 'popitem(): dictionary is empty'".into()),
                }
            }
            sym::CLEAR => {
                c(0, 0)?;
                dict_mut(g).clear();
                Ok(Value::None)
            }
            sym::COPY => {
                c(0, 0)?;
                let d = dict_mut(g).clone();
                Ok(vm.heap.dict(d))
            }
            _ => Err(format!(
                "AttributeError: 'dict' object has no attribute '{}'",
                mname()
            )),
        }
    })())
}

fn set_mut<'a>(g: Gc) -> &'a mut Dict {
    match g.get_mut() {
        Obj::Set(d) => d,
        _ => unreachable!(),
    }
}

fn set_method(vm: &mut Vm, g: Gc, name: u32, args: &[Value]) -> Option<R> {
    // The interner is only read for error messages; a raw pointer keeps `vm` free
    // for the method bodies (which allocate and convert iterables).
    let syms: *const Interner = &vm.syms;
    let mname = move || unsafe { (*syms).name(name).to_string() };
    let c = |min, max| arity(args, min, max, || format!("set.{}", mname()));
    let other_items = |vm: &mut Vm, i: usize| vm.to_vec(args[i]);
    Some((|| -> R {
        match name {
            sym::ADD => {
                c(1, 1)?;
                set_mut(g).insert(to_key(args[0])?, args[0], Value::None);
                Ok(Value::None)
            }
            sym::REMOVE => {
                c(1, 1)?;
                match set_mut(g).remove(&to_key(args[0])?) {
                    Some(_) => Ok(Value::None),
                    None => Err(format!("KeyError: {}", args[0].repr())),
                }
            }
            sym::DISCARD => {
                c(1, 1)?;
                set_mut(g).remove(&to_key(args[0])?);
                Ok(Value::None)
            }
            sym::POP => {
                c(0, 0)?;
                let d = set_mut(g);
                let first = d.entries.first().map(|(k, _)| *k);
                match first {
                    Some(k) => {
                        d.remove(&to_key(k)?);
                        Ok(k)
                    }
                    None => Err("KeyError: 'pop from an empty set'".into()),
                }
            }
            sym::CLEAR => {
                c(0, 0)?;
                set_mut(g).clear();
                Ok(Value::None)
            }
            sym::COPY => {
                c(0, 0)?;
                let d = set_mut(g).clone();
                Ok(vm.heap.set(d))
            }
            sym::UPDATE => {
                c(1, 1)?;
                for x in other_items(vm, 0)? {
                    set_mut(g).insert(to_key(x)?, x, Value::None);
                }
                Ok(Value::None)
            }
            sym::UNION | sym::INTERSECTION | sym::DIFFERENCE | sym::SYMMETRIC_DIFFERENCE => {
                c(1, 1)?;
                let items = other_items(vm, 0)?;
                let other = ops::make_set(&mut vm.heap, items)?;
                let me = Value::Obj(g);
                let h = &mut vm.heap;
                match name {
                    sym::UNION => ops::bitwise(h, me, other, '|'),
                    sym::INTERSECTION => ops::bitwise(h, me, other, '&'),
                    sym::DIFFERENCE => ops::sub(h, me, other),
                    _ => ops::bitwise(h, me, other, '^'),
                }
            }
            sym::ISSUBSET | sym::ISSUPERSET | sym::ISDISJOINT => {
                c(1, 1)?;
                let items = other_items(vm, 0)?;
                let other = ops::make_set(&mut vm.heap, items)?;
                let (a, b) = (set_mut(g), ops::set_of(other).unwrap());
                let has = |d: &Dict, k: &Value| d.contains(&to_key(*k).unwrap());
                Ok(Value::Bool(match name {
                    sym::ISSUBSET => a.entries.iter().all(|(k, _)| has(b, k)),
                    sym::ISSUPERSET => b.entries.iter().all(|(k, _)| has(a, k)),
                    _ => !a.entries.iter().any(|(k, _)| has(b, k)),
                }))
            }
            _ => Err(format!("AttributeError: 'set' object has no attribute '{}'", mname())),
        }
    })())
}

fn tuple_method(vm: &mut Vm, t: &[Value], name: u32, args: &[Value]) -> Option<R> {
    let syms: *const Interner = &vm.syms;
    let mname = move || unsafe { (*syms).name(name).to_string() };
    let c = |min, max| arity(args, min, max, || format!("tuple.{}", mname()));
    Some((|| -> R {
        match name {
            sym::COUNT => {
                c(1, 1)?;
                Ok(Value::Int(positions_equal(vm, t, args[0])?.len() as i64))
            }
            sym::INDEX => {
                c(1, 1)?;
                positions_equal(vm, t, args[0])?
                    .first()
                    .map(|&i| Value::Int(i as i64))
                    .ok_or_else(|| format!("ValueError: {} is not in tuple", args[0].repr()))
            }
            _ => Err(format!("AttributeError: 'tuple' object has no attribute '{}'", mname())),
        }
    })())
}
