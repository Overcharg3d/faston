//! Bytecode files (`.fbc`): compiled Faston modules that the VM loads without
//! the compiler, and the `bytecode` module that exposes this to Faston code
//! (so a compiler written in Faston can hand its output to the VM).
//!
//! Format (little endian; `str` = u32 byte length + UTF-8):
//!
//! ```text
//! file     := "FSTNBC" u16:version  symbols  globals  function
//! symbols  := u32:n  str*n        names used as attribute / method / class / import names
//! globals  := u32:n  str*n        the module's global variable names
//! function := str:name  u32:arity  u32:num_locals  u8:flags(1 *args, 2 **kwargs,
//!             4 defaults evaluated at def time)  u32:n (u32:symbol)*n  (parameter names)
//!             u32:n (u8:from_parent_local u32:index)*n  (captured variables)
//!             u32:n const*n  (defaults)   u32:n const*n  (constants)
//!             u32:n (u8:opcode u32:operand*k u32:line)*n   (k from the opcode table)
//! const    := u8:tag  0 None | 1 False | 2 True | 3 i64 | 4 f64 | 5 str | 6 u32:n bytes
//!             | 7 function | 8 u32:n (u32:symbol)*n  (keyword-argument names)
//! ```
//!
//! Opcodes and operand kinds are listed in bytecode.rs (`OPCODES`, also
//! `bytecode.opcodes` / `bytecode.operands` in Faston). Loading checks every
//! operand, every jump and the stack depth along every path, so a broken file
//! is reported as an error instead of crashing the VM.

use super::*;
use crate::bytecode::OPCODES;

pub const MAGIC: &[u8; 6] = b"FSTNBC";
pub const VERSION: u16 = 1;

// ---------------------------------------------------------------------------- writing

struct Writer<'a> {
    out: Vec<u8>,
    syms: &'a Interner,
    globals: &'a [String],
    sym_ids: HashMap<u32, u32>,
    sym_names: Vec<String>,
    glob_ids: HashMap<u32, u32>,
    glob_names: Vec<String>,
}

impl Writer<'_> {
    fn u8(&mut self, v: u8) {
        self.out.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }
    fn str(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.out.extend_from_slice(s.as_bytes());
    }
    fn sym(&mut self, s: u32) -> u32 {
        if let Some(&i) = self.sym_ids.get(&s) {
            return i;
        }
        let i = self.sym_names.len() as u32;
        self.sym_names.push(self.syms.name(s).to_string());
        self.sym_ids.insert(s, i);
        i
    }
    fn glob(&mut self, g: u32) -> u32 {
        if let Some(&i) = self.glob_ids.get(&g) {
            return i;
        }
        let i = self.glob_names.len() as u32;
        self.glob_names.push(self.globals.get(g as usize).cloned().unwrap_or_default());
        self.glob_ids.insert(g, i);
        i
    }

    fn konst(&mut self, v: Value) -> Result<(), String> {
        match v {
            Value::None => self.u8(0),
            Value::Bool(false) => self.u8(1),
            Value::Bool(true) => self.u8(2),
            Value::Int(i) => {
                self.u8(3);
                self.out.extend_from_slice(&i.to_le_bytes());
            }
            Value::Float(f) => {
                self.u8(4);
                self.out.extend_from_slice(&f.to_bits().to_le_bytes());
            }
            Value::Obj(g) => match g.get() {
                Obj::Str(s) => {
                    self.u8(5);
                    self.str(s);
                }
                Obj::Bytes(b) => {
                    self.u8(6);
                    self.u32(b.len() as u32);
                    self.out.extend_from_slice(b);
                }
                Obj::Func(_) => {
                    self.u8(7);
                    self.function(g)?;
                }
                Obj::Tuple(t) => {
                    self.u8(8);
                    self.u32(t.len() as u32);
                    for x in t.iter() {
                        match x {
                            Value::Int(s) => {
                                let i = self.sym(*s as u32);
                                self.u32(i);
                            }
                            _ => return Err("ValueError: unsupported tuple constant".into()),
                        }
                    }
                }
                _ => return Err(format!("ValueError: can't save a '{}' constant", v.type_name())),
            },
            Value::Undef => return Err("ValueError: can't save an undefined constant".into()),
        }
        Ok(())
    }

    fn function(&mut self, g: Gc) -> Result<(), String> {
        let f = g.as_func();
        self.str(&f.name);
        self.u32(f.arity);
        self.u32(f.num_locals);
        self.u8(f.varargs as u8 | (f.varkw as u8) << 1 | (f.dynamic_defaults as u8) << 2);
        self.u32(f.params.len() as u32);
        for &p in f.params.iter() {
            let i = self.sym(p);
            self.u32(i);
        }
        self.u32(f.captures.len() as u32);
        for &(local, idx) in f.captures.iter() {
            self.u8(local as u8);
            self.u32(idx);
        }
        self.u32(f.defaults.len() as u32);
        for &d in f.defaults.iter() {
            self.konst(d)?;
        }
        self.u32(f.consts.len() as u32);
        for &c in f.consts.iter() {
            self.konst(c)?;
        }
        self.u32(f.code.len() as u32);
        for (i, &op) in f.code.iter().enumerate() {
            let op = jit_rt::canon(op);
            let (code, args) = op.encode();
            let kinds = OPCODES[code as usize].1.as_bytes();
            self.u8(code);
            for (k, &a) in kinds.iter().zip(args.iter()) {
                let a = match k {
                    b'S' => self.sym(a),
                    b'G' => self.glob(a),
                    _ => a,
                };
                self.u32(a);
            }
            self.u32(f.lines.get(i).copied().unwrap_or(0));
        }
        Ok(())
    }
}

impl Vm {
    /// Serializes a compiled module (its top-level function and everything in it).
    pub fn encode_bytecode(&self, script: Gc) -> Result<Vec<u8>, String> {
        let unit = script.as_func().unit as usize;
        let mut w = Writer {
            out: Vec::new(),
            syms: &self.syms,
            globals: &self.units[unit].gtable.names,
            sym_ids: HashMap::new(),
            sym_names: Vec::new(),
            glob_ids: HashMap::new(),
            glob_names: Vec::new(),
        };
        w.function(script)?;
        let body = std::mem::take(&mut w.out);
        let (syms, globs) = (std::mem::take(&mut w.sym_names), std::mem::take(&mut w.glob_names));
        w.out.extend_from_slice(MAGIC);
        w.out.extend_from_slice(&VERSION.to_le_bytes());
        for table in [syms, globs] {
            w.u32(table.len() as u32);
            for s in table.iter() {
                w.str(s);
            }
        }
        w.out.extend_from_slice(&body);
        Ok(w.out)
    }

    /// Compiles source into a fresh module and serializes it.
    pub fn compile_to_bytecode(&mut self, name: &str, src: &str) -> Result<Vec<u8>, String> {
        let unit = self.new_unit(name.to_string(), None);
        let script = self.compile_unit_plain(unit, src)?;
        self.encode_bytecode(script)
    }
}

// ---------------------------------------------------------------------------- reading

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    /// file symbol index -> interned symbol
    syms: Vec<u32>,
    /// file global index -> slot in the unit
    globals: Vec<u32>,
    unit: u32,
}

fn bad(what: &str) -> String {
    format!("ValueError: bad bytecode: {}", what)
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        if self.data.len() - self.pos < n {
            return Err(bad("unexpected end of data"));
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    /// A count, sanity-checked against the bytes left (each item takes >= `min` bytes).
    fn count(&mut self, min: usize) -> Result<usize, String> {
        let n = self.u32()? as usize;
        if n.saturating_mul(min) > self.data.len() - self.pos {
            return Err(bad("count larger than the file"));
        }
        Ok(n)
    }
    fn str(&mut self) -> Result<String, String> {
        let n = self.count(1)?;
        let b = self.take(n)?;
        std::str::from_utf8(b).map(str::to_string).map_err(|_| bad("string is not UTF-8"))
    }
    fn sym(&mut self) -> Result<u32, String> {
        let i = self.u32()? as usize;
        self.syms.get(i).copied().ok_or_else(|| bad("symbol index out of range"))
    }

    fn konst(&mut self, vm: &mut Vm, parent: (u32, usize), depth: usize) -> Result<Value, String> {
        Ok(match self.u8()? {
            0 => Value::None,
            1 => Value::Bool(false),
            2 => Value::Bool(true),
            3 => Value::Int(self.u64()? as i64),
            4 => Value::Float(f64::from_bits(self.u64()?)),
            5 => {
                let s = self.str()?;
                vm.heap.string(s)
            }
            6 => {
                let n = self.count(1)?;
                let b = self.take(n)?.to_vec();
                vm.heap.bytes(b)
            }
            7 => Value::Obj(self.function(vm, Some(parent), depth + 1)?),
            8 => {
                let n = self.count(4)?;
                let mut items = Vec::with_capacity(n);
                for _ in 0..n {
                    items.push(Value::Int(self.sym()? as i64));
                }
                vm.heap.tuple(items)
            }
            t => return Err(bad(&format!("unknown constant tag {}", t))),
        })
    }

    /// Reads and checks one function. `parent` is (num_locals, captures) of the
    /// enclosing function, for checking what this one captures.
    fn function(&mut self, vm: &mut Vm, parent: Option<(u32, usize)>, depth: usize) -> Result<Gc, String> {
        if depth > 200 {
            return Err(bad("functions nested too deeply"));
        }
        let name = self.str()?;
        let arity = self.u32()?;
        let num_locals = self.u32()?;
        let flags = self.u8()?;
        let (varargs, varkw, dynamic) = (flags & 1 != 0, flags & 2 != 0, flags & 4 != 0);
        let np = self.count(4)?;
        let mut params = Vec::with_capacity(np);
        for _ in 0..np {
            params.push(self.sym()?);
        }
        let nc = self.count(5)?;
        let mut captures = Vec::with_capacity(nc);
        for _ in 0..nc {
            let local = self.u8()? != 0;
            let idx = self.u32()?;
            let ok = match parent {
                Some((pl, pc)) => if local { idx < pl } else { (idx as usize) < pc },
                None => false,
            };
            if !ok {
                return Err(bad(&format!("{}: captured variable out of range", name)));
            }
            captures.push((local, idx));
        }
        let here = (num_locals, captures.len());
        let nd = self.count(1)?;
        let mut defaults = Vec::with_capacity(nd);
        for _ in 0..nd {
            defaults.push(self.konst(vm, here, depth)?);
        }
        let nk = self.count(1)?;
        let mut consts = Vec::with_capacity(nk);
        for _ in 0..nk {
            consts.push(self.konst(vm, here, depth)?);
        }
        let n = self.count(5)?;
        let mut code = Vec::with_capacity(n);
        let mut lines = Vec::with_capacity(n);
        for i in 0..n {
            let opcode = self.u8()?;
            let kinds = OPCODES.get(opcode as usize).ok_or_else(|| bad(&format!("unknown opcode {}", opcode)))?.1;
            let mut args = [0u32; 3];
            for (j, k) in kinds.bytes().enumerate() {
                let a = self.u32()?;
                args[j] = match k {
                    b'S' => *self.syms.get(a as usize).ok_or_else(|| bad("symbol index out of range"))?,
                    b'G' => *self.globals.get(a as usize).ok_or_else(|| bad("global index out of range"))?,
                    b'X' => return Err(bad(&format!("{}: runtime-only instruction {}", name, OPCODES[opcode as usize].0))),
                    _ => a,
                };
            }
            let op = Op::decode(opcode, &args[..kinds.len()]).ok_or_else(|| bad("malformed instruction"))?;
            code.push(op);
            lines.push(self.u32()?);
            let _ = i;
        }
        if params.len() != arity as usize || defaults.len() > arity as usize {
            return Err(bad(&format!("{}: parameter list doesn't match the arity", name)));
        }
        if arity as u64 + varargs as u64 + varkw as u64 > num_locals as u64 || num_locals > 1 << 16 {
            return Err(bad(&format!("{}: bad local count", name)));
        }
        let max_stack = verify(&name, &code, &consts, num_locals, captures.len())?;
        let f = Function {
            name,
            arity,
            defaults,
            code,
            lines,
            consts,
            num_locals,
            max_stack: max_stack + 4,
            captures,
            unit: self.unit,
            dynamic_defaults: dynamic,
            fast_arity: if varargs || varkw { u32::MAX } else { arity },
            params,
            varargs,
            varkw,
            jit: std::ptr::null(),
            native_state: 0,
        };
        Ok(vm.heap.alloc(Obj::Func(Box::new(f))))
    }
}

/// Checks operands, jump targets and stack depths; returns the deepest stack.
fn verify(name: &str, code: &[Op], consts: &[Value], num_locals: u32, ncaptures: usize) -> Result<u32, String> {
    let err = |i: usize, what: &str| bad(&format!("{}: instruction {}: {}", name, i, what));
    if code.is_empty() {
        return Err(bad(&format!("{}: no code", name)));
    }
    let is_func = |k: u32| matches!(consts.get(k as usize), Some(Value::Obj(g)) if matches!(g.get(), Obj::Func(_)));
    let is_names = |k: u32| matches!(consts.get(k as usize), Some(Value::Obj(g)) if matches!(g.get(), Obj::Tuple(_)));
    for (i, &op) in code.iter().enumerate() {
        let (opcode, args) = op.encode();
        let kinds = OPCODES[opcode as usize].1.as_bytes();
        for (&k, &a) in kinds.iter().zip(args.iter()) {
            let ok = match k {
                b'K' => (a as usize) < consts.len(),
                b'L' => a < num_locals,
                b'T' => (a as usize) < code.len(),
                b'U' => (a as usize) < ncaptures,
                _ => true,
            };
            if !ok {
                return Err(err(i, &format!("{:?}: operand out of range", op)));
            }
        }
        let ok = match op {
            Op::Closure(k) | Op::ClosureDefaults(k, _) => is_func(k),
            Op::CallKw(_, k) | Op::InvokeKw(_, _, k) => is_names(k),
            // loop state takes three slots
            Op::GetIter(s) | Op::ForIter(s, _, _) | Op::ForLoop(s, _, _) => s as u64 + 2 < num_locals as u64,
            _ => true,
        };
        if !ok {
            return Err(err(i, &format!("{:?}: wrong kind of operand", op)));
        }
    }
    // stack depth along every path (must agree where paths meet)
    let mut depth: Vec<Option<i32>> = vec![None; code.len()];
    let mut todo = vec![0usize];
    depth[0] = Some(0);
    let mut max = 0i32;
    while let Some(i) = todo.pop() {
        let d = depth[i].unwrap();
        let op = code[i];
        if d < op.pops() {
            return Err(err(i, &format!("{:?}: stack underflow", op)));
        }
        let after = d + op.stack_effect();
        max = max.max(after).max(d);
        let mut next: [(usize, i32); 2] = [(usize::MAX, 0); 2];
        match op {
            Op::Return | Op::Raise => {}
            Op::Jump(t) => next[0] = (t as usize, after),
            Op::JumpIfFalseOrPop(t) | Op::JumpIfTrueOrPop(t) => next = [(t as usize, d), (i + 1, after)],
            Op::SetupTry(t) => next = [(t as usize, d + 1), (i + 1, after)],
            _ => match op.jump_target() {
                Some(t) => next = [(t as usize, after), (i + 1, after)],
                None => next[0] = (i + 1, after),
            },
        }
        for (s, nd) in next {
            if s == usize::MAX {
                continue;
            }
            if s >= code.len() {
                return Err(err(i, "execution runs past the end of the code"));
            }
            match depth[s] {
                Some(old) if old != nd => {
                    return Err(err(s, &format!("stack depth {} here on one path, {} on another", old, nd)))
                }
                Some(_) => {}
                None => {
                    depth[s] = Some(nd);
                    todo.push(s);
                }
            }
        }
    }
    if max > 1 << 16 {
        return Err(bad(&format!("{}: stack too deep", name)));
    }
    Ok(max as u32)
}

impl Vm {
    /// Loads a bytecode file into `unit`; returns its top-level function.
    pub fn load_bytecode(&mut self, data: &[u8], unit: u32) -> Result<Gc, String> {
        if data.len() < 8 || &data[..6] != MAGIC {
            return Err(bad("not a Faston bytecode file"));
        }
        let version = u16::from_le_bytes([data[6], data[7]]);
        if version != VERSION {
            return Err(bad(&format!("version {} (this Faston reads version {})", version, VERSION)));
        }
        let mut r = Reader { data, pos: 8, syms: Vec::new(), globals: Vec::new(), unit };
        let ns = r.count(4)?;
        for _ in 0..ns {
            let s = r.str()?;
            r.syms.push(self.syms.intern(&s));
        }
        let ng = r.count(4)?;
        for _ in 0..ng {
            let s = r.str()?;
            let slot = self.units[unit as usize].gtable.slot(&s);
            r.globals.push(slot);
        }
        let u = &mut self.units[unit as usize];
        let n = u.gtable.names.len();
        u.globals.resize(n, Value::Undef);
        u.gtable.is_const.resize(n, false);
        let script = r.function(self, None, 0)?;
        if r.pos != data.len() {
            return Err(bad("extra data after the module"));
        }
        if self.jit.as_ref().is_some_and(|j| j.eager) {
            self.compile_native(script, true);
        }
        Ok(script)
    }

    /// Runs a bytecode file as the main module.
    pub fn run_bytecode_file(&mut self, path: &str, data: &[u8]) -> Result<(), String> {
        let p = std::path::Path::new(path);
        let unit = &mut self.units[self.main as usize];
        unit.name = p.file_name().map_or(path.to_string(), |n| n.to_string_lossy().into_owned());
        unit.dir = Some(match p.parent() {
            Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
            _ => PathBuf::from("."),
        });
        let script = self.load_bytecode(data, self.main)?;
        let r = self.execute(script);
        self.flush_all();
        r.map(|_| ())
    }

    /// `faston --dis` listing of a bytecode file.
    pub fn disassemble_bytecode(&mut self, data: &[u8]) -> Result<String, String> {
        let script = self.load_bytecode(data, self.main)?;
        Ok(self.listing(script))
    }
}

// ---------------------------------------------------------------------------- the `bytecode` module

fn want_bytes<'a>(v: Value, what: &str) -> Result<&'a [u8], String> {
    v.as_bytes().ok_or_else(|| format!("TypeError: {} must be bytes, not '{}'", what, v.type_name()))
}

fn name_arg(args: &[Value], i: usize, default: &str) -> Result<String, String> {
    match args.get(i) {
        None => Ok(default.to_string()),
        Some(v) => v
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("TypeError: name must be a str, not '{}'", v.type_name())),
    }
}

fn nargs(what: &str, args: &[Value], min: usize, max: usize) -> Result<(), String> {
    if args.len() < min || args.len() > max {
        return Err(format!("TypeError: bytecode.{}() takes {} to {} arguments ({} given)", what, min, max, args.len()));
    }
    Ok(())
}

/// `bytecode.compile(source, name="<string>")`: source -> bytecode file contents.
fn bc_compile(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    nargs("compile", args, 1, 2)?;
    let src = args[0]
        .as_str()
        .ok_or_else(|| format!("TypeError: source must be a str, not '{}'", args[0].type_name()))?
        .to_string();
    let name = name_arg(args, 1, "<string>")?;
    let b = vm.compile_to_bytecode(&name, &src)?;
    Ok(vm.heap.bytes(b))
}

/// `bytecode.load(data, name="<bytecode>")`: a function that runs the module's
/// top-level code (in its own globals) when called.
fn bc_load(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    nargs("load", args, 1, 2)?;
    let data = want_bytes(args[0], "data")?.to_vec();
    let name = name_arg(args, 1, "<bytecode>")?;
    let unit = vm.new_unit(name, None);
    Ok(Value::Obj(vm.load_bytecode(&data, unit)?))
}

/// `bytecode.run(data, name="<bytecode>")`: loads and runs a module; returns it.
fn bc_run(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    nargs("run", args, 1, 2)?;
    let data = want_bytes(args[0], "data")?.to_vec();
    let name = name_arg(args, 1, "<bytecode>")?;
    let unit = vm.new_unit(name.clone(), None);
    let script = vm.load_bytecode(&data, unit)?;
    let module = Value::Obj(vm.heap.alloc(Obj::Module(ModuleRef { unit, name })));
    vm.call(Value::Obj(script), &[])?;
    Ok(module)
}

/// `bytecode.dis(data)`: a readable listing.
fn bc_dis(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    nargs("dis", args, 1, 1)?;
    let data = want_bytes(args[0], "data")?.to_vec();
    let unit = vm.new_unit("<dis>".into(), None);
    let script = vm.load_bytecode(&data, unit)?;
    let s = vm.listing(script);
    Ok(vm.heap.string(s))
}

pub fn register(vm: &mut Vm) {
    let mut opcodes = Dict::default();
    let mut operands = Dict::default();
    for (i, (name, kinds)) in OPCODES.iter().enumerate() {
        let k = vm.heap.string(*name);
        let kv = vm.heap.string(*kinds);
        opcodes.insert(to_key(k).unwrap(), k, Value::Int(i as i64));
        operands.insert(to_key(k).unwrap(), k, kv);
    }
    let members = vec![
        ("compile", vm.native("compile", bc_compile)),
        ("load", vm.native("load", bc_load)),
        ("run", vm.native("run", bc_run)),
        ("dis", vm.native("dis", bc_dis)),
        ("opcodes", vm.heap.dict(opcodes)),
        ("operands", vm.heap.dict(operands)),
        ("VERSION", Value::Int(VERSION as i64)),
        ("MAGIC", vm.heap.bytes(MAGIC.to_vec())),
    ];
    vm.define_module("bytecode", members);
}
