//! The VM.
//!
//! One contiguous value stack holds every frame as `[locals.. | temporaries..]`.
//! A call's arguments are already sitting where the callee's first locals go, so
//! calling never copies arguments. The garbage collector only runs at safe points
//! (calls and loop back-edges) where every live value is on this stack, in a
//! global, or reachable from a frame's function.
//!
//! Each source file is a compilation *unit* (module) with its own globals; a frame
//! caches a pointer to its unit's globals. `run` is re-entrant: built-ins can call
//! back into Faston code with `Vm::call`.

use std::cmp::Ordering;
use std::fmt::Write as _;
use std::collections::HashMap;
use std::io::{self, BufWriter, IsTerminal, Stdout, Write};
use std::path::PathBuf;
use std::time::Instant;

use crate::bytecode::Op;
use crate::compiler::{Compiler, Globals};
use crate::lexer;
use crate::ops;
use crate::value::*;

#[path = "jit_rt.rs"]
pub mod jit_rt;
#[cfg(feature = "jit")]
#[path = "jit.rs"]
pub mod jit;

/// Without the code generator: the same type (so `Vm` keeps one layout, which
/// ahead-of-time compiled code relies on), doing nothing.
#[cfg(not(feature = "jit"))]
pub mod jit {
    pub struct Jit {
        pub compiled: usize,
        pub interpreted: usize,
        pub eager: bool,
    }
    impl Jit {
        pub fn compile(&mut self, _vm: &mut super::Vm, _funcs: &[crate::value::Gc]) {}
    }
}

pub(crate) const STACK_SLOTS: usize = 1 << 20;
pub(crate) const MAX_FRAMES: usize = 10_000;

/// The value stack, allocated zeroed so the OS can hand out pages lazily
/// (all-zero bytes are `Value::Undef`, the enum's first variant).
fn zeroed_stack() -> Vec<Value> {
    let layout = std::alloc::Layout::array::<Value>(STACK_SLOTS).unwrap();
    unsafe {
        let ptr = std::alloc::alloc_zeroed(layout) as *mut Value;
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        Vec::from_raw_parts(ptr, STACK_SLOTS, STACK_SLOTS)
    }
}

/// `repr(C)`: compiled code reads and writes frames directly.
#[repr(C)]
pub(crate) struct Frame {
    /// The value that was called: the function itself, or its closure.
    owner: Gc,
    code: *const Op,
    consts: *const Value,
    /// Next instruction to execute (saved while a callee runs).
    ip: *const Op,
    base: usize,
    /// Stack slot that receives the return value.
    ret: usize,
    /// Captured variables of the running closure (null for plain functions).
    upvals: *const Gc,
    /// Globals of the function's module.
    globals: *mut Value,
}

#[derive(Clone, Copy, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    FloorDiv,
    Mod,
    Pow,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

impl BinOp {
    /// (method on the left operand, reflected method on the right operand)
    fn dunders(self) -> (Option<u32>, Option<u32>) {
        match self {
            BinOp::Add => (Some(sym::D_ADD), Some(sym::D_RADD)),
            BinOp::Sub => (Some(sym::D_SUB), Some(sym::D_RSUB)),
            BinOp::Mul => (Some(sym::D_MUL), Some(sym::D_RMUL)),
            BinOp::Div => (Some(sym::D_TRUEDIV), Some(sym::D_RTRUEDIV)),
            BinOp::FloorDiv => (Some(sym::D_FLOORDIV), Some(sym::D_RFLOORDIV)),
            BinOp::Mod => (Some(sym::D_MOD), Some(sym::D_RMOD)),
            BinOp::Pow => (Some(sym::D_POW), Some(sym::D_RPOW)),
            BinOp::BitAnd => (Some(sym::D_AND), Some(sym::D_RAND)),
            BinOp::BitOr => (Some(sym::D_OR), Some(sym::D_ROR)),
            BinOp::BitXor => (Some(sym::D_XOR), Some(sym::D_RXOR)),
            BinOp::Shl | BinOp::Shr => (None, None),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// The class method `name` of `v`, if `v` is an instance whose class defines it.
#[inline]
fn dunder(v: Value, name: u32) -> Option<Value> {
    match v {
        Value::Obj(g) => match g.get() {
            Obj::Instance(i) => i.class.as_class().method(name),
            _ => None,
        },
        _ => None,
    }
}

/// `a is b`: the same object (or the same immediate value).
pub fn same_object(a: Value, b: Value) -> bool {
    match (a, b) {
        (Value::None, Value::None) | (Value::Undef, Value::Undef) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x.to_bits() == y.to_bits(),
        (Value::Obj(x), Value::Obj(y)) => x == y,
        _ => false,
    }
}

fn slice_bound(v: Value) -> Result<Option<i64>, String> {
    match v {
        Value::None => Ok(None),
        Value::Int(i) => Ok(Some(i)),
        Value::Bool(b) => Ok(Some(b as i64)),
        _ => Err("TypeError: slice indices must be integers or None".into()),
    }
}

/// `list[a:b:c] = items` (Some) or `del list[a:b:c]` (None), with Python's rules.
fn slice_assign(obj: Value, start: Value, stop: Value, step: Value, items: Option<Vec<Value>>) -> Result<(), String> {
    let l = match obj {
        Value::Obj(g) => match g.get_mut() {
            Obj::List(l) => l,
            _ => {
                return Err(format!(
                    "TypeError: '{}' object does not support slice {}",
                    obj.type_name(),
                    if items.is_some() { "assignment" } else { "deletion" }
                ))
            }
        },
        _ => return Err(format!("TypeError: '{}' object is not subscriptable", obj.type_name())),
    };
    let len = l.len() as i64;
    let step = slice_bound(step)?.unwrap_or(1);
    if step == 0 {
        return Err("ValueError: slice step cannot be zero".into());
    }
    let (lower, upper) = if step < 0 { (-1, len - 1) } else { (0, len) };
    let norm = |v: Option<i64>, default: i64| match v {
        None => default,
        Some(i) if i < 0 => (i + len).max(lower),
        Some(i) => i.min(upper),
    };
    let start = norm(slice_bound(start)?, if step < 0 { upper } else { lower });
    let stop = norm(slice_bound(stop)?, if step < 0 { lower } else { upper });
    if step == 1 {
        let (a, b) = (start as usize, stop.max(start) as usize);
        match items {
            Some(new) => {
                l.splice(a..b, new);
            }
            None => {
                l.drain(a..b);
            }
        }
        return Ok(());
    }
    let mut idxs = Vec::new();
    let mut i = start;
    while (step > 0 && i < stop) || (step < 0 && i > stop) {
        idxs.push(i as usize);
        i += step;
    }
    match items {
        Some(new) => {
            if new.len() != idxs.len() {
                return Err(format!(
                    "ValueError: attempt to assign sequence of size {} to extended slice of size {}",
                    new.len(),
                    idxs.len()
                ));
            }
            for (k, &i) in idxs.iter().enumerate() {
                l[i] = new[k];
            }
        }
        None => {
            idxs.sort_unstable();
            for &i in idxs.iter().rev() {
                l.remove(i);
            }
        }
    }
    Ok(())
}

#[inline(always)]
fn is_instance(v: &Value) -> bool {
    matches!(v, Value::Obj(g) if matches!(g.get(), Obj::Instance(_)))
}

/// An active `try`: where to resume when an exception reaches it.
struct Handler {
    /// `frames.len()` when the try started (its frame is the last of those).
    frames: usize,
    /// Stack height to restore.
    sp: usize,
    /// Code index of the handler in that frame's function.
    target: u32,
}

/// Built-in exception classes, defined in Faston so programs can subclass them.
const PRELUDE: &str = r#"
class BaseException {
    def init(self, message = "") { self.message = message; }
    def __str__(self) { return str(self.message); }
    def __repr__(self) { return type(self) + "(" + repr(self.message) + ")"; }
}
class Exception(BaseException) { pass; }
class ArithmeticError(Exception) { pass; }
class ZeroDivisionError(ArithmeticError) { pass; }
class OverflowError(ArithmeticError) { pass; }
class LookupError(Exception) { pass; }
class IndexError(LookupError) { pass; }
class KeyError(LookupError) { pass; }
class TypeError(Exception) { pass; }
class ValueError(Exception) { pass; }
class NameError(Exception) { pass; }
class AttributeError(Exception) { pass; }
class RuntimeError(Exception) { pass; }
class RecursionError(RuntimeError) { pass; }
class NotImplementedError(RuntimeError) { pass; }
class AssertionError(Exception) { pass; }
class ImportError(Exception) { pass; }
class OSError(Exception) { pass; }
class IOError(OSError) { pass; }
class FileNotFoundError(OSError) { pass; }
class FileExistsError(OSError) { pass; }
class PermissionError(OSError) { pass; }
class IsADirectoryError(OSError) { pass; }
class NotADirectoryError(OSError) { pass; }
class TimeoutError(OSError) { pass; }
class EOFError(Exception) { pass; }
class ModuleNotFoundError(ImportError) { pass; }
class UnicodeError(ValueError) { pass; }
class JSONDecodeError(ValueError) { pass; }
class SyntaxError(Exception) { pass; }
class StopIteration(Exception) { pass; }
class KeyboardInterrupt(BaseException) { pass; }
class GeneratorExit(BaseException) { pass; }
class property {
    def init(self, fget = None, fset = None, fdel = None, doc = None) {
        self.fget = fget; self.fset = fset; self.fdel = fdel; self.__doc__ = doc;
    }
    def getter(self, f) { return property(f, self.fset, self.fdel, self.__doc__); }
    def setter(self, f) { return property(self.fget, f, self.fdel, self.__doc__); }
    def deleter(self, f) { return property(self.fget, self.fset, f, self.__doc__); }
}
class staticmethod {
    def init(self, f) { self.__func__ = f; }
    def __call__(self, *args, **kwargs) { return self.__func__(*args, **kwargs); }
}
class classmethod {
    def init(self, f) { self.__func__ = f; }
}
"#;

impl Frame {
    /// The function running in this frame.
    fn func(&self) -> Gc {
        match self.owner.get() {
            Obj::Closure(c) => c.func,
            _ => self.owner,
        }
    }
}

/// One source file / module: its own global variables.
pub struct Unit {
    /// Display name for tracebacks, e.g. "game.fst" or "<stdin>".
    pub name: String,
    /// Directory that `import` searches for sibling modules.
    pub dir: Option<PathBuf>,
    pub gtable: Globals,
    pub globals: Vec<Value>,
    /// Symbol -> global slot cache for `module.attr` lookups.
    attr_slots: crate::fx::FxMap<u32, u32>,
    /// A package (directory): `dir` holds its submodules.
    pub package: bool,
    /// Submodules imported so far (`game.enemies` -> `enemies`).
    children: crate::fx::FxMap<u32, Value>,
}

pub struct Vm {
    /// Built-in modules available to `import`, by symbol.
    pub modules: crate::fx::FxMap<u32, Value>,
    /// Modules loaded from files, by canonical path.
    loaded: HashMap<PathBuf, Value>,
    /// xorshift state for the `random` module.
    pub rng: u64,
    stack: Vec<Value>,
    sp: usize,
    frames: Vec<Frame>,
    /// Captured variables that still live on the stack, sorted by slot.
    open_upvals: Vec<Gc>,
    handlers: Vec<Handler>,
    /// Keyword arguments passed to the native currently running.
    kwargs: Vec<(u32, Value)>,
    /// Exception in flight, with the error string that carries it through Rust code.
    exc: Option<(Value, String)>,
    /// Call stack (function, line) captured where the in-flight exception was raised.
    trace: Vec<(Gc, u32)>,
    trace_exc: Option<Gc>,
    base_exception: Option<Gc>,
    /// Index of the main program's unit.
    pub main: u32,
    /// Unit 0 is the main program; imported files get their own.
    pub units: Vec<Box<Unit>>,
    /// Built-in names every new unit starts with.
    builtin_names: Globals,
    builtin_values: Vec<Value>,
    pub syms: Interner,
    pub heap: Heap,
    pub out: BufWriter<Stdout>,
    pub tty: bool,
    pub start: Instant,
    /// `stack.as_mut_ptr()` (the stack never reallocates), for compiled code.
    stack_ptr: *mut Value,
    /// Error raised inside compiled code, on its way out to Rust.
    jit_err: String,
    /// The native code generator, when running compiled (`faston build` / `--jit`).
    jit: Option<Box<jit::Jit>>,
    /// Machine code saved by `faston build`, attached to functions as they are compiled.
    aot: Option<Box<jit_rt::Aot>>,
    /// Functions given saved machine code.
    pub aot_count: usize,
    /// While building: the machine code of every module compiled so far.
    aot_out: Option<Vec<jit_rt::AotUnit>>,
    /// Calls go to native code (there is a code generator or saved code).
    native: bool,
    /// Register the real gui module (when compiled in).
    pub(crate) gui: bool,
    /// Source files bundled into a built executable, by module name.
    pub bundle: Option<HashMap<String, String>>,
    /// Files opened by the program, flushed when it ends (even without close()).
    pub open_files: Vec<std::rc::Weak<std::cell::RefCell<crate::fileio::Stream>>>,
    /// Command-line arguments for `sys.argv` (argv[0] is the script).
    pub argv: Vec<String>,
    /// Generators currently running, innermost last (`yield` suspends the top one).
    gen_stack: Vec<Gc>,
    /// Prelude classes the VM treats specially: property, staticmethod, classmethod.
    descriptors: Option<[Gc; 3]>,
}

impl Default for Vm {
    fn default() -> Self {
        Self::new()
    }
}

impl Vm {
    /// A VM that interprets bytecode.
    pub fn new() -> Vm {
        Self::with_mode(false)
    }

    /// A VM that compiles every function to native code before running it.
    pub fn compiled() -> Vm {
        Self::with_mode(true)
    }

    /// The VM `faston build` compiles a program with: one set up exactly like the
    /// runtime that will run it (with or without the gui module), so the machine
    /// code it saves matches what that runtime compiles.
    pub fn for_build(gui: bool) -> Vm {
        Self::with_options(true, gui)
    }

    fn with_mode(compiled: bool) -> Vm {
        Self::with_options(compiled, true)
    }

    fn with_options(compiled: bool, gui: bool) -> Vm {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15);
        let mut vm = Vm {
            modules: crate::fx::FxMap::default(),
            loaded: HashMap::new(),
            rng: seed | 1,
            stack: zeroed_stack(),
            sp: 0,
            // never reallocates: compiled code keeps pointers to frames
            frames: Vec::with_capacity(MAX_FRAMES + 16),
            open_upvals: Vec::new(),
            handlers: Vec::new(),
            kwargs: Vec::new(),
            exc: None,
            trace: Vec::new(),
            trace_exc: None,
            base_exception: None,
            main: 0,
            units: Vec::new(),
            builtin_names: Globals::default(),
            builtin_values: Vec::new(),
            syms: Interner::default(),
            heap: Heap::default(),
            out: BufWriter::with_capacity(1 << 16, io::stdout()),
            tty: io::stdout().is_terminal(),
            start: Instant::now(),
            stack_ptr: std::ptr::null_mut(),
            jit_err: String::new(),
            jit: None,
            aot: None,
            aot_count: 0,
            aot_out: None,
            native: false,
            gui,
            bundle: None,
            open_files: Vec::new(),
            argv: Vec::new(),
            gen_stack: Vec::new(),
            descriptors: None,
        };
        vm.stack_ptr = vm.stack.as_mut_ptr();
        #[cfg(feature = "jit")]
        if compiled {
            vm.jit = Some(Box::new(jit::Jit::new()));
        }
        #[cfg(not(feature = "jit"))]
        let _ = compiled;
        vm.native = vm.jit.is_some();
        crate::builtins::register(&mut vm);
        vm.load_prelude();
        vm.main = vm.new_unit("<stdin>".into(), std::env::current_dir().ok());
        vm
    }

    /// Runs the prelude and makes everything it defines a built-in.
    fn load_prelude(&mut self) {
        let unit = self.new_unit("<prelude>".into(), None);
        let script = self.compile_unit(unit, PRELUDE).expect("prelude compiles");
        self.call(Value::Obj(script), &[]).expect("prelude runs");
        let u = &self.units[unit as usize];
        let defs: Vec<(String, Value)> = u
            .gtable
            .names
            .iter()
            .zip(u.globals.iter())
            .filter(|(n, v)| !matches!(v, Value::Undef) && !self.builtin_names.index.contains_key(*n))
            .map(|(n, v)| (n.clone(), *v))
            .collect();
        for (name, v) in defs {
            let g = self.builtin_names.slot(&name) as usize;
            if self.builtin_values.len() <= g {
                self.builtin_values.resize(g + 1, Value::Undef);
            }
            self.builtin_values[g] = v;
        }
        self.base_exception = self.builtin_class("BaseException");
        if let (Some(p), Some(s), Some(c)) =
            (self.builtin_class("property"), self.builtin_class("staticmethod"), self.builtin_class("classmethod"))
        {
            self.descriptors = Some([p, s, c]);
        }
    }

    /// For a value found on a class: 0 = property, 1 = staticmethod, 2 = classmethod
    /// (with the wrapper object), or None for an ordinary method / attribute.
    fn descriptor(&self, v: Value) -> Option<(u8, Gc)> {
        let d = self.descriptors?;
        if let Value::Obj(g) = v {
            if let Obj::Instance(i) = g.get() {
                for (k, c) in d.iter().enumerate() {
                    if i.class == *c {
                        return Some((k as u8, g));
                    }
                }
            }
        }
        None
    }

    /// A field of a prelude object (None when unset).
    fn field_of(&mut self, g: Gc, name: &str) -> Value {
        let s = self.syms.intern(name);
        match g.get() {
            Obj::Instance(i) => match i.class.as_class().slots.get(&s).and_then(|&sl| i.fields.get(sl as usize)) {
                Some(Value::Undef) | None => Value::None,
                Some(&v) => v,
            },
            _ => Value::None,
        }
    }

    /// `obj.name` where class `cls` provides `m` (no instance field of that name):
    /// properties run their getter, static methods unwrap, class methods bind the
    /// class, and plain functions bind `obj`.
    fn class_member_for(&mut self, obj: Value, cls: Gc, name: u32, m: Value) -> Result<Value, String> {
        match self.descriptor(m) {
            Some((0, p)) => {
                let fget = self.field_of(p, "fget");
                if matches!(fget, Value::None) {
                    return Err(format!("AttributeError: property '{}' has no getter", self.syms.name(name)));
                }
                self.call(fget, &[obj])
            }
            Some((1, s)) => Ok(self.field_of(s, "__func__")),
            Some((_, c)) => {
                let f = self.field_of(c, "__func__");
                self.bind(Value::Obj(cls), f)
            }
            None => self.bind(obj, m),
        }
    }

    /// A bound method for bytecode functions; anything else as is.
    fn bind(&mut self, recv: Value, m: Value) -> Result<Value, String> {
        match m {
            Value::Obj(f) if f.as_callable().is_some() => {
                Ok(Value::Obj(self.heap.alloc(Obj::Bound(Bound { recv, method: f }))))
            }
            other => Ok(other),
        }
    }

    /// `Class.name`: static methods unwrap and class methods bind the class;
    /// properties and plain functions come back as they are.
    fn class_attr_on_class(&mut self, cls: Gc, m: Value) -> Result<Value, String> {
        match self.descriptor(m) {
            Some((1, s)) => Ok(self.field_of(s, "__func__")),
            Some((2, c)) => {
                let f = self.field_of(c, "__func__");
                self.bind(Value::Obj(cls), f)
            }
            _ => Ok(m),
        }
    }

    fn builtin_class(&self, name: &str) -> Option<Gc> {
        let g = *self.builtin_names.index.get(name)?;
        match self.builtin_values[g as usize] {
            Value::Obj(c) if matches!(c.get(), Obj::Class(_)) => Some(c),
            _ => None,
        }
    }

    /// Defines a built-in global (only valid before the first unit is created).
    pub fn define_native(&mut self, name: &'static str, func: NativeFn) {
        self.define_native_kw(name, func, &[]);
    }

    /// Defines a built-in that accepts the given keyword arguments.
    pub fn define_native_kw(&mut self, name: &'static str, func: NativeFn, kwargs: &'static [u32]) {
        let g = self.builtin_names.slot(name) as usize;
        if self.builtin_values.len() <= g {
            self.builtin_values.resize(g + 1, Value::Undef);
        }
        let n = self.heap.alloc(Obj::Native(Native { name, func, kwargs }));
        self.builtin_values[g] = Value::Obj(n);
    }

    fn new_unit(&mut self, name: String, dir: Option<PathBuf>) -> u32 {
        self.units.push(Box::new(Unit {
            name,
            dir,
            gtable: self.builtin_names.clone(),
            globals: self.builtin_values.clone(),
            attr_slots: Default::default(),
            package: false,
            children: Default::default(),
        }));
        (self.units.len() - 1) as u32
    }

    #[inline(always)]
    fn unit_globals(&mut self, unit: u32) -> *mut Value {
        unsafe { self.units.get_unchecked_mut(unit as usize).globals.as_mut_ptr() }
    }

    /// Compiles `src` into `unit` and returns its top-level function.
    fn compile_unit(&mut self, unit: u32, src: &str) -> Result<Gc, String> {
        let toks = lexer::lex(src)?;
        let u = &mut self.units[unit as usize];
        let script = Compiler::new(toks, &mut u.gtable, &mut self.heap, &mut self.syms, unit).compile()?;
        let n = u.gtable.names.len();
        u.globals.resize(n, Value::Undef);
        #[cfg(feature = "jit")]
        if self.aot_out.is_some() {
            // building: save machine code instead of installing it
            let funcs = jit_rt::unit_funcs(script);
            let mut jit = self.jit.take().unwrap();
            let saved = jit.compile_aot(self, &funcs);
            self.jit = Some(jit);
            let unit = jit_rt::AotUnit { src: jit_rt::hash_str(src), funcs: saved };
            self.aot_out.as_mut().unwrap().push(unit);
            return Ok(script);
        }
        if self.jit.as_ref().is_some_and(|j| j.eager) {
            self.compile_native(script, true);
        } else if self.aot.is_some() {
            self.install_aot(src, script);
        }
        Ok(script)
    }

    /// Compiles `script` (and, if `nested`, every function nested in it) to
    /// machine code. Normally functions are compiled when first called.
    fn compile_native(&mut self, script: Gc, nested: bool) {
        let mut todo = vec![script];
        let mut funcs = Vec::new();
        while let Some(g) = todo.pop() {
            if nested {
                for c in g.as_func().consts.iter() {
                    if let Value::Obj(cg) = c {
                        if let Obj::Func(_) = cg.get() {
                            todo.push(*cg);
                        }
                    }
                }
            }
            funcs.push(g);
        }
        let mut jit = self.jit.take().unwrap();
        jit.compile(self, &funcs);
        self.jit = Some(jit);
    }

    /// Runs a program file as the main module.
    pub fn run_file(&mut self, path: &str, src: &str) -> Result<(), String> {
        let p = std::path::Path::new(path);
        let unit = &mut self.units[self.main as usize];
        unit.name = p.file_name().map_or(path.to_string(), |n| n.to_string_lossy().into_owned());
        unit.dir = Some(match p.parent() {
            Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
            _ => PathBuf::from("."),
        });
        self.run_source(src)
    }

    /// Compiles a program and its bundled modules without running anything
    /// (`faston build` reports mistakes before producing an executable).
    pub fn check_program(&mut self, path: &str, src: &str) -> Result<(), String> {
        let p = std::path::Path::new(path);
        let unit = &mut self.units[self.main as usize];
        unit.name = p.file_name().map_or(path.to_string(), |n| n.to_string_lossy().into_owned());
        if let Some(j) = self.jit.as_mut() {
            j.eager = true;
            j.compiled = 0;
            j.interpreted = 0;
        }
        self.compile_unit(self.main, src)?;
        let mut mods: Vec<(String, String)> = self.bundle.clone().unwrap_or_default().into_iter().collect();
        mods.sort();
        for (file, msrc) in mods {
            if file.starts_with('\0') {
                continue; // metadata (sys.path)
            }
            let u = self.new_unit(file.clone(), None);
            self.compile_unit(u, &msrc).map_err(|e| format!("File \"{}\": {}", file, e))?;
        }
        Ok(())
    }

    /// `faston build`: compiles the program (and the standard-library modules it
    /// imports) like `check_program` and returns the machine code to save.
    #[cfg(feature = "jit")]
    pub fn build_native(&mut self, path: &str, src: &str, stdlib: &[String]) -> Result<Vec<u8>, String> {
        self.aot_out = Some(Vec::new());
        let r = self.check_program(path, src).and_then(|_| {
            for name in stdlib {
                if let Some(s) = crate::stdlib::source(name) {
                    let u = self.new_unit(name.clone(), None);
                    self.compile_unit(u, s)?;
                }
            }
            Ok(())
        });
        let units = self.aot_out.take().unwrap_or_default();
        r?;
        Ok(jit_rt::aot_encode(&units, &self.syms.names, self.env_hash()))
    }

    /// Takes the machine code `faston build` saved; it is attached to functions
    /// as their modules are compiled. False if it can't be used here (made by
    /// a different Faston, or for a runtime set up differently).
    pub fn load_aot(&mut self, data: &[u8]) -> bool {
        let Some((aot, syms, env)) = jit_rt::Aot::load(data) else { return false };
        if env != self.env_hash() || !syms.starts_with(&self.syms.names) {
            return false;
        }
        // symbols get the numbers they had when the code was compiled, whatever
        // order modules are imported in
        for s in &syms[self.syms.names.len()..] {
            self.syms.intern(s);
        }
        self.aot = Some(Box::new(aot));
        self.native = true;
        true
    }

    /// Identifies the built-ins (whose global slots compiled code uses).
    fn env_hash(&self) -> u64 {
        jit_rt::hash_str(&self.builtin_names.names.join("\n"))
    }

    /// Compiles and runs a piece of source in the main module (globals persist between
    /// calls, which is what the REPL relies on).
    pub fn run_source(&mut self, src: &str) -> Result<(), String> {
        let script = self.compile_unit(self.main, src)?;
        let r = self.execute(script);
        self.flush_all();
        r.map(|_| ())
    }

    /// Flushes stdout and every file the program still has open.
    pub fn flush_all(&mut self) {
        let _ = self.out.flush();
        self.open_files.retain(|w| match w.upgrade() {
            Some(s) => {
                crate::fileio::flush_stream(&mut s.borrow_mut());
                true
            }
            None => false,
        });
    }

    /// Compiles `src` and returns a human-readable bytecode listing (`faston --dis`).
    pub fn disassemble(&mut self, src: &str) -> Result<String, String> {
        let script = self.compile_unit(self.main, src)?;
        let mut out = String::new();
        let mut todo = vec![script];
        while let Some(g) = todo.pop() {
            let f = g.as_func();
            let _ = writeln!(
                out,
                "== {} (arity {}, locals {}, stack {}) ==",
                f.name, f.arity, f.num_locals, f.max_stack
            );
            for (i, op) in f.code.iter().enumerate() {
                let _ = writeln!(out, "{:5} {:4}  {:?}", i, f.lines[i], op);
            }
            for c in f.consts.iter().rev() {
                if let Value::Obj(cg) = c {
                    if let Obj::Func(_) = cg.get() {
                        todo.push(*cg);
                    }
                }
            }
            out.push('\n');
        }
        Ok(out)
    }

    fn execute(&mut self, script: Gc) -> Result<Value, String> {
        self.sp = 0;
        self.frames.clear();
        self.handlers.clear();
        let r = match self.push_frame(script, 0, 0, 0) {
            Ok(()) => self.run(),
            Err(e) => Err(e),
        };
        let r = r.map_err(|e| self.traceback(e));
        // Closures that escaped (e.g. into globals) keep working after this run.
        self.close_upvalues(0);
        self.sp = 0;
        self.frames.clear();
        self.handlers.clear();
        self.exc = None;
        self.trace.clear();
        self.trace_exc = None;
        r
    }

    // ------------------------------------------------------------------ user operators
    //
    // These run only when an operand isn't a plain number (the fast paths in `run`
    // handle those). The `*_at` variants are called from the dispatch loop: they first
    // publish the loop's `sp`/`ip` so a Faston callback sees a consistent stack (and
    // the GC sees the operands, which are still below `sp`).

    #[inline(always)]
    fn sync(&mut self, sp: usize, ip: *const Op) {
        self.sp = sp;
        self.frames.last_mut().unwrap().ip = ip;
    }

    /// `a OP b` for anything that isn't int/float arithmetic.
    pub fn binop(&mut self, op: BinOp, a: Value, b: Value) -> Result<Value, String> {
        if is_instance(&a) || is_instance(&b) {
            let (fwd, rev) = op.dunders();
            if let Some(m) = fwd.and_then(|n| dunder(a, n)) {
                return self.call(m, &[a, b]);
            }
            if let Some(m) = rev.and_then(|n| dunder(b, n)) {
                return self.call(m, &[b, a]);
            }
        }
        let h = &mut self.heap;
        match op {
            BinOp::Add => ops::add(h, a, b),
            BinOp::Sub => ops::sub(h, a, b),
            BinOp::Mul => ops::mul(h, a, b),
            BinOp::Div => ops::div(h, a, b),
            BinOp::FloorDiv => ops::floordiv(h, a, b),
            BinOp::Mod => ops::modulo(h, a, b),
            BinOp::Pow => ops::pow(h, a, b),
            BinOp::BitAnd => ops::bitwise(h, a, b, '&'),
            BinOp::BitOr => ops::bitwise(h, a, b, '|'),
            BinOp::BitXor => ops::bitwise(h, a, b, '^'),
            BinOp::Shl => ops::shift(a, b, true),
            BinOp::Shr => ops::shift(a, b, false),
        }
    }

    #[inline(never)]
    fn binop_at(&mut self, op: BinOp, a: Value, b: Value, sp: usize, ip: *const Op) -> Result<Value, String> {
        self.sync(sp, ip);
        self.binop(op, a, b)
    }

    /// Comparison with `__eq__`/`__lt__`/... support (and reflected fallbacks).
    pub fn compare(&mut self, op: CmpOp, a: Value, b: Value) -> Result<bool, String> {
        if is_instance(&a) || is_instance(&b) {
            let (fwd, rev) = match op {
                CmpOp::Eq => (sym::D_EQ, sym::D_EQ),
                CmpOp::Ne => (sym::D_NE, sym::D_NE),
                CmpOp::Lt => (sym::D_LT, sym::D_GT),
                CmpOp::Le => (sym::D_LE, sym::D_GE),
                CmpOp::Gt => (sym::D_GT, sym::D_LT),
                CmpOp::Ge => (sym::D_GE, sym::D_LE),
            };
            if let Some(m) = dunder(a, fwd) {
                return Ok(self.call(m, &[a, b])?.truthy());
            }
            if let Some(m) = dunder(b, rev) {
                return Ok(self.call(m, &[b, a])?.truthy());
            }
            if op == CmpOp::Ne && (dunder(a, sym::D_EQ).is_some() || dunder(b, sym::D_EQ).is_some()) {
                return Ok(!self.compare(CmpOp::Eq, a, b)?);
            }
        }
        let sym = match op {
            CmpOp::Eq => return Ok(ops::equals(a, b)),
            CmpOp::Ne => return Ok(!ops::equals(a, b)),
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
        };
        Ok(match ops::compare(a, b, sym)? {
            Some(o) => match op {
                CmpOp::Lt => o == Ordering::Less,
                CmpOp::Le => o != Ordering::Greater,
                CmpOp::Gt => o == Ordering::Greater,
                _ => o != Ordering::Less,
            },
            None => false,
        })
    }

    #[inline(never)]
    fn compare_at(&mut self, op: CmpOp, a: Value, b: Value, sp: usize, ip: *const Op) -> Result<bool, String> {
        self.sync(sp, ip);
        self.compare(op, a, b)
    }

    /// Truthiness, honouring `__bool__` and `__len__`.
    pub fn truthy(&mut self, v: Value) -> Result<bool, String> {
        if let Some(m) = dunder(v, sym::D_BOOL) {
            return Ok(self.call(m, &[v])?.truthy());
        }
        if let Some(m) = dunder(v, sym::D_LEN) {
            return Ok(self.call(m, &[v])?.truthy());
        }
        Ok(v.truthy())
    }

    #[inline(never)]
    fn truthy_at(&mut self, v: Value, sp: usize, ip: *const Op) -> Result<bool, String> {
        self.sync(sp, ip);
        self.truthy(v)
    }

    #[inline(never)]
    fn neg_at(&mut self, v: Value, sp: usize, ip: *const Op) -> Result<Value, String> {
        self.sync(sp, ip);
        match dunder(v, sym::D_NEG) {
            Some(m) => self.call(m, &[v]),
            None => ops::neg(v),
        }
    }

    pub fn get_item(&mut self, obj: Value, idx: Value) -> Result<Value, String> {
        match dunder(obj, sym::D_GETITEM) {
            Some(m) => self.call(m, &[obj, idx]),
            None => {
                // native handles with a __getitem__ method (re.Match: m[1])
                if let Value::Obj(g) = obj {
                    if let Obj::Handle(h) = g.get() {
                        let method = h.ty.method;
                        if let Some(r) = method(self, g, "__getitem__", &[idx]) {
                            return r;
                        }
                    }
                }
                ops::get_index(&mut self.heap, obj, idx)
            }
        }
    }

    #[inline(never)]
    fn get_item_at(&mut self, obj: Value, idx: Value, sp: usize, ip: *const Op) -> Result<Value, String> {
        self.sync(sp, ip);
        self.get_item(obj, idx)
    }

    /// `item in container` with `__contains__` / `__eq__` support.
    pub fn contains(&mut self, container: Value, item: Value) -> Result<bool, String> {
        if let Some(m) = dunder(container, sym::D_CONTAINS) {
            return Ok(self.call(m, &[container, item])?.truthy());
        }
        if let Value::Obj(g) = container {
            if matches!(g.get(), Obj::Generator(_)) {
                // consumes items up to the first match, like Python
                while let Some(x) = self.resume_gen(g, Value::None)? {
                    if self.compare(CmpOp::Eq, item, x)? {
                        return Ok(true);
                    }
                }
                return Ok(false);
            }
        }
        if dunder(item, sym::D_EQ).is_some() {
            if let Value::Obj(g) = container {
                let items: Option<Vec<Value>> = match g.get() {
                    Obj::List(l) => Some(l.clone()),
                    Obj::Tuple(t) => Some(t.to_vec()),
                    _ => None,
                };
                if let Some(items) = items {
                    for x in items {
                        if self.compare(CmpOp::Eq, item, x)? {
                            return Ok(true);
                        }
                    }
                    return Ok(false);
                }
            }
        }
        ops::contains(container, item)
    }

    /// `str(v)` using `__str__` (falling back to `__repr__`) for instances, and
    /// element `__repr__`s inside containers.
    pub fn str_of(&mut self, v: Value) -> Result<String, String> {
        if let Some(s) = v.as_str() {
            return Ok(s.to_string());
        }
        let mut out = String::new();
        self.display(v, false, &mut out, 0)?;
        Ok(out)
    }

    pub fn repr_of(&mut self, v: Value) -> Result<String, String> {
        let mut out = String::new();
        self.display(v, true, &mut out, 0)?;
        Ok(out)
    }

    pub fn display(&mut self, v: Value, repr: bool, out: &mut String, depth: usize) -> Result<(), String> {
        if depth > 64 {
            out.push_str("...");
            return Ok(());
        }
        let g = match v {
            Value::Obj(g) => g,
            Value::Int(i) => {
                push_int(out, i);
                return Ok(());
            }
            _ => {
                v.write_repr(out, depth);
                return Ok(());
            }
        };
        let sep = |out: &mut String, i: usize| {
            if i > 0 {
                out.push_str(", ");
            }
        };
        match g.get() {
            Obj::Str(s) if !repr => out.push_str(s),
            Obj::Instance(inst) => {
                let cls = inst.class.as_class();
                let m = if repr {
                    cls.method(sym::D_REPR)
                } else {
                    cls.method(sym::D_STR).or_else(|| cls.method(sym::D_REPR))
                };
                match m {
                    Some(m) => {
                        let r = self.call(m, &[v])?;
                        match r.as_str() {
                            Some(s) => out.push_str(s),
                            None => {
                                return Err(format!(
                                    "TypeError: {} returned non-string (type {})",
                                    if repr { "__repr__" } else { "__str__" },
                                    r.type_name()
                                ))
                            }
                        }
                    }
                    None => v.write_repr(out, depth),
                }
            }
            Obj::List(l) => {
                let items = l.clone();
                out.push('[');
                for (i, x) in items.iter().enumerate() {
                    sep(out, i);
                    self.display(*x, true, out, depth + 1)?;
                }
                out.push(']');
            }
            Obj::Tuple(t) => {
                let items = t.to_vec();
                out.push('(');
                for (i, x) in items.iter().enumerate() {
                    sep(out, i);
                    self.display(*x, true, out, depth + 1)?;
                }
                if items.len() == 1 {
                    out.push(',');
                }
                out.push(')');
            }
            Obj::Dict(d) => {
                let entries = d.entries.clone();
                out.push('{');
                for (i, (k, x)) in entries.iter().enumerate() {
                    sep(out, i);
                    self.display(*k, true, out, depth + 1)?;
                    out.push_str(": ");
                    self.display(*x, true, out, depth + 1)?;
                }
                out.push('}');
            }
            Obj::Set(d) if d.len() > 0 => {
                let keys: Vec<Value> = d.entries.iter().map(|(k, _)| *k).collect();
                out.push('{');
                for (i, k) in keys.iter().enumerate() {
                    sep(out, i);
                    self.display(*k, true, out, depth + 1)?;
                }
                out.push('}');
            }
            _ => v.write_repr(out, depth),
        }
        Ok(())
    }

    /// Items of any iterable, including instances with `__iter__`.
    pub fn to_vec(&mut self, v: Value) -> Result<Vec<Value>, String> {
        if let Value::Obj(g) = v {
            if matches!(g.get(), Obj::Generator(_)) {
                return self.drain_gen(g);
            }
        }
        if dunder(v, sym::D_NEXT).is_some() {
            let it = self.make_iter(v)?;
            return self.to_vec(it);
        }
        if let Some(m) = dunder(v, sym::D_ITER) {
            let it = self.call(m, &[v])?;
            if is_instance(&it) {
                if dunder(it, sym::D_NEXT).is_some() {
                    let it = self.make_iter(it)?;
                    return self.to_vec(it);
                }
                return Err("TypeError: __iter__ must return an iterable or an iterator (an object with __next__)".into());
            }
            return self.to_vec(it);
        }
        if let Some(items) = self.handle_iter(v)? {
            return ops::to_vec(&mut self.heap, items);
        }
        ops::to_vec(&mut self.heap, v)
    }

    /// For native handles that can be iterated (files: their lines), the items as a list.
    pub fn handle_iter(&mut self, v: Value) -> Result<Option<Value>, String> {
        if let Value::Obj(g) = v {
            if let Obj::Handle(h) = g.get() {
                let method = h.ty.method;
                return match method(self, g, "__iter__", &[]) {
                    Some(r) => r.map(Some),
                    None => Err(format!("TypeError: '{}' object is not iterable", v.type_name())),
                };
            }
        }
        Ok(None)
    }

    /// `getattr(obj, name)` for natives.
    pub fn get_attr(&mut self, obj: Value, name: u32) -> Result<Value, String> {
        self.load_attr(obj, name).map(|(v, _)| v)
    }

    /// `setattr(obj, name, v)` for natives.
    pub fn set_attr(&mut self, obj: Value, name: u32, v: Value) -> Result<(), String> {
        self.store_attr(obj, name, v).map(|_| ())
    }

    /// `len(v)`, honouring `__len__`.
    pub fn len_of(&mut self, v: Value) -> Result<Option<i64>, String> {
        if let Some(m) = dunder(v, sym::D_LEN) {
            return match self.call(m, &[v])? {
                Value::Int(n) if n >= 0 => Ok(Some(n)),
                r => Err(format!(
                    "TypeError: __len__ should return a non-negative int, not '{}'",
                    r.type_name()
                )),
            };
        }
        Ok(None)
    }

    // ------------------------------------------------------------------ exceptions

    /// Turns a Rust-side error like "KeyError: 'b'" into an exception instance.
    fn make_exception(&mut self, err: &str) -> Value {
        let is_ident = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_');
        let (name, msg) = match err.split_once(": ") {
            Some((n, m)) if is_ident(n) => (n, m),
            _ if is_ident(err) => (err, ""),
            _ if err.contains("SyntaxError") => ("SyntaxError", err),
            _ => ("Exception", err),
        };
        let (cls, msg) = match self.builtin_class(name) {
            Some(c) => (c, msg),
            None => (self.builtin_class("Exception").unwrap(), err),
        };
        let fields = vec![self.heap.string(msg)];
        let inst = Value::Obj(self.heap.alloc(Obj::Instance(Instance::new(cls, Vec::new()))));
        let _ = self.store_attr(inst, sym::MESSAGE, fields[0]);
        inst
    }

    fn is_subclass(mut cls: Option<Gc>, target: Gc) -> bool {
        while let Some(c) = cls {
            if c == target {
                return true;
            }
            cls = c.as_class().parent;
        }
        false
    }

    fn is_exception(&self, v: Value) -> bool {
        match (v, self.base_exception) {
            (Value::Obj(g), Some(base)) => match g.get() {
                Obj::Instance(i) => Self::is_subclass(Some(i.class), base),
                _ => false,
            },
            _ => false,
        }
    }

    /// "Name: message" (or just "Name"), using the exception's `__str__`.
    fn exception_message(&mut self, exc: Value) -> Result<String, String> {
        let name = exc.type_name();
        let text = self.str_of(exc)?;
        Ok(if text.is_empty() { name } else { format!("{}: {}", name, text) })
    }

    /// `raise v`: instantiates classes, checks the type, and starts unwinding.
    fn raise(&mut self) -> String {
        let mut v = self.stack[self.sp - 1];
        if let Value::Obj(g) = v {
            if matches!(g.get(), Obj::Class(_)) {
                match self.call(v, &[]) {
                    Ok(inst) => {
                        v = inst;
                        self.stack[self.sp - 1] = v;
                    }
                    Err(e) => return e,
                }
            }
        }
        if !self.is_exception(v) {
            self.sp -= 1;
            return format!(
                "TypeError: exceptions must derive from BaseException, not '{}'",
                v.type_name()
            );
        }
        let msg = match self.exception_message(v) {
            Ok(m) => m,
            Err(e) => return e,
        };
        self.sp -= 1;
        self.exc = Some((v, msg.clone()));
        msg
    }

    fn capture_trace(&mut self) {
        self.trace = self
            .frames
            .iter()
            .map(|fr| {
                let func = fr.func();
                let f = func.as_func();
                let idx = unsafe { fr.ip.offset_from(fr.code) } as usize;
                (func, f.lines.get(idx.saturating_sub(1)).copied().unwrap_or(0))
            })
            .collect();
    }

    /// Finds the innermost handler belonging to this `run` (frames above `stop`) and
    /// unwinds to it, pushing the exception. Returns the handler's code index.
    #[cold]
    #[inline(never)]
    fn handle_exception(&mut self, err: String, stop: usize) -> Result<u32, String> {
        let exc = match self.exc.take() {
            Some((v, m)) if m == err => v,
            _ => self.make_exception(&err),
        };
        let g = match exc {
            Value::Obj(g) => g,
            _ => unreachable!(),
        };
        if self.trace_exc != Some(g) {
            self.capture_trace();
            self.trace_exc = Some(g);
        }
        match self.handlers.last() {
            Some(h) if h.frames > stop => {
                let h = self.handlers.pop().unwrap();
                self.close_upvalues(h.sp);
                self.frames.truncate(h.frames);
                self.sp = h.sp;
                self.spush(exc);
                Ok(h.target)
            }
            _ => {
                self.exc = Some((exc, err.clone()));
                Err(err)
            }
        }
    }

    /// Calls a Faston value (function, closure, bound method, class, native) from Rust,
    /// e.g. from a built-in like `sorted(xs, key)`. Re-entrant.
    pub fn call(&mut self, f: Value, args: &[Value]) -> Result<Value, String> {
        let callee = self.sp;
        if callee + args.len() + 1 >= STACK_SLOTS {
            return Err("RecursionError: maximum recursion depth exceeded".into());
        }
        self.stack[callee] = f;
        for (i, a) in args.iter().enumerate() {
            self.stack[callee + 1 + i] = *a;
        }
        self.sp = callee + 1 + args.len();
        let depth = self.frames.len();
        let r = match self.call_value(callee, args.len()) {
            Ok(true) => self.run(),
            Ok(false) => Ok(self.stack[callee]),
            Err(e) => Err(e),
        };
        if r.is_err() {
            self.close_upvalues(callee);
            self.frames.truncate(depth);
            while self.handlers.last().map_or(false, |h| h.frames > depth) {
                self.handlers.pop();
            }
        }
        self.sp = callee;
        r
    }

    // ------------------------------------------------------------------ generators

    /// Runs generator `g` until its next `yield` (Some(value)) or until it finishes
    /// (None). `send` is the value the paused `yield` expression evaluates to.
    pub fn resume_gen(&mut self, g: Gc, send: Value) -> Result<Option<Value>, String> {
        let gen = match g.get_mut() {
            Obj::Generator(x) => x,
            _ => return Err("TypeError: not a generator".into()),
        };
        match gen.state {
            gen_state::DONE => return Ok(None),
            gen_state::RUNNING => return Err("ValueError: generator already executing".into()),
            _ => {}
        }
        let (owner, saved, resume_at, hs) = match &mut gen.kind {
            GenKind::Seq { items, idx } => {
                let next = match *items {
                    Value::Obj(o) => match o.get() {
                        Obj::List(l) => l.get(*idx).copied(),
                        Obj::Tuple(t) => t.get(*idx).copied(),
                        _ => None,
                    },
                    _ => None,
                };
                *idx += 1;
                if next.is_none() {
                    gen.state = gen_state::DONE;
                }
                return Ok(next);
            }
            GenKind::Next { obj } => {
                let obj = *obj;
                gen.state = gen_state::RUNNING;
                let r = match dunder(obj, sym::D_NEXT) {
                    Some(m) => self.call(m, &[obj]),
                    None => Err(format!("TypeError: '{}' object is not an iterator", obj.type_name())),
                };
                let gen = match g.get_mut() {
                    Obj::Generator(x) => x,
                    _ => unreachable!(),
                };
                return match r {
                    Ok(v) => {
                        gen.state = gen_state::SUSPENDED;
                        Ok(Some(v))
                    }
                    Err(e) if e.starts_with("StopIteration") => {
                        gen.state = gen_state::DONE;
                        self.exc = None;
                        Ok(None)
                    }
                    Err(e) => {
                        gen.state = gen_state::DONE;
                        Err(e)
                    }
                };
            }
            GenKind::Frame { owner, saved, ip, handlers } => {
                (*owner, std::mem::take(saved), *ip, std::mem::take(handlers))
            }
        };
        let was_suspended = gen.state == gen_state::SUSPENDED;
        if !was_suspended && !matches!(send, Value::None) {
            if let GenKind::Frame { saved: s, handlers: h, .. } = &mut gen.kind {
                *s = saved;
                *h = hs;
            }
            return Err("TypeError: can't send non-None value to a just-started generator".into());
        }
        let (fobj, upvals) = owner.as_callable().expect("generator owner is callable");
        let f = fobj.as_func();
        let base = self.sp;
        if self.frames.len() >= MAX_FRAMES || base + saved.len() + f.max_stack as usize + 2 >= STACK_SLOTS {
            if let GenKind::Frame { saved: s, handlers: h, .. } = &mut gen.kind {
                *s = saved;
                *h = hs;
            }
            return Err("RecursionError: maximum recursion depth exceeded".into());
        }
        for (i, v) in saved.iter().enumerate() {
            self.stack[base + i] = *v;
        }
        let mut sp = base + saved.len();
        if was_suspended {
            self.stack[sp] = send;
            sp += 1;
        }
        self.sp = sp;
        gen.state = gen_state::RUNNING;
        let globals = self.unit_globals(f.unit);
        self.frames.push(Frame {
            owner,
            code: f.code.as_ptr(),
            consts: f.consts.as_ptr(),
            ip: unsafe { f.code.as_ptr().add(resume_at) },
            base,
            ret: base,
            upvals,
            globals,
        });
        let depth = self.frames.len();
        for (off, target) in hs {
            self.handlers.push(Handler { frames: depth, sp: base + off, target });
        }
        self.gen_stack.push(g);
        let r = self.interp();
        self.gen_stack.pop();
        let gen = match g.get_mut() {
            Obj::Generator(x) => x,
            _ => unreachable!(),
        };
        let out = match r {
            Ok(v) if gen.state == gen_state::SUSPENDED => Ok(Some(v)),
            Ok(_) => {
                // returned: the generator is exhausted
                gen.state = gen_state::DONE;
                Ok(None)
            }
            Err(e) => {
                gen.state = gen_state::DONE;
                self.close_upvalues(base);
                self.frames.truncate(depth - 1);
                while self.handlers.last().is_some_and(|h| h.frames >= depth) {
                    self.handlers.pop();
                }
                Err(e)
            }
        };
        self.sp = base;
        out
    }

    /// An iterator over `v`: generators are their own iterators; objects with
    /// `__next__` are wrapped; anything else iterable becomes a sequence iterator.
    pub fn make_iter(&mut self, v: Value) -> Result<Value, String> {
        if let Value::Obj(g) = v {
            if matches!(g.get(), Obj::Generator(_)) {
                return Ok(v);
            }
        }
        if dunder(v, sym::D_NEXT).is_some() {
            return Ok(Value::Obj(self.heap.alloc(Obj::Generator(Box::new(Generator {
                kind: GenKind::Next { obj: v },
                state: gen_state::SUSPENDED,
            })))));
        }
        if let Some(m) = dunder(v, sym::D_ITER) {
            let it = self.call(m, &[v])?;
            if dunder(it, sym::D_NEXT).is_some() || matches!(it, Value::Obj(g) if matches!(g.get(), Obj::Generator(_))) {
                return self.make_iter(it);
            }
        }
        let items = self.to_vec(v)?;
        let list = self.heap.list(items);
        Ok(Value::Obj(self.heap.alloc(Obj::Generator(Box::new(Generator {
            kind: GenKind::Seq { items: list, idx: 0 },
            state: gen_state::SUSPENDED,
        })))))
    }

    /// Every remaining item of generator / iterator `g`.
    fn drain_gen(&mut self, g: Gc) -> Result<Vec<Value>, String> {
        let list = self.heap.list(Vec::new());
        let mark = self.temp_mark();
        self.push_temp(list)?;
        self.push_temp(Value::Obj(g))?;
        let r = loop {
            match self.resume_gen(g, Value::None) {
                Ok(Some(x)) => {
                    if let Value::Obj(l) = list {
                        if let Obj::List(items) = l.get_mut() {
                            items.push(x);
                        }
                    }
                }
                Ok(None) => break Ok(()),
                Err(e) => break Err(e),
            }
        };
        self.temp_reset(mark);
        r?;
        Ok(match list {
            Value::Obj(l) => match l.get() {
                Obj::List(items) => items.clone(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        })
    }

    /// Keeps `v` alive across `call`s by parking it on the VM stack (for built-ins).
    pub fn push_temp(&mut self, v: Value) -> Result<(), String> {
        if self.sp + 1 >= STACK_SLOTS {
            return Err("RecursionError: maximum recursion depth exceeded".into());
        }
        self.spush(v);
        Ok(())
    }

    pub fn temp_mark(&self) -> usize {
        self.sp
    }

    pub fn set_temp(&mut self, at: usize, v: Value) {
        self.stack[at] = v;
    }

    pub fn temp_reset(&mut self, mark: usize) {
        self.sp = mark;
    }

    fn capture_upvalue(&mut self, slot: usize) -> Gc {
        let mut i = self.open_upvals.len();
        while i > 0 {
            let g = self.open_upvals[i - 1];
            let s = g.as_upval_mut().slot;
            if s == slot {
                return g;
            }
            if s < slot {
                break;
            }
            i -= 1;
        }
        let ptr = unsafe { self.stack.as_mut_ptr().add(slot) };
        let g = self.heap.alloc(Obj::Upvalue(Upval {
            ptr,
            closed: Value::None,
            slot,
        }));
        self.open_upvals.insert(i, g);
        g
    }

    /// Moves every captured variable in stack slots >= `from` to the heap.
    #[inline]
    fn close_upvalues(&mut self, from: usize) {
        while let Some(&g) = self.open_upvals.last() {
            let u = g.as_upval_mut();
            if u.slot < from {
                break;
            }
            u.close();
            self.open_upvals.pop();
        }
    }

    #[inline(never)]
    fn collect_garbage(&mut self) {
        for i in 0..self.sp {
            self.heap.mark(self.stack[i]);
        }
        for u in self.units.iter() {
            for v in u.globals.iter().chain(u.children.values()) {
                self.heap.mark(*v);
            }
        }
        for v in self.builtin_values.iter() {
            self.heap.mark(*v);
        }
        for f in self.frames.iter() {
            self.heap.mark_gc(f.owner);
        }
        for u in self.open_upvals.iter() {
            self.heap.mark_gc(*u);
        }
        for m in self.modules.values().chain(self.loaded.values()) {
            self.heap.mark(*m);
        }
        if let Some((v, _)) = self.exc {
            self.heap.mark(v);
        }
        for (f, _) in self.trace.iter() {
            self.heap.mark_gc(*f);
        }
        if let Some(g) = self.trace_exc {
            self.heap.mark_gc(g);
        }
        self.heap.finish_collection();
    }

    #[cold]
    #[inline(never)]
    fn traceback(&self, msg: String) -> String {
        let lines: Vec<String> = self
            .trace
            .iter()
            .map(|&(func, line)| {
                let f = func.as_func();
                let file = self.units.get(f.unit as usize).map_or("?", |u| u.name.as_str());
                format!("  File \"{}\", line {}, in {}", file, line, f.name)
            })
            .collect();
        // Collapse runs of identical lines, like Python does for deep recursion.
        let mut s = String::from("Traceback (most recent call last):\n");
        let mut i = 0;
        while i < lines.len() {
            let mut j = i + 1;
            while j < lines.len() && lines[j] == lines[i] {
                j += 1;
            }
            let run = j - i;
            let shown = run.min(3);
            for _ in 0..shown {
                let _ = writeln!(s, "{}", lines[i]);
            }
            if run > shown {
                let _ = writeln!(s, "  [previous line repeated {} more times]", run - shown);
            }
            i = j;
        }
        s.push_str(&msg);
        s
    }

    #[cold]
    #[inline(never)]
    fn arity_error(f: &Function, nargs: usize) -> String {
        let min = f.arity as usize - f.defaults.len();
        let takes = if min == f.arity as usize {
            format!("{} positional argument{}", f.arity, if f.arity == 1 { "" } else { "s" })
        } else {
            format!("from {} to {} positional arguments", min, f.arity)
        };
        format!(
            "TypeError: {}() takes {} but {} {} given",
            f.name,
            takes,
            nargs,
            if nargs == 1 { "was" } else { "were" }
        )
    }

    /// Registers a built-in module made of named values (usually natives).
    pub fn define_module(&mut self, name: &str, members: Vec<(&str, Value)>) {
        let id = self.heap.new_class_id();
        let cls = self.heap.alloc(Obj::Class(Box::new(Class::new("module".into(), id))));
        let mut fields = Vec::new();
        for (member, v) in members {
            let sym = self.syms.intern(member);
            let slot = cls.as_class_mut().slot_for(sym, &mut self.heap) as usize;
            if fields.len() <= slot {
                fields.resize(slot + 1, Value::Undef);
            }
            fields[slot] = v;
        }
        let module = self.heap.alloc(Obj::Instance(Instance::new(cls, fields)));
        let sym = self.syms.intern(name);
        self.modules.insert(sym, Value::Obj(module));
    }

    /// A registered built-in module.
    pub fn module(&mut self, name: &str) -> Option<Value> {
        let s = self.syms.intern(name);
        self.modules.get(&s).copied()
    }

    /// Sets `sys.argv` (argv[0] is the script path).
    pub fn set_argv(&mut self, args: Vec<String>) {
        let vals: Vec<Value> = args.iter().map(|a| self.heap.string(a.clone())).collect();
        if let Some(sys) = self.module("sys") {
            let argv = self.syms.intern("argv");
            if let Ok(Value::Obj(g)) = self.get_attr(sys, argv) {
                if let Obj::List(l) = g.get_mut() {
                    *l = vals;
                }
            }
        }
        self.argv = args;
    }

    pub fn native(&mut self, name: &'static str, func: NativeFn) -> Value {
        Value::Obj(self.heap.alloc(Obj::Native(Native { name, func, kwargs: &[] })))
    }

    /// Enters function or closure `callee` with its `nargs` arguments already at
    /// `stack[base..]`.
    #[inline(always)]
    fn push_frame(&mut self, callee: Gc, base: usize, nargs: usize, ret: usize) -> Result<(), String> {
        self.push_frame_kw(callee, base, nargs, &[], ret)
    }

    /// Like `push_frame`, with keyword arguments (already copied off the stack).
    fn push_frame_kw(
        &mut self,
        callee: Gc,
        base: usize,
        npos: usize,
        kws: &[(u32, Value)],
        ret: usize,
    ) -> Result<(), String> {
        let (fobj, upvals) = match callee.as_callable() {
            Some(x) => x,
            None => return Err("TypeError: object is not callable".into()),
        };
        let f = fobj.as_func();
        let filled = if npos == f.fast_arity as usize && kws.is_empty() {
            npos
        } else {
            self.bind_args(callee, f, base, npos, kws)?
        };
        let top = base + f.num_locals as usize;
        if self.frames.len() >= MAX_FRAMES || top + f.max_stack as usize >= STACK_SLOTS {
            return Err("RecursionError: maximum recursion depth exceeded".into());
        }
        for i in base + filled..top {
            unsafe { *self.stack.get_unchecked_mut(i) = Value::None };
        }
        self.sp = top;
        let globals = self.unit_globals(f.unit);
        self.frames.push(Frame {
            owner: callee,
            code: f.code.as_ptr(),
            consts: f.consts.as_ptr(),
            ip: f.code.as_ptr(),
            base,
            ret,
            upvals,
            globals,
        });
        Ok(())
    }

    /// Arranges positional and keyword arguments into `f`'s parameter slots, filling
    /// defaults and building `*args` / `**kwargs`. Returns the number of slots set.
    #[inline(never)]
    fn bind_args(
        &mut self,
        callee: Gc,
        f: &Function,
        base: usize,
        npos: usize,
        kws: &[(u32, Value)],
    ) -> Result<usize, String> {
        let arity = f.arity as usize;
        let nslots = arity + f.varargs as usize + f.varkw as usize;
        if base + nslots.max(npos) + 1 >= STACK_SLOTS {
            return Err("RecursionError: maximum recursion depth exceeded".into());
        }
        let min = arity - f.defaults.len();
        if npos > arity && !f.varargs {
            return Err(Self::arity_error(f, npos));
        }
        let extra: Vec<Value> = if npos > arity {
            self.stack[base + arity..base + npos].to_vec()
        } else {
            Vec::new()
        };
        let mut filled: Vec<bool> = (0..arity).map(|i| i < npos).collect();
        let mut kwdict = if f.varkw { Some(Dict::default()) } else { None };
        for &(name, v) in kws {
            match f.params.iter().position(|&p| p == name) {
                Some(i) => {
                    if filled[i] {
                        return Err(format!(
                            "TypeError: {}() got multiple values for argument '{}'",
                            f.name,
                            self.syms.name(name)
                        ));
                    }
                    self.stack[base + i] = v;
                    filled[i] = true;
                }
                None => match kwdict.as_mut() {
                    Some(d) => {
                        let k = self.heap.string(self.syms.name(name));
                        d.insert(to_key(k)?, k, v);
                    }
                    None => {
                        return Err(format!(
                            "TypeError: {}() got an unexpected keyword argument '{}'",
                            f.name,
                            self.syms.name(name)
                        ))
                    }
                },
            }
        }
        let defaults = match callee.get() {
            Obj::Closure(c) if f.dynamic_defaults => &c.defaults,
            _ => &f.defaults,
        };
        let mut missing = Vec::new();
        for i in 0..arity {
            if !filled[i] {
                if i >= min {
                    self.stack[base + i] = defaults[i - min];
                } else {
                    missing.push(format!("'{}'", self.syms.name(f.params[i])));
                }
            }
        }
        if !missing.is_empty() {
            let list = match missing.len() {
                1 => missing[0].clone(),
                n => format!("{} and {}", missing[..n - 1].join(", "), missing[n - 1]),
            };
            return Err(format!(
                "TypeError: {}() missing {} required positional argument{}: {}",
                f.name,
                missing.len(),
                if missing.len() == 1 { "" } else { "s" },
                list
            ));
        }
        if f.varargs {
            self.stack[base + arity] = self.heap.tuple(extra);
        }
        if let Some(d) = kwdict {
            self.stack[base + arity + f.varargs as usize] = self.heap.dict(d);
        }
        Ok(nslots)
    }

    /// Keyword arguments for the native being called (see `take_kw`).
    pub fn take_kw(&mut self, name: u32) -> Option<Value> {
        let i = self.kwargs.iter().position(|(n, _)| *n == name)?;
        Some(self.kwargs.remove(i).1)
    }

    /// All keyword arguments of the running native (for natives that accept any).
    pub fn take_all_kw(&mut self) -> Vec<(u32, Value)> {
        std::mem::take(&mut self.kwargs)
    }

    /// Calls native `n` with positional `args` and keyword `kws`.
    fn call_native_kw(&mut self, func: NativeFn, fname: &str, args_at: usize, npos: usize, kws: Vec<(u32, Value)>) -> Result<Value, String> {
        let saved = std::mem::replace(&mut self.kwargs, kws);
        let args = unsafe { std::slice::from_raw_parts(self.stack.as_ptr().add(args_at), npos) };
        let r = func(self, args);
        let leftover = std::mem::replace(&mut self.kwargs, saved);
        let r = r?;
        if let Some((name, _)) = leftover.first() {
            return Err(format!(
                "TypeError: {}() got an unexpected keyword argument '{}'",
                fname,
                self.syms.name(*name)
            ));
        }
        Ok(r)
    }

    /// Calls `stack[callee]` with `npos` positional arguments above it and keyword
    /// arguments `kws`. Returns true if a bytecode frame was pushed.
    #[inline(never)]
    fn call_value_kw(&mut self, callee: usize, npos: usize, kws: Vec<(u32, Value)>) -> Result<bool, String> {
        let v = self.stack[callee];
        let g = match v {
            Value::Obj(g) => g,
            _ => return Err(format!("TypeError: '{}' object is not callable", v.type_name())),
        };
        match g.get() {
            Obj::Func(_) | Obj::Closure(_) => {
                self.push_frame_kw(g, callee + 1, npos, &kws, callee)?;
                Ok(true)
            }
            Obj::Bound(b) => {
                self.stack[callee] = b.recv;
                self.push_frame_kw(b.method, callee, npos + 1, &kws, callee)?;
                Ok(true)
            }
            Obj::Class(c) => {
                let inst = self.heap.alloc(Obj::Instance(Instance::new(g, vec![Value::Undef; c.slot_count as usize])));
                self.stack[callee] = Value::Obj(inst);
                match c.init {
                    Some(init) => {
                        self.push_frame_kw(init, callee, npos + 1, &kws, callee)?;
                        Ok(true)
                    }
                    None => {
                        if npos + kws.len() != 0 {
                            return Err(format!(
                                "TypeError: {}() takes no arguments (define init to accept some)",
                                c.name
                            ));
                        }
                        self.sp = callee + 1;
                        Ok(false)
                    }
                }
            }
            Obj::Instance(_) => match dunder(v, sym::D_CALL) {
                Some(Value::Obj(m)) if m.as_callable().is_some() => {
                    self.push_frame_kw(m, callee, npos + 1, &kws, callee)?;
                    Ok(true)
                }
                _ => Err(format!("TypeError: '{}' object is not callable", v.type_name())),
            },
            Obj::Native(n) => {
                let any = n.kwargs.contains(&sym::ANY);
                if let Some((k, _)) = kws.iter().find(|(k, _)| !any && !n.kwargs.contains(k)) {
                    return Err(format!(
                        "TypeError: {}() got an unexpected keyword argument '{}'",
                        n.name,
                        self.syms.name(*k)
                    ));
                }
                let r = self.call_native_kw(n.func, n.name, callee + 1, npos, kws)?;
                self.stack[callee] = r;
                self.sp = callee + 1;
                Ok(false)
            }
            _ => Err(format!("TypeError: '{}' object is not callable", v.type_name())),
        }
    }

    /// Splits the top `argc` stack values into (positional count, keyword pairs)
    /// using the names in tuple constant `k`.
    fn kw_split(&self, argc: usize, k: u32) -> (usize, Vec<(u32, Value)>) {
        let consts = self.frames.last().unwrap().consts;
        let names = match unsafe { *consts.add(k as usize) } {
            Value::Obj(g) => match g.get() {
                Obj::Tuple(t) => t.to_vec(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        };
        let nk = names.len();
        let first_kw = self.sp - nk;
        let kws = names
            .iter()
            .enumerate()
            .map(|(i, n)| match n {
                Value::Int(s) => (*s as u32, self.stack[first_kw + i]),
                _ => (0, Value::None),
            })
            .collect();
        (argc - nk, kws)
    }

    /// `recv.name(pos..., key=value...)`
    #[inline(never)]
    fn invoke_kw(&mut self, recv: usize, name: u32, npos: usize, kws: Vec<(u32, Value)>) -> Result<bool, String> {
        let rv = self.stack[recv];
        if let Value::Obj(g) = rv {
            match g.get() {
                Obj::Instance(inst) => {
                    let cls = inst.class.as_class();
                    if let Some(&slot) = cls.slots.get(&name) {
                        if let Some(&v) = inst.fields.get(slot as usize) {
                            if !matches!(v, Value::Undef) {
                                self.stack[recv] = v;
                                return self.call_value_kw(recv, npos, kws);
                            }
                        }
                    }
                    return match cls.method(name) {
                        Some(Value::Obj(m)) if m.as_callable().is_some() => {
                            self.push_frame_kw(m, recv, npos + 1, &kws, recv)?;
                            Ok(true)
                        }
                        Some(m) => {
                            let v = self.class_member_for(rv, inst.class, name, m)?;
                            self.stack[recv] = v;
                            self.call_value_kw(recv, npos, kws)
                        }
                        None => Err(format!(
                            "AttributeError: '{}' object has no attribute '{}'",
                            cls.name,
                            self.syms.name(name)
                        )),
                    };
                }
                Obj::Class(_) | Obj::Module(_) => {
                    let m = self.get_attr_slow(rv, name)?;
                    self.stack[recv] = m;
                    return self.call_value_kw(recv, npos, kws);
                }
                _ => {}
            }
        }
        // Keyword arguments of built-in methods (handles read their own).
        let is_handle = matches!(rv, Value::Obj(g) if matches!(g.get(), Obj::Handle(_)));
        let allowed: &[u32] = match name {
            _ if is_handle => sym::ANY_KW,
            sym::SORT => &[sym::KEY, sym::REVERSE],
            sym::SPLIT | sym::RSPLIT => &[sym::SEP, sym::MAXSPLIT],
            sym::SPLITLINES => &[sym::KEEPENDS],
            sym::FORMAT | sym::UPDATE => sym::ANY_KW,
            _ => &[],
        };
        let any = allowed.contains(&sym::ANY);
        if let Some((k, _)) = kws.iter().find(|(k, _)| !any && !allowed.contains(k)) {
            return Err(format!(
                "TypeError: {}.{}() got an unexpected keyword argument '{}'",
                rv.type_name(),
                self.syms.name(name),
                self.syms.name(*k)
            ));
        }
        let saved = std::mem::replace(&mut self.kwargs, kws);
        let args = unsafe { std::slice::from_raw_parts(self.stack.as_ptr().add(recv + 1), npos) };
        let r = crate::builtins::call_method(self, rv, name, args);
        let leftover = std::mem::replace(&mut self.kwargs, saved);
        let r = r?;
        if let Some((k, _)) = leftover.first() {
            return Err(format!(
                "TypeError: {}.{}() got an unexpected keyword argument '{}'",
                rv.type_name(),
                self.syms.name(name),
                self.syms.name(*k)
            ));
        }
        self.stack[recv] = r;
        self.sp = recv + 1;
        Ok(false)
    }

    /// `f(*list, **dict)`: [callee, list, dict] on the stack.
    #[inline(never)]
    fn call_ex(&mut self) -> Result<bool, String> {
        let callee = self.sp - 3;
        let (list, dict) = (self.stack[callee + 1], self.stack[callee + 2]);
        let items = match list {
            Value::Obj(g) => match g.get() {
                Obj::List(l) => l.clone(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        };
        let entries = match dict {
            Value::Obj(g) => match g.get() {
                Obj::Dict(d) => d.entries.clone(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        };
        if callee + 1 + items.len() + entries.len() + 1 >= STACK_SLOTS {
            return Err("RecursionError: maximum recursion depth exceeded".into());
        }
        let mut kws = Vec::with_capacity(entries.len());
        for (k, v) in entries.iter() {
            match k.as_str() {
                Some(name) => kws.push((self.syms.intern(name), *v)),
                None => return Err("TypeError: keywords must be strings".into()),
            }
        }
        // keep the values on the stack (GC roots) while the call is set up
        let mut at = callee + 1;
        for v in items.iter().chain(entries.iter().map(|(_, v)| v)) {
            self.stack[at] = *v;
            at += 1;
        }
        self.sp = at;
        self.call_value_kw(callee, items.len(), kws)
    }

    /// Calls the value at `stack[callee]` with `argc` args above it.
    /// Returns true if a bytecode frame was pushed.
    #[inline(never)]
    fn call_value(&mut self, callee: usize, argc: usize) -> Result<bool, String> {
        let v = self.stack[callee];
        let g = match v {
            Value::Obj(g) => g,
            _ => return Err(format!("TypeError: '{}' object is not callable", v.type_name())),
        };
        match g.get() {
            Obj::Func(_) | Obj::Closure(_) => {
                self.push_frame(g, callee + 1, argc, callee)?;
                Ok(true)
            }
            Obj::Native(n) => {
                if !self.kwargs.is_empty() {
                    // a native called back into from another native: don't let it see
                    // the outer call's keyword arguments
                    let r = self.call_native_kw(n.func, n.name, callee + 1, argc, Vec::new())?;
                    self.stack[callee] = r;
                    self.sp = callee + 1;
                    return Ok(false);
                }
                // Natives never touch the VM stack, so they can read their args in place.
                let args =
                    unsafe { std::slice::from_raw_parts(self.stack.as_ptr().add(callee + 1), argc) };
                let r = (n.func)(self, args)?;
                self.stack[callee] = r;
                self.sp = callee + 1;
                Ok(false)
            }
            Obj::Class(c) => {
                let inst = self.heap.alloc(Obj::Instance(Instance::new(
                    g,
                    vec![Value::Undef; c.slot_count as usize],
                )));
                self.stack[callee] = Value::Obj(inst);
                match c.init {
                    Some(init) => {
                        self.push_frame(init, callee, argc + 1, callee)?;
                        Ok(true)
                    }
                    None => {
                        if argc != 0 {
                            return Err(format!(
                                "TypeError: {}() takes no arguments (define init to accept some)",
                                c.name
                            ));
                        }
                        self.sp = callee + 1;
                        Ok(false)
                    }
                }
            }
            Obj::Bound(b) => {
                self.stack[callee] = b.recv;
                self.push_frame(b.method, callee, argc + 1, callee)?;
                Ok(true)
            }
            Obj::Instance(_) => match dunder(v, sym::D_CALL) {
                Some(Value::Obj(m)) if m.as_callable().is_some() => {
                    // `obj(args)` -> obj.__call__(args): obj is already where `self` goes.
                    self.push_frame(m, callee, argc + 1, callee)?;
                    Ok(true)
                }
                _ => Err(format!("TypeError: '{}' object is not callable", v.type_name())),
            },
            _ => Err(format!("TypeError: '{}' object is not callable", v.type_name())),
        }
    }

    /// `recv.name(args)` with the receiver at `stack[recv]`.
    #[inline(never)]
    fn invoke(&mut self, recv: usize, name: u32, argc: usize) -> Result<bool, String> {
        let rv = self.stack[recv];
        if let Value::Obj(g) = rv {
            match g.get() {
                Obj::Instance(inst) => {
                    let cls = inst.class.as_class();
                    if let Some(&slot) = cls.slots.get(&name) {
                        if let Some(&v) = inst.fields.get(slot as usize) {
                            if !matches!(v, Value::Undef) {
                                self.stack[recv] = v;
                                return self.call_value(recv, argc);
                            }
                        }
                    }
                    return match cls.method(name) {
                        Some(Value::Obj(m)) if m.as_callable().is_some() => {
                            self.push_frame(m, recv, argc + 1, recv)?;
                            Ok(true)
                        }
                        Some(m) => {
                            let v = self.class_member_for(rv, inst.class, name, m)?;
                            self.stack[recv] = v;
                            self.call_value(recv, argc)
                        }
                        _ => Err(format!(
                            "AttributeError: '{}' object has no attribute '{}'",
                            cls.name,
                            self.syms.name(name)
                        )),
                    };
                }
                Obj::Module(m) => {
                    let v = self.module_attr(m.unit, name, &m.name)?;
                    self.stack[recv] = v;
                    return self.call_value(recv, argc);
                }
                Obj::Class(c) => {
                    return match c.method(name) {
                        Some(m) => {
                            let v = self.class_attr_on_class(g, m)?;
                            self.stack[recv] = v;
                            self.call_value(recv, argc)
                        }
                        None => Err(format!(
                            "AttributeError: class '{}' has no method '{}'",
                            c.name,
                            self.syms.name(name)
                        )),
                    };
                }
                _ => {}
            }
        }
        let args = unsafe { std::slice::from_raw_parts(self.stack.as_ptr().add(recv + 1), argc) };
        let r = crate::builtins::call_method(self, rv, name, args)?;
        self.stack[recv] = r;
        self.sp = recv + 1;
        Ok(false)
    }

    /// Attribute read. Also returns `(class_id, slot)` when the value came from an
    /// instance field, so the caller can rewrite the instruction into its cached form.
    #[inline(never)]
    fn load_attr(&mut self, obj: Value, name: u32) -> Result<(Value, Option<(u32, u32)>), String> {
        if let Value::Obj(g) = obj {
            if let Obj::Instance(inst) = g.get() {
                let cls = inst.class.as_class();
                if let Some(&slot) = cls.slots.get(&name) {
                    match inst.fields.get(slot as usize) {
                        Some(Value::Undef) | None => {}
                        Some(&v) => return Ok((v, Some((cls.id, slot)))),
                    }
                }
            }
        }
        self.get_attr_slow(obj, name).map(|v| (v, None))
    }

    /// Attribute write; returns `(class_id, slot)` for the inline cache when the
    /// target was an instance.
    #[inline(never)]
    fn store_attr(&mut self, obj: Value, name: u32, v: Value) -> Result<Option<(u32, u32)>, String> {
        if let Value::Obj(g) = obj {
            // a property on the class takes the assignment
            if let Obj::Instance(inst) = g.get() {
                if let Some(m) = inst.class.as_class().method(name) {
                    if let Some((0, p)) = self.descriptor(m) {
                        let fset = self.field_of(p, "fset");
                        if matches!(fset, Value::None) {
                            return Err(format!(
                                "AttributeError: property '{}' of '{}' object has no setter",
                                self.syms.name(name),
                                obj.type_name()
                            ));
                        }
                        self.call(fset, &[obj, v])?;
                        return Ok(None);
                    }
                }
            }
            match g.get_mut() {
                Obj::Instance(inst) => {
                    let cls = inst.class.as_class_mut();
                    let slot = cls.slot_for(name, &mut self.heap);
                    let s = slot as usize;
                    if s >= inst.fields.len() {
                        inst.fields.resize(s + 1, Value::Undef);
                    }
                    inst.fields[s] = v;
                    return Ok(Some((cls.id, slot)));
                }
                // Class.attr = v: a class attribute (also replaces a method)
                Obj::Class(c) => {
                    let existed = c.methods.contains_key(&name);
                    c.set_method(name, v);
                    if !existed {
                        c.id = self.heap.new_class_id();
                    }
                    return Ok(None);
                }
                Obj::Module(m) => {
                    // Modules can't grow new globals at runtime (frames hold pointers
                    // into their storage), but existing ones can be reassigned.
                    let (unit, mname) = (m.unit, m.name.clone());
                    match self.module_slot(unit, name) {
                        Some(slot) => {
                            self.units[unit as usize].globals[slot as usize] = v;
                            return Ok(None);
                        }
                        None => {
                            return Err(format!(
                                "AttributeError: module '{}' has no variable '{}' (modules can't gain new variables from outside)",
                                mname,
                                self.syms.name(name)
                            ))
                        }
                    }
                }
                _ => {}
            }
        }
        Err(self.set_attr_error(obj, name))
    }

    /// `del obj[idx]`
    fn del_index(&mut self, obj: Value, idx: Value) -> Result<(), String> {
        if let Some(m) = dunder(obj, sym::D_DELITEM) {
            self.call(m, &[obj, idx])?;
            return Ok(());
        }
        if let Value::Obj(g) = obj {
            match g.get_mut() {
                Obj::List(l) => {
                    let i = match idx {
                        Value::Int(i) => i,
                        Value::Bool(b) => b as i64,
                        _ => return Err(format!("TypeError: list indices must be integers, not '{}'", idx.type_name())),
                    };
                    let n = l.len() as i64;
                    let j = if i < 0 { i + n } else { i };
                    if j < 0 || j >= n {
                        return Err("IndexError: list assignment index out of range".into());
                    }
                    l.remove(j as usize);
                    return Ok(());
                }
                Obj::Dict(d) => {
                    let k = to_key(idx)?;
                    return match d.remove(&k) {
                        Some(_) => Ok(()),
                        None => Err(format!("KeyError: {}", idx.repr())),
                    };
                }
                _ => {}
            }
        }
        Err(format!("TypeError: '{}' object doesn't support item deletion", obj.type_name()))
    }

    /// `del obj.name`
    fn del_attr(&mut self, obj: Value, name: u32) -> Result<(), String> {
        if let Value::Obj(g) = obj {
            if let Obj::Instance(inst) = g.get_mut() {
                let cls = inst.class.as_class_mut();
                if let Some(&slot) = cls.slots.get(&name) {
                    if let Some(f) = inst.fields.get_mut(slot as usize) {
                        if !matches!(f, Value::Undef) {
                            *f = Value::Undef;
                            // cached field reads must re-check
                            cls.id = self.heap.new_class_id();
                            return Ok(());
                        }
                    }
                }
                return Err(format!(
                    "AttributeError: '{}' object has no attribute '{}'",
                    cls.name,
                    self.syms.name(name)
                ));
            }
        }
        Err(format!(
            "AttributeError: cannot delete attribute '{}' of '{}' object",
            self.syms.name(name),
            obj.type_name()
        ))
    }

    fn module_slot(&mut self, unit: u32, name: u32) -> Option<u32> {
        let u = &mut self.units[unit as usize];
        if let Some(&s) = u.attr_slots.get(&name) {
            return Some(s);
        }
        let s = *u.gtable.index.get(self.syms.name(name))?;
        u.attr_slots.insert(name, s);
        Some(s)
    }

    fn module_attr(&mut self, unit: u32, name: u32, mname: &str) -> Result<Value, String> {
        let v = match self.module_slot(unit, name) {
            Some(slot) => self.units[unit as usize].globals[slot as usize],
            None => Value::Undef,
        };
        if let Value::Undef = v {
            // a submodule of a package (imported now if needed)
            if let Some(&m) = self.units[unit as usize].children.get(&name) {
                return Ok(m);
            }
            if self.units[unit as usize].package {
                let full = format!("{}.{}", mname, self.syms.name(name));
                let fsym = self.syms.intern(&full);
                match self.import(fsym) {
                    Ok(m) => return Ok(m),
                    Err(e) if !e.starts_with("ModuleNotFoundError") => return Err(e),
                    Err(_) => {}
                }
            }
            return Err(format!(
                "AttributeError: module '{}' has no attribute '{}'",
                mname,
                self.syms.name(name)
            ));
        }
        Ok(v)
    }

    /// `import name`: a built-in module, or `name.fst` next to the importing file.
    /// `import name` (possibly dotted): a built-in module, a standard-library
    /// module written in Faston, or a file / package found next to the importing
    /// file or on `sys.path` (see modules.rs).
    #[inline(never)]
    fn import(&mut self, sym: u32) -> Result<Value, String> {
        if let Some(&m) = self.modules.get(&sym) {
            return Ok(m);
        }
        let name = self.syms.name(sym).to_string();
        if let Some(src) = crate::stdlib::source(&name) {
            let file = format!("<{}>", name);
            let unit = self.new_unit(file.clone(), None);
            let script = self.compile_unit(unit, src).map_err(|e| format!("File \"{}\": {}", file, e))?;
            let module = Value::Obj(self.heap.alloc(Obj::Module(ModuleRef { unit, name })));
            self.modules.insert(sym, module);
            self.call(Value::Obj(script), &[])?;
            return Ok(module);
        }
        let (parent, last) = match name.rsplit_once('.') {
            Some((p, l)) => (Some(p.to_string()), l.to_string()),
            None => (None, name.clone()),
        };
        let mut parent_unit = None;
        let dirs: Vec<PathBuf> = match &parent {
            Some(p) => {
                let psym = self.syms.intern(p);
                let pm = self.import(psym)?;
                let pu = match pm {
                    Value::Obj(g) => match g.get() {
                        Obj::Module(m) => m.unit,
                        _ => return Err(format!("ModuleNotFoundError: No module named '{}'; '{}' is not a package", name, p)),
                    },
                    _ => unreachable!(),
                };
                if !self.units[pu as usize].package {
                    return Err(format!("ModuleNotFoundError: No module named '{}'; '{}' is not a package", name, p));
                }
                parent_unit = Some(pu);
                vec![self.units[pu as usize].dir.clone().unwrap_or_default()]
            }
            None => {
                let cur = self.frames.last().map_or(self.main, |f| f.func().as_func().unit);
                let mut v = vec![self.units[cur as usize].dir.clone().unwrap_or_else(|| PathBuf::from("."))];
                v.extend(self.sys_path());
                v
            }
        };
        let found = {
            let source = match &self.bundle {
                Some(b) => crate::modules::Source::Bundle(b),
                None => crate::modules::Source::Disk,
            };
            crate::modules::find(&source, &last, &dirs)
        };
        let found = found.ok_or_else(|| {
            let looked: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
            format!(
                "ModuleNotFoundError: No module named '{}' (looked in: {})",
                name,
                if looked.is_empty() { "nowhere".into() } else { looked.join(", ") }
            )
        })?;
        let key = PathBuf::from(&found.key);
        let lsym = self.syms.intern(&last);
        if let Some(&m) = self.loaded.get(&key) {
            // already loaded (or loading: circular imports see it partially)
            if let Some(pu) = parent_unit {
                self.units[pu as usize].children.insert(lsym, m);
            }
            return Ok(m);
        }
        let unit = self.new_unit(found.display.clone(), Some(found.dir.clone()));
        self.units[unit as usize].package = found.package;
        let script = self
            .compile_unit(unit, &found.src)
            .map_err(|e| format!("File \"{}\": {}", found.display, e))?;
        let module = Value::Obj(self.heap.alloc(Obj::Module(ModuleRef { unit, name })));
        self.loaded.insert(key, module);
        if let Some(pu) = parent_unit {
            self.units[pu as usize].children.insert(lsym, module);
        }
        self.call(Value::Obj(script), &[])?;
        Ok(module)
    }

    /// The directories in `sys.path`.
    fn sys_path(&mut self) -> Vec<PathBuf> {
        let Some(sys) = self.module("sys") else { return Vec::new() };
        let p = self.syms.intern("path");
        match self.get_attr(sys, p) {
            Ok(Value::Obj(g)) => match g.get() {
                Obj::List(l) => l.iter().filter_map(|v| v.as_str().map(PathBuf::from)).collect(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    /// Sets `sys.path` (directories searched by `import`).
    pub fn set_sys_path(&mut self, dirs: Vec<PathBuf>) {
        let vals: Vec<Value> = dirs.iter().map(|d| self.heap.string(d.display().to_string())).collect();
        if let Some(sys) = self.module("sys") {
            let p = self.syms.intern("path");
            if let Ok(Value::Obj(g)) = self.get_attr(sys, p) {
                if let Obj::List(l) = g.get_mut() {
                    *l = vals;
                }
            }
        }
    }

    #[inline(never)]
    fn get_attr_slow(&mut self, obj: Value, name: u32) -> Result<Value, String> {
        if let Value::Obj(g) = obj {
            match g.get() {
                Obj::Instance(inst) => {
                    let cls = inst.class.as_class();
                    return match cls.method(name) {
                        Some(Value::Obj(m)) if m.as_callable().is_some() => Ok(Value::Obj(
                            self.heap.alloc(Obj::Bound(Bound { recv: obj, method: m })),
                        )),
                        Some(other) => self.class_member_for(obj, inst.class, name, other),
                        _ => Err(format!(
                            "AttributeError: '{}' object has no attribute '{}'",
                            cls.name,
                            self.syms.name(name)
                        )),
                    };
                }
                Obj::Module(m) => return self.module_attr(m.unit, name, &m.name),
                Obj::Handle(h) => {
                    let attr = h.ty.attr;
                    let n = self.syms.name(name).to_string();
                    if let Some(r) = attr(self, g, &n) {
                        return r;
                    }
                }
                Obj::Class(c) => {
                    return match c.method(name) {
                        Some(m) => self.class_attr_on_class(g, m),
                        None => Err(format!(
                            "AttributeError: class '{}' has no attribute '{}'",
                            c.name,
                            self.syms.name(name)
                        )),
                    };
                }
                _ => {}
            }
        }
        Err(format!(
            "AttributeError: '{}' object has no attribute '{}'",
            obj.type_name(),
            self.syms.name(name)
        ))
    }

    #[cold]
    #[inline(never)]
    fn set_attr_error(&self, obj: Value, name: u32) -> String {
        format!(
            "AttributeError: cannot set attribute '{}' on '{}' object",
            self.syms.name(name),
            obj.type_name()
        )
    }

    #[cold]
    #[inline(never)]
    fn name_error(&self, g: u32) -> String {
        let unit = self.frames.last().map_or(self.main, |f| f.func().as_func().unit);
        let name = self.units[unit as usize].gtable.names.get(g as usize).map_or("?", |s| s.as_str());
        format!("NameError: name '{}' is not defined", name)
    }

    #[inline(never)]
    fn get_iter_state(&mut self, v: Value) -> Result<(Value, Value, Value), String> {
        if let Value::Obj(g) = v {
            match g.get() {
                Obj::Range(r) => return Ok((Value::Int(r.start), Value::Int(r.stop), Value::Int(r.step))),
                Obj::List(_) => return Ok((v, Value::Int(0), Value::None)),
                Obj::Tuple(_) => return Ok((v, Value::Int(0), Value::None)),
                Obj::Str(_) | Obj::Dict(_) | Obj::Set(_) => {
                    let items = ops::to_vec(&mut self.heap, v)?;
                    return Ok((self.heap.list(items), Value::Int(0), Value::None));
                }
                Obj::Generator(_) => return Ok((v, Value::Int(0), Value::None)),
                Obj::Instance(_) => {
                    if dunder(v, sym::D_NEXT).is_some() {
                        let it = self.make_iter(v)?;
                        return Ok((it, Value::Int(0), Value::None));
                    }
                    if let Some(m) = dunder(v, sym::D_ITER) {
                        let it = self.call(m, &[v])?;
                        if is_instance(&it) {
                            if dunder(it, sym::D_NEXT).is_some() {
                                let it = self.make_iter(it)?;
                                return Ok((it, Value::Int(0), Value::None));
                            }
                            return Err("TypeError: __iter__ must return an iterable or an iterator (an object with __next__)".into());
                        }
                        return self.get_iter_state(it);
                    }
                }
                Obj::Handle(_) => {
                    if let Some(items) = self.handle_iter(v)? {
                        return self.get_iter_state(items);
                    }
                }
                _ => {}
            }
        }
        Err(format!("TypeError: '{}' object is not iterable", v.type_name()))
    }

    // ------------------------------------------------------------------ cold ops
    //
    // Rarely-hot instructions live out of line so the main dispatch loop stays
    // small enough for the compiler to keep its state in registers. They work on
    // `self.sp` / the current frame and may push a frame (uncached `Invoke`).

    #[inline]
    fn spop(&mut self) -> Value {
        self.sp -= 1;
        self.stack[self.sp]
    }

    #[inline]
    fn spush(&mut self, v: Value) {
        self.stack[self.sp] = v;
        self.sp += 1;
    }

    #[inline]
    fn rewrite_current(&mut self, op: Op) {
        let fr = self.frames.last().unwrap();
        unsafe { *(fr.ip as *mut Op).sub(1) = op };
    }

    #[inline(never)]
    fn exec_cold(&mut self, op: Op) -> Result<(), String> {
        match op {
            Op::Swap => {
                let n = self.sp;
                self.stack.swap(n - 1, n - 2);
            }
            Op::Rot3 => {
                let n = self.sp;
                let c = self.stack[n - 1];
                self.stack[n - 1] = self.stack[n - 2];
                self.stack[n - 2] = self.stack[n - 3];
                self.stack[n - 3] = c;
            }
            Op::Dup2 => {
                let a = self.stack[self.sp - 2];
                let b = self.stack[self.sp - 1];
                self.spush(a);
                self.spush(b);
            }
            Op::GetAttr(n) => {
                let obj = self.spop();
                let (v, cache) = self.load_attr(obj, n)?;
                self.spush(v);
                if let Some((cid, slot)) = cache {
                    self.rewrite_current(Op::GetAttrIC(n, cid, slot));
                }
            }
            Op::GetLocalAttr(a, n) => {
                let base = self.frames.last().unwrap().base;
                let obj = self.stack[base + a as usize];
                let (v, cache) = self.load_attr(obj, n)?;
                self.spush(v);
                if let Some((cid, slot)) = cache {
                    if a < 0x10000 && slot < 0x10000 {
                        self.rewrite_current(Op::GetLocalAttrIC(n, cid, a | (slot << 16)));
                    }
                }
            }
            Op::SetAttr(n) => {
                let v = self.spop();
                let obj = self.spop();
                if let Some((cid, slot)) = self.store_attr(obj, n, v)? {
                    self.rewrite_current(Op::SetAttrIC(n, cid, slot));
                }
            }
            Op::SetLocalAttr(a, n) => {
                let v = self.spop();
                let base = self.frames.last().unwrap().base;
                let obj = self.stack[base + a as usize];
                if let Some((cid, slot)) = self.store_attr(obj, n, v)? {
                    if a < 0x10000 && slot < 0x10000 {
                        self.rewrite_current(Op::SetLocalAttrIC(n, cid, a | (slot << 16)));
                    }
                }
            }
            Op::Closure(k) | Op::ClosureDefaults(k, _) => {
                let ndefaults = match op {
                    Op::ClosureDefaults(_, n) => n as usize,
                    _ => 0,
                };
                let defaults = self.stack[self.sp - ndefaults..self.sp].to_vec();
                self.sp -= ndefaults;
                let fr = self.frames.last().unwrap();
                let (base, parent_upvals) = (fr.base, fr.upvals);
                let fval = unsafe { *fr.consts.add(k as usize) };
                let func = match fval {
                    Value::Obj(g) => g,
                    _ => unreachable!("closure constant is not a function"),
                };
                let caps = &func.as_func().captures;
                let mut upvals = Vec::with_capacity(caps.len());
                for &(from_local, idx) in caps.iter() {
                    upvals.push(if from_local {
                        self.capture_upvalue(base + idx as usize)
                    } else {
                        unsafe { *parent_upvals.add(idx as usize) }
                    });
                }
                let c = self.heap.alloc(Obj::Closure(Closure { func, upvals, defaults }));
                self.spush(Value::Obj(c));
            }
            Op::CloseUpvals(slot) => {
                let base = self.frames.last().unwrap().base;
                self.close_upvalues(base + slot as usize);
            }
            Op::SetIndex => {
                // operands stay on the stack (visible to the GC) during `__setitem__`
                let n = self.sp;
                let (obj, idx, v) = (self.stack[n - 3], self.stack[n - 2], self.stack[n - 1]);
                match dunder(obj, sym::D_SETITEM) {
                    Some(m) => {
                        self.call(m, &[obj, idx, v])?;
                    }
                    None => ops::set_index(obj, idx, v)?,
                }
                self.sp -= 3;
            }
            Op::FloorDiv | Op::Pow | Op::BitAnd | Op::BitOr | Op::BitXor | Op::Shl | Op::Shr => {
                let kind = match op {
                    Op::FloorDiv => BinOp::FloorDiv,
                    Op::Pow => BinOp::Pow,
                    Op::BitAnd => BinOp::BitAnd,
                    Op::BitOr => BinOp::BitOr,
                    Op::BitXor => BinOp::BitXor,
                    Op::Shl => BinOp::Shl,
                    _ => BinOp::Shr,
                };
                let (a, b) = (self.stack[self.sp - 2], self.stack[self.sp - 1]);
                let r = self.binop(kind, a, b)?;
                self.sp -= 2;
                self.spush(r);
            }
            Op::Invert => {
                let a = self.stack[self.sp - 1];
                self.stack[self.sp - 1] = ops::invert(a)?;
            }
            Op::In => {
                let (item, container) = (self.stack[self.sp - 2], self.stack[self.sp - 1]);
                let r = self.contains(container, item)?;
                self.sp -= 2;
                self.spush(Value::Bool(r));
            }
            Op::GetIter(s) => {
                let v = self.stack[self.sp - 1];
                let (a, b, c) = self.get_iter_state(v)?;
                self.sp -= 1;
                let base = self.frames.last().unwrap().base + s as usize;
                self.stack[base] = a;
                self.stack[base + 1] = b;
                self.stack[base + 2] = c;
            }
            Op::Invoke(name, argc) => {
                let argc = argc as usize;
                let recv = self.sp - argc - 1;
                // Learn the (class, method) pair for the inline cache.
                if let Value::Obj(g) = self.stack[recv] {
                    if name == sym::APPEND && argc == 1 && matches!(g.get(), Obj::List(_)) {
                        self.rewrite_current(Op::InvokeAppend(name));
                    }
                    if let Obj::Instance(inst) = g.get() {
                        let cls = inst.class.as_class();
                        if !cls.slots.contains_key(&name) && argc < 0x100 {
                            if let Some(&midx) = cls.methods.get(&name) {
                                if midx < 0x0100_0000 {
                                    let packed = argc as u32 | (midx << 8);
                                    self.rewrite_current(Op::InvokeIC(name, cls.id, packed));
                                }
                            }
                        }
                    }
                }
                self.invoke(recv, name, argc)?;
            }
            Op::BuildList(n) => {
                let n = n as usize;
                let items = self.stack[self.sp - n..self.sp].to_vec();
                self.sp -= n;
                let l = self.heap.list(items);
                self.spush(l);
            }
            Op::BuildTuple(n) => {
                let n = n as usize;
                let items = self.stack[self.sp - n..self.sp].to_vec();
                self.sp -= n;
                let t = self.heap.tuple(items);
                self.spush(t);
            }
            Op::BuildSet(n) => {
                let n = n as usize;
                let items = self.stack[self.sp - n..self.sp].to_vec();
                let set = ops::make_set(&mut self.heap, items)?;
                self.sp -= n;
                self.spush(set);
            }
            Op::BuildDict(n) => {
                let n = n as usize;
                let start = self.sp - 2 * n;
                let mut d = Dict::default();
                for i in 0..n {
                    let k = self.stack[start + 2 * i];
                    let v = self.stack[start + 2 * i + 1];
                    d.insert(to_key(k)?, k, v);
                }
                self.sp = start;
                let d = self.heap.dict(d);
                self.spush(d);
            }
            Op::Class(n) => {
                let id = self.heap.new_class_id();
                let cls = Class::new(self.syms.name(n).to_string(), id);
                let c = self.heap.alloc(Obj::Class(Box::new(cls)));
                self.spush(Value::Obj(c));
            }
            Op::Inherit => {
                let sup = self.spop();
                let cls = self.stack[self.sp - 1];
                match (sup, cls) {
                    (Value::Obj(s), Value::Obj(c)) if matches!(s.get(), Obj::Class(_)) && s != c => {
                        let parent = s.as_class();
                        let child = c.as_class_mut();
                        let mut inherited: Vec<(u32, u32)> =
                            parent.methods.iter().map(|(k, i)| (*k, *i)).collect();
                        inherited.sort_by_key(|&(_, i)| i);
                        for (k, i) in inherited {
                            child.set_method(k, parent.method_vals[i as usize]);
                        }
                        child.parent = Some(s);
                    }
                    _ => {
                        return Err(format!(
                            "TypeError: can only inherit from a class, not '{}'",
                            sup.type_name()
                        ))
                    }
                }
            }
            Op::Method(n) => {
                let m = self.spop();
                if let Value::Obj(c) = self.stack[self.sp - 1] {
                    c.as_class_mut().set_method(n, m);
                }
            }
            Op::Unpack(n) => {
                let v = self.stack[self.sp - 1];
                let items = self.to_vec(v).map_err(|e| {
                    if e.contains("not iterable") {
                        format!("TypeError: cannot unpack non-sequence '{}'", v.type_name())
                    } else {
                        e
                    }
                })?;
                self.sp -= 1;
                if items.len() != n as usize {
                    return Err(format!(
                        "ValueError: expected {} values to unpack, got {}",
                        n,
                        items.len()
                    ));
                }
                if self.sp + items.len() >= STACK_SLOTS {
                    return Err("RecursionError: stack overflow".into());
                }
                for it in items {
                    self.spush(it);
                }
            }
            Op::GetSlice => {
                let step = self.spop();
                let stop = self.spop();
                let start = self.spop();
                let obj = self.spop();
                let r = ops::slice(&mut self.heap, obj, start, stop, step)?;
                self.spush(r);
            }
            Op::Import(sym) => {
                let m = self.import(sym)?;
                self.spush(m);
            }
            Op::Raise => return Err(self.raise()),
            Op::SetAdd(slot) | Op::DictSet(slot) => {
                let base = self.frames.last().unwrap().base;
                let target = self.stack[base + slot as usize];
                let (k, v) = if let Op::DictSet(_) = op {
                    (self.stack[self.sp - 2], self.stack[self.sp - 1])
                } else {
                    (self.stack[self.sp - 1], Value::None)
                };
                if let Value::Obj(g) = target {
                    match g.get_mut() {
                        Obj::Set(d) | Obj::Dict(d) => d.insert(to_key(k)?, k, v),
                        _ => {}
                    }
                }
                self.sp -= if let Op::DictSet(_) = op { 2 } else { 1 };
            }
            Op::CallKw(argc, k) => {
                let (npos, kws) = self.kw_split(argc as usize, k);
                let callee = self.sp - argc as usize - 1;
                self.call_value_kw(callee, npos, kws)?;
            }
            Op::InvokeKw(name, argc, k) => {
                let (npos, kws) = self.kw_split(argc as usize, k);
                let recv = self.sp - argc as usize - 1;
                self.invoke_kw(recv, name, npos, kws)?;
            }
            Op::CallEx => {
                self.call_ex()?;
            }
            Op::ArgPush => {
                let v = self.spop();
                if let Value::Obj(g) = self.stack[self.sp - 1] {
                    if let Obj::List(l) = g.get_mut() {
                        l.push(v);
                    }
                }
            }
            Op::ArgExtend => {
                let xs = self.stack[self.sp - 1];
                let items = self.to_vec(xs).map_err(|_| {
                    format!("TypeError: argument after * must be iterable, not '{}'", xs.type_name())
                })?;
                self.sp -= 1;
                if let Value::Obj(g) = self.stack[self.sp - 1] {
                    if let Obj::List(l) = g.get_mut() {
                        l.extend(items);
                    }
                }
            }
            Op::ArgKw | Op::ArgMerge => {
                let pairs: Vec<(Value, Value)> = if op == Op::ArgKw {
                    let v = self.spop();
                    let k = self.spop();
                    vec![(k, v)]
                } else {
                    let d = self.spop();
                    match d {
                        Value::Obj(g) if matches!(g.get(), Obj::Dict(_)) => match g.get() {
                            Obj::Dict(dd) => dd.entries.clone(),
                            _ => unreachable!(),
                        },
                        _ => {
                            return Err(format!(
                                "TypeError: argument after ** must be a dict, not '{}'",
                                d.type_name()
                            ))
                        }
                    }
                };
                if let Value::Obj(g) = self.stack[self.sp - 1] {
                    if let Obj::Dict(dict) = g.get_mut() {
                        for (k, v) in pairs {
                            if k.as_str().is_none() {
                                return Err("TypeError: keywords must be strings".into());
                            }
                            let key = to_key(k)?;
                            if dict.contains(&key) {
                                return Err(format!(
                                    "TypeError: got multiple values for keyword argument {}",
                                    k.repr()
                                ));
                            }
                            dict.insert(key, k, v);
                        }
                    }
                }
            }
            Op::ExcMatch(n) => {
                let n = n as usize;
                let exc = self.stack[self.sp - n - 1];
                let cls = match exc {
                    Value::Obj(g) => match g.get() {
                        Obj::Instance(i) => Some(i.class),
                        _ => None,
                    },
                    _ => None,
                };
                let mut hit = false;
                for i in 0..n {
                    let t = self.stack[self.sp - n + i];
                    let mut targets = vec![t];
                    if let Value::Obj(g) = t {
                        if let Obj::Tuple(items) = g.get() {
                            targets = items.to_vec();
                        }
                    }
                    for t in targets {
                        match t {
                            Value::Obj(c) if matches!(c.get(), Obj::Class(_)) => {
                                hit |= Self::is_subclass(cls, c);
                            }
                            _ => {
                                return Err(format!(
                                    "TypeError: catching '{}' that is not an exception class",
                                    t.type_name()
                                ))
                            }
                        }
                    }
                }
                self.sp -= n + 1;
                self.spush(Value::Bool(hit));
            }
            Op::Is => {
                let b = self.spop();
                let a = self.spop();
                self.spush(Value::Bool(same_object(a, b)));
            }
            Op::DelIndex => {
                let (obj, idx) = (self.stack[self.sp - 2], self.stack[self.sp - 1]);
                self.del_index(obj, idx)?;
                self.sp -= 2;
            }
            Op::DelAttr(n) => {
                let obj = self.stack[self.sp - 1];
                self.del_attr(obj, n)?;
                self.sp -= 1;
            }
            Op::DelGlobal(g) => {
                let globals = self.frames.last().unwrap().globals;
                let slot = unsafe { &mut *globals.add(g as usize) };
                if matches!(*slot, Value::Undef) {
                    return Err(self.name_error(g));
                }
                *slot = Value::Undef;
            }
            Op::SetSlice => {
                let s = self.sp;
                let (obj, start, stop, step, v) =
                    (self.stack[s - 5], self.stack[s - 4], self.stack[s - 3], self.stack[s - 2], self.stack[s - 1]);
                let items = self.to_vec(v)?;
                slice_assign(obj, start, stop, step, Some(items))?;
                self.sp -= 5;
            }
            Op::DelSlice => {
                let s = self.sp;
                let (obj, start, stop, step) = (self.stack[s - 4], self.stack[s - 3], self.stack[s - 2], self.stack[s - 1]);
                slice_assign(obj, start, stop, step, None)?;
                self.sp -= 4;
            }
            other => unreachable!("{:?} is handled in the main loop", other),
        }
        Ok(())
    }

    // ------------------------------------------------------------------ main loop

    /// Runs the top frame until it returns (to the depth the frame was pushed at):
    /// natively when its function was compiled, otherwise in the interpreter.
    fn run(&mut self) -> Result<Value, String> {
        let g = self.frames.last().unwrap().func();
        let f = g.as_func();
        if !f.jit.is_null() {
            return self.run_native(f.jit);
        }
        if self.jit.is_some() && f.native_state == 0 {
            // first call: compile it now
            self.compile_native(g, false);
            let info = g.as_func().jit;
            if !info.is_null() {
                return self.run_native(info);
            }
        }
        self.interp()
    }

    /// True when the top frame's function runs natively (or is about to be compiled).
    #[inline]
    fn top_is_native(&self) -> bool {
        self.native && {
            let f = self.frames.last().unwrap().func().as_func();
            !f.jit.is_null() || (f.native_state == 0 && self.jit.is_some())
        }
    }

    /// The bytecode interpreter: runs the top frame until it returns.
    #[allow(unused_assignments)] // `reload!` sets `ip` even where a jump overrides it
    fn interp(&mut self) -> Result<Value, String> {
        let stop = self.frames.len() - 1;
        // compiled mode: calls go to native code instead of being interpreted inline
        let native_mode = self.native;
        // without a code generator, functions with no saved code are interpreted
        let can_compile = self.jit.is_some();
        let st: *mut Value = self.stack.as_mut_ptr();
        let mut sp = self.sp;
        let (mut code, mut consts, mut ip, mut base, mut upv, mut globals) = {
            let fr = self.frames.last().unwrap();
            (fr.code, fr.consts, fr.ip, fr.base, fr.upvals, fr.globals)
        };

        macro_rules! reload {
            () => {{
                let fr = self.frames.last().unwrap();
                code = fr.code;
                consts = fr.consts;
                ip = fr.ip;
                base = fr.base;
                upv = fr.upvals;
                globals = fr.globals;
            }};
        }
        macro_rules! save_ip {
            () => {
                self.frames.last_mut().unwrap().ip = ip
            };
        }
        macro_rules! push {
            ($v:expr) => {{
                let v = $v;
                unsafe { *st.add(sp) = v };
                sp += 1;
            }};
        }
        macro_rules! pop {
            () => {{
                sp -= 1;
                unsafe { *st.add(sp) }
            }};
        }
        macro_rules! top {
            () => {
                unsafe { &mut *st.add(sp - 1) }
            };
        }
        macro_rules! local {
            ($s:expr) => {
                unsafe { &mut *st.add(base + $s as usize) }
            };
        }
        macro_rules! konst {
            ($k:expr) => {
                unsafe { *consts.add($k as usize) }
            };
        }
        macro_rules! sref {
            ($i:expr) => {
                unsafe { &*st.add($i) }
            };
        }
        macro_rules! lref {
            ($s:expr) => {
                unsafe { &*st.add(base + $s as usize) }
            };
        }
        macro_rules! kref {
            ($k:expr) => {
                unsafe { &*consts.add($k as usize) }
            };
        }
        macro_rules! jump_to {
            ($t:expr) => {
                ip = unsafe { code.add($t as usize) }
            };
        }
        // Replace the instruction that is currently executing (inline-cache update).
        macro_rules! rewrite {
            ($op:expr) => {
                unsafe { *(ip as *mut Op).sub(1) = $op }
            };
        }
        // Field `slot` of `obj` if it is an instance of the class with shape `cid`.
        macro_rules! instance_field {
            ($obj:expr, $cid:expr, $slot:expr) => {
                match $obj {
                    Value::Obj(g) => match g.get() {
                        Obj::Instance(inst) if inst.shape() == $cid => {
                            match inst.fields.get($slot as usize) {
                                Some(Value::Undef) | None => None,
                                Some(&v) => Some(v),
                            }
                        }
                        _ => None,
                    },
                    _ => None,
                }
            };
        }
        macro_rules! as_int {
            ($v:expr) => {
                match $v {
                    Value::Int(i) => i,
                    _ => 0,
                }
            };
        }
        // Enter bytecode function `$func` (called through `$owner`, with upvalues `$upv`)
        // whose frame starts at `$base_new`; `$ret` receives the result. Falls through if
        // the fast path can't apply (wrong arg count, defaults, stack limits).
        macro_rules! enter_fast {
            ($owner:expr, $f:expr, $upv:expr, $nargs:expr, $base_new:expr, $ret:expr) => {{
                let f: &Function = $f;
                let base_new: usize = $base_new;
                let top = base_new + f.num_locals as usize;
                if f.fast_arity as usize == $nargs
                    && (!native_mode || f.native_state == 2 || (!can_compile && f.jit.is_null()))
                    && self.frames.len() < MAX_FRAMES
                    && top + (f.max_stack as usize) < STACK_SLOTS
                {
                    for i in base_new + $nargs..top {
                        unsafe { *st.add(i) = Value::None };
                    }
                    code = f.code.as_ptr();
                    consts = f.consts.as_ptr();
                    ip = code;
                    base = base_new;
                    sp = top;
                    upv = $upv;
                    globals = self.unit_globals(f.unit);
                    self.frames.push(Frame {
                        owner: $owner,
                        code,
                        consts,
                        ip,
                        base,
                        ret: $ret,
                        upvals: upv,
                        globals,
                    });
                    continue;
                }
            }};
        }

        loop {
        let err: String = 'dispatch: loop {
            macro_rules! throw {
                ($e:expr) => {
                    break 'dispatch $e
                };
            }
            macro_rules! tri {
                ($e:expr) => {
                    match $e {
                        Ok(v) => v,
                        Err(m) => throw!(m),
                    }
                };
            }
            // A frame was just pushed: run it natively to completion (pushing its
            // result) if it was compiled, else continue interpreting inside it.
            macro_rules! entered {
                () => {{
                    if self.top_is_native() {
                        match self.run() {
                            Ok(v) => self.spush(v),
                            Err(e) => throw!(e),
                        }
                    } else {
                        reload!();
                    }
                }};
            }
            // Operands are passed as `&Value` places (stack slots, locals, constants) so the
            // fast paths read tag and payload straight from memory without copying.
            //
            // a OP b with int/float fast paths; slow path (strings, lists, overflow...) in `ops`.
            // Slow paths go through `binop_at`, which handles strings, lists, overflow
            // errors and user-defined operator methods. All operands live on the stack
            // or in locals/constants, so they stay visible to the GC during callbacks.
            macro_rules! arith {
                ($a:expr, $b:expr, $int:expr, $float:expr, $kind:expr) => {{
                    let (pa, pb): (&Value, &Value) = ($a, $b);
                    match (pa, pb) {
                        (Value::Int(x), Value::Int(y)) => match $int(*x, *y) {
                            Some(r) => Value::Int(r),
                            None => tri!(self.binop_at($kind, *pa, *pb, sp, ip)),
                        },
                        (Value::Float(x), Value::Float(y)) => Value::Float($float(*x, *y)),
                        (Value::Int(x), Value::Float(y)) => Value::Float($float(*x as f64, *y)),
                        (Value::Float(x), Value::Int(y)) => Value::Float($float(*x, *y as f64)),
                        _ => tri!(self.binop_at($kind, *pa, *pb, sp, ip)),
                    }
                }};
            }
            macro_rules! add {
                ($a:expr, $b:expr) => {
                    arith!($a, $b, |x: i64, y: i64| x.checked_add(y), |x: f64, y: f64| x + y, BinOp::Add)
                };
            }
            macro_rules! sub {
                ($a:expr, $b:expr) => {
                    arith!($a, $b, |x: i64, y: i64| x.checked_sub(y), |x: f64, y: f64| x - y, BinOp::Sub)
                };
            }
            macro_rules! mul {
                ($a:expr, $b:expr) => {
                    arith!($a, $b, |x: i64, y: i64| x.checked_mul(y), |x: f64, y: f64| x * y, BinOp::Mul)
                };
            }
            macro_rules! modulo {
                ($a:expr, $b:expr) => {{
                    let (pa, pb): (&Value, &Value) = ($a, $b);
                    match (pa, pb) {
                        (Value::Int(x), Value::Int(y)) => match ops::int_mod(*x, *y) {
                            Some(v) => Value::Int(v),
                            None => tri!(self.binop_at(BinOp::Mod, *pa, *pb, sp, ip)),
                        },
                        _ => tri!(self.binop_at(BinOp::Mod, *pa, *pb, sp, ip)),
                    }
                }};
            }
            // Ordering comparison -> bool.
            macro_rules! cmp {
                ($a:expr, $b:expr, $kind:expr, $pred:expr) => {{
                    let (pa, pb): (&Value, &Value) = ($a, $b);
                    match (pa, pb) {
                        (Value::Int(x), Value::Int(y)) => $pred(x.cmp(y)),
                        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).map_or(false, $pred),
                        (Value::Int(x), Value::Float(y)) => (*x as f64).partial_cmp(y).map_or(false, $pred),
                        (Value::Float(x), Value::Int(y)) => x.partial_cmp(&(*y as f64)).map_or(false, $pred),
                        _ => tri!(self.compare_at($kind, *pa, *pb, sp, ip)),
                    }
                }};
            }
            macro_rules! eq {
                ($a:expr, $b:expr) => {{
                    let (pa, pb): (&Value, &Value) = ($a, $b);
                    match (pa, pb) {
                        (Value::Int(x), Value::Int(y)) => x == y,
                        _ if is_instance(pa) || is_instance(pb) => {
                            tri!(self.compare_at(CmpOp::Eq, *pa, *pb, sp, ip))
                        }
                        _ => ops::equals(*pa, *pb),
                    }
                }};
            }
            macro_rules! test {
                (Eq, $a:expr, $b:expr) => { eq!($a, $b) };
                (Ne, $a:expr, $b:expr) => {{
                    let (pa, pb): (&Value, &Value) = ($a, $b);
                    if is_instance(pa) || is_instance(pb) {
                        tri!(self.compare_at(CmpOp::Ne, *pa, *pb, sp, ip))
                    } else {
                        !eq!(pa, pb)
                    }
                }};
                (Lt, $a:expr, $b:expr) => { cmp!($a, $b, CmpOp::Lt, |o: Ordering| o == Ordering::Less) };
                (Le, $a:expr, $b:expr) => { cmp!($a, $b, CmpOp::Le, |o: Ordering| o != Ordering::Greater) };
                (Gt, $a:expr, $b:expr) => { cmp!($a, $b, CmpOp::Gt, |o: Ordering| o == Ordering::Greater) };
                (Ge, $a:expr, $b:expr) => { cmp!($a, $b, CmpOp::Ge, |o: Ordering| o != Ordering::Less) };
            }
            // Truthiness; instances may define __bool__ / __len__. `$live` = stack height
            // that keeps `$v` visible to the GC.
            macro_rules! truth {
                ($v:expr, $live:expr) => {{
                    let v: Value = $v;
                    match v {
                        Value::Bool(b) => b,
                        Value::Obj(g) if matches!(g.get(), Obj::Instance(_)) => {
                            tri!(self.truthy_at(v, $live, ip))
                        }
                        _ => v.truthy(),
                    }
                }};
            }
            macro_rules! jump_unless {
                ($kind:ident, $a:expr, $b:expr, $t:expr) => {{
                    if !test!($kind, $a, $b) {
                        jump_to!($t);
                    }
                }};
            }
            // Binary op on the two top stack slots, result replaces them.
            macro_rules! binop_top {
                ($m:ident) => {{
                    let r = $m!(sref!(sp - 2), sref!(sp - 1));
                    sp -= 1;
                    *top!() = r;
                }};
            }
            // top = top OP const
            macro_rules! binop_const {
                ($m:ident, $k:expr) => {{
                    let r = $m!(sref!(sp - 1), kref!($k));
                    *top!() = r;
                }};
            }
            macro_rules! test_top {
                ($kind:ident) => {{
                    let r = test!($kind, sref!(sp - 2), sref!(sp - 1));
                    sp -= 1;
                    *top!() = Value::Bool(r);
                }};
            }
            macro_rules! test_const {
                ($kind:ident, $k:expr) => {{
                    let r = test!($kind, sref!(sp - 1), kref!($k));
                    *top!() = Value::Bool(r);
                }};
            }
            macro_rules! jump_unless_top {
                ($kind:ident, $t:expr) => {{
                    let r = test!($kind, sref!(sp - 2), sref!(sp - 1));
                    sp -= 2;
                    if !r {
                        jump_to!($t);
                    }
                }};
            }
            macro_rules! jump_if_const {
                ($kind:ident, $k:expr, $t:expr) => {{
                    let r = test!($kind, sref!(sp - 1), kref!($k));
                    sp -= 1;
                    if r {
                        jump_to!($t);
                    }
                }};
            }
            macro_rules! jump_unless_const {
                ($kind:ident, $k:expr, $t:expr) => {{
                    let r = test!($kind, sref!(sp - 1), kref!($k));
                    sp -= 1;
                    if !r {
                        jump_to!($t);
                    }
                }};
            }

            let op = unsafe { *ip };
            ip = unsafe { ip.add(1) };
            match op {
                Op::Const(k) => push!(konst!(k)),
                Op::None => push!(Value::None),
                Op::True => push!(Value::Bool(true)),
                Op::False => push!(Value::Bool(false)),
                Op::Pop => sp -= 1,

                // ---------------------------------------------------- variables
                Op::GetLocal(s) => push!(*local!(s)),
                Op::GetLocal2(a, b) => {
                    push!(*local!(a));
                    push!(*local!(b));
                }
                Op::SetLocal(s) => *local!(s) = pop!(),
                Op::AddLocal(s) => {
                    sp -= 1;
                    let r = add!(lref!(s), sref!(sp));
                    *local!(s) = r;
                }
                Op::AddLocalL(d, s) => {
                    let r = add!(lref!(d), lref!(s));
                    *local!(d) = r;
                }
                Op::AddLocalC(d, k) => {
                    let r = add!(lref!(d), kref!(k));
                    *local!(d) = r;
                }
                Op::GetGlobal(g) => {
                    let v = unsafe { *globals.add(g as usize) };
                    if let Value::Undef = v {
                        throw!(self.name_error(g));
                    }
                    push!(v);
                }
                Op::SetGlobal(g) => {
                    let v = pop!();
                    unsafe { *globals.add(g as usize) = v };
                }
                Op::GetUpval(i) => {
                    let u = unsafe { *upv.add(i as usize) };
                    push!(u.as_upval_mut().get());
                }
                Op::SetUpval(i) => {
                    let v = pop!();
                    let u = unsafe { *upv.add(i as usize) };
                    u.as_upval_mut().set(v);
                }

                // ---------------------------------------------------- attributes
                Op::GetAttrIC(n, cid, slot) => {
                    let obj = *top!();
                    *top!() = match instance_field!(obj, cid, slot) {
                        Some(v) => v,
                        None => {
                            let (v, cache) = tri!(self.load_attr(obj, n));
                            if let Some((cid, slot)) = cache {
                                rewrite!(Op::GetAttrIC(n, cid, slot));
                            }
                            v
                        }
                    };
                }
                Op::GetLocalAttrIC(n, cid, packed) => {
                    let a = packed & 0xffff;
                    let obj = *local!(a);
                    let v = match instance_field!(obj, cid, packed >> 16) {
                        Some(v) => v,
                        None => {
                            let (v, cache) = tri!(self.load_attr(obj, n));
                            if let Some((cid, slot)) = cache {
                                if slot < 0x10000 {
                                    rewrite!(Op::GetLocalAttrIC(n, cid, a | (slot << 16)));
                                }
                            }
                            v
                        }
                    };
                    push!(v);
                }
                Op::SetAttrIC(n, cid, slot) => {
                    let v = pop!();
                    let obj = pop!();
                    let mut done = false;
                    if let Value::Obj(g) = obj {
                        if let Obj::Instance(inst) = g.get_mut() {
                            if inst.shape() == cid {
                                let s = slot as usize;
                                if s >= inst.fields.len() {
                                    inst.fields.resize(s + 1, Value::Undef);
                                }
                                unsafe { *inst.fields.get_unchecked_mut(s) = v };
                                done = true;
                            }
                        }
                    }
                    if !done {
                        if let Some((cid, slot)) = tri!(self.store_attr(obj, n, v)) {
                            rewrite!(Op::SetAttrIC(n, cid, slot));
                        }
                    }
                }
                Op::SetLocalAttrIC(n, cid, packed) => {
                    let v = pop!();
                    let obj = *lref!(packed & 0xffff);
                    let mut done = false;
                    if let Value::Obj(g) = obj {
                        if let Obj::Instance(inst) = g.get_mut() {
                            if inst.shape() == cid {
                                let s = (packed >> 16) as usize;
                                if s >= inst.fields.len() {
                                    inst.fields.resize(s + 1, Value::Undef);
                                }
                                unsafe { *inst.fields.get_unchecked_mut(s) = v };
                                done = true;
                            }
                        }
                    }
                    if !done {
                        if let Some((cid, slot)) = tri!(self.store_attr(obj, n, v)) {
                            if slot < 0x10000 {
                                rewrite!(Op::SetLocalAttrIC(n, cid, (packed & 0xffff) | (slot << 16)));
                            }
                        }
                    }
                }
                Op::Dup => {
                    let v = *sref!(sp - 1);
                    push!(v);
                }
                Op::Neg => {
                    let r = match sref!(sp - 1) {
                        Value::Int(i) if *i != i64::MIN => Value::Int(-i),
                        Value::Float(f) => Value::Float(-f),
                        v => tri!(self.neg_at(*v, sp, ip)),
                    };
                    *top!() = r;
                }
                Op::GetIndex => {
                    let idx = pop!();
                    let obj = *top!();
                    let fast = match (obj, idx) {
                        (Value::Obj(g), Value::Int(i)) if i >= 0 => match g.get() {
                            Obj::List(l) => l.get(i as usize).copied(),
                            Obj::Tuple(t) => t.get(i as usize).copied(),
                            _ => None,
                        },
                        _ => None,
                    };
                    *top!() = match fast {
                        Some(v) => v,
                        None => tri!(self.get_item_at(obj, idx, sp + 1, ip)),
                    };
                }

                // ---------------------------------------------------- arithmetic
                Op::Add => binop_top!(add),
                Op::Sub => binop_top!(sub),
                Op::Mul => binop_top!(mul),
                Op::Mod => binop_top!(modulo),
                Op::AddC(k) => binop_const!(add, k),
                Op::SubC(k) => binop_const!(sub, k),
                Op::MulC(k) => binop_const!(mul, k),
                Op::ModC(k) => binop_const!(modulo, k),
                Op::AddLC(l, k) => {
                    let r = add!(lref!(l), kref!(k));
                    push!(r);
                }
                Op::SubLC(l, k) => {
                    let r = sub!(lref!(l), kref!(k));
                    push!(r);
                }
                Op::AddL(l) => {
                    let r = add!(sref!(sp - 1), lref!(l));
                    *top!() = r;
                }
                Op::SubL(l) => {
                    let r = sub!(sref!(sp - 1), lref!(l));
                    *top!() = r;
                }
                Op::MulL(l) => {
                    let r = mul!(sref!(sp - 1), lref!(l));
                    *top!() = r;
                }
                Op::AddLL(a, b) => {
                    let r = add!(lref!(a), lref!(b));
                    push!(r);
                }
                Op::SubLL(a, b) => {
                    let r = sub!(lref!(a), lref!(b));
                    push!(r);
                }
                Op::MulLL(a, b) => {
                    let r = mul!(lref!(a), lref!(b));
                    push!(r);
                }
                Op::MulLC(l, k) => {
                    let r = mul!(lref!(l), kref!(k));
                    push!(r);
                }
                Op::ModLC(l, k) => {
                    let r = modulo!(lref!(l), kref!(k));
                    push!(r);
                }
                Op::Div => {
                    let r = match (sref!(sp - 2), sref!(sp - 1)) {
                        (Value::Float(x), Value::Float(y)) if *y != 0.0 => Value::Float(x / y),
                        (a, b) => tri!(self.binop_at(BinOp::Div, *a, *b, sp, ip)),
                    };
                    sp -= 1;
                    *top!() = r;
                }
                Op::Not => {
                    let a = *top!();
                    *top!() = Value::Bool(!truth!(a, sp));
                }

                // ---------------------------------------------------- comparisons
                Op::Eq => test_top!(Eq),
                Op::Ne => test_top!(Ne),
                Op::Lt => test_top!(Lt),
                Op::Le => test_top!(Le),
                Op::Gt => test_top!(Gt),
                Op::Ge => test_top!(Ge),
                Op::EqC(k) => test_const!(Eq, k),
                Op::NeC(k) => test_const!(Ne, k),
                Op::LtC(k) => test_const!(Lt, k),
                Op::LeC(k) => test_const!(Le, k),
                Op::GtC(k) => test_const!(Gt, k),
                Op::GeC(k) => test_const!(Ge, k),

                // ---------------------------------------------------- jumps
                Op::Jump(t) => {
                    let target = unsafe { code.add(t as usize) };
                    if target < ip && self.heap.should_collect() {
                        self.sp = sp;
                        self.collect_garbage();
                    }
                    ip = target;
                }
                Op::JumpIfFalse(t) => {
                    let v = *sref!(sp - 1);
                    let falsy = !truth!(v, sp);
                    sp -= 1;
                    if falsy {
                        jump_to!(t);
                    }
                }
                Op::JumpIfTrue(t) => {
                    let v = *sref!(sp - 1);
                    let truthy = truth!(v, sp);
                    sp -= 1;
                    if truthy {
                        jump_to!(t);
                    }
                }
                Op::JumpIfFalseOrPop(t) => {
                    if !truth!(*top!(), sp) {
                        jump_to!(t);
                    } else {
                        sp -= 1;
                    }
                }
                Op::JumpIfTrueOrPop(t) => {
                    if truth!(*top!(), sp) {
                        jump_to!(t);
                    } else {
                        sp -= 1;
                    }
                }
                Op::JumpIfNotEq(t) => jump_unless_top!(Eq, t),
                Op::JumpIfNotNe(t) => jump_unless_top!(Ne, t),
                Op::JumpIfNotLt(t) => jump_unless_top!(Lt, t),
                Op::JumpIfNotLe(t) => jump_unless_top!(Le, t),
                Op::JumpIfNotGt(t) => jump_unless_top!(Gt, t),
                Op::JumpIfNotGe(t) => jump_unless_top!(Ge, t),
                Op::JumpIfEqC(k, t) => jump_if_const!(Eq, k, t),
                Op::JumpIfNeC(k, t) => jump_if_const!(Ne, k, t),
                Op::JumpIfLtC(k, t) => jump_if_const!(Lt, k, t),
                Op::JumpIfLeC(k, t) => jump_if_const!(Le, k, t),
                Op::JumpIfGtC(k, t) => jump_if_const!(Gt, k, t),
                Op::JumpIfGeC(k, t) => jump_if_const!(Ge, k, t),
                Op::JumpIfNotEqC(k, t) => jump_unless_const!(Eq, k, t),
                Op::JumpIfNotNeC(k, t) => jump_unless_const!(Ne, k, t),
                Op::JumpIfNotLtC(k, t) => jump_unless_const!(Lt, k, t),
                Op::JumpIfNotLeC(k, t) => jump_unless_const!(Le, k, t),
                Op::JumpIfNotGtC(k, t) => jump_unless_const!(Gt, k, t),
                Op::JumpIfNotGeC(k, t) => jump_unless_const!(Ge, k, t),
                Op::JumpIfNotEqLC(a, k, t) => jump_unless!(Eq, lref!(a), kref!(k), t),
                Op::JumpIfNotNeLC(a, k, t) => jump_unless!(Ne, lref!(a), kref!(k), t),
                Op::JumpIfNotLtLC(a, k, t) => jump_unless!(Lt, lref!(a), kref!(k), t),
                Op::JumpIfNotLeLC(a, k, t) => jump_unless!(Le, lref!(a), kref!(k), t),
                Op::JumpIfNotGtLC(a, k, t) => jump_unless!(Gt, lref!(a), kref!(k), t),
                Op::JumpIfNotGeLC(a, k, t) => jump_unless!(Ge, lref!(a), kref!(k), t),
                Op::JumpIfNotEqLL(a, b, t) => jump_unless!(Eq, lref!(a), lref!(b), t),
                Op::JumpIfNotNeLL(a, b, t) => jump_unless!(Ne, lref!(a), lref!(b), t),
                Op::JumpIfNotLtLL(a, b, t) => jump_unless!(Lt, lref!(a), lref!(b), t),
                Op::JumpIfNotLeLL(a, b, t) => jump_unless!(Le, lref!(a), lref!(b), t),
                Op::JumpIfNotGtLL(a, b, t) => jump_unless!(Gt, lref!(a), lref!(b), t),
                Op::JumpIfNotGeLL(a, b, t) => jump_unless!(Ge, lref!(a), lref!(b), t),

                // ---------------------------------------------------- loops
                Op::ForIter(s, var, exit) => match *local!(s) {
                    // range state: [Int(cur), Int(stop), Int(step)]
                    Value::Int(cur) => {
                        let stop = as_int!(*local!(s + 1));
                        let step = as_int!(*local!(s + 2));
                        if (step > 0 && cur < stop) || (step < 0 && cur > stop) {
                            *local!(var) = Value::Int(cur);
                            *local!(s) = Value::Int(cur.wrapping_add(step));
                        } else {
                            jump_to!(exit);
                        }
                    }
                    // list state: [list, Int(index), -]; generator state: [gen, -, -]
                    Value::Obj(g) => {
                        let idx = as_int!(*local!(s + 1)) as usize;
                        let next = match g.get() {
                            Obj::List(l) => l.get(idx).copied(),
                            Obj::Tuple(t) => t.get(idx).copied(),
                            Obj::Generator(_) => {
                                self.sp = sp;
                                save_ip!();
                                match self.resume_gen(g, Value::None) {
                                    Ok(v) => v,
                                    Err(e) => throw!(e),
                                }
                            }
                            _ => None,
                        };
                        match next {
                            Some(v) => {
                                *local!(var) = v;
                                *local!(s + 1) = Value::Int(idx as i64 + 1);
                            }
                            None => jump_to!(exit),
                        }
                    }
                    _ => jump_to!(exit),
                },
                Op::ForLoop(s, var, body) => {
                    let more = match *local!(s) {
                        Value::Int(cur) => {
                            let stop = as_int!(*local!(s + 1));
                            let step = as_int!(*local!(s + 2));
                            if (step > 0 && cur < stop) || (step < 0 && cur > stop) {
                                *local!(var) = Value::Int(cur);
                                *local!(s) = Value::Int(cur.wrapping_add(step));
                                true
                            } else {
                                false
                            }
                        }
                        Value::Obj(g) => {
                            let idx = as_int!(*local!(s + 1)) as usize;
                            let next = match g.get() {
                                Obj::List(l) => l.get(idx).copied(),
                                Obj::Tuple(t) => t.get(idx).copied(),
                                Obj::Generator(_) => {
                                    self.sp = sp;
                                    save_ip!();
                                    match self.resume_gen(g, Value::None) {
                                        Ok(v) => v,
                                        Err(e) => throw!(e),
                                    }
                                }
                                _ => None,
                            };
                            match next {
                                Some(v) => {
                                    *local!(var) = v;
                                    *local!(s + 1) = Value::Int(idx as i64 + 1);
                                    true
                                }
                                None => false,
                            }
                        }
                        _ => false,
                    };
                    if more {
                        // loop back-edge: a garbage-collection safe point
                        if self.heap.should_collect() {
                            self.sp = sp;
                            self.collect_garbage();
                        }
                        jump_to!(body);
                    }
                }
                Op::ListAppend(s) => {
                    let v = pop!();
                    if let Value::Obj(g) = *local!(s) {
                        if let Obj::List(l) = g.get_mut() {
                            l.push(v);
                        }
                    }
                }

                // ---------------------------------------------------- calls
                Op::Call(argc) => {
                    let argc = argc as usize;
                    let callee = sp - argc - 1;
                    save_ip!();
                    if self.heap.should_collect() {
                        self.sp = sp;
                        self.collect_garbage();
                    }
                    if let Value::Obj(g) = unsafe { *st.add(callee) } {
                        match g.get() {
                            Obj::Func(f) => enter_fast!(g, f, std::ptr::null(), argc, callee + 1, callee),
                            Obj::Closure(c) => {
                                enter_fast!(g, c.func.as_func(), c.upvals.as_ptr(), argc, callee + 1, callee)
                            }
                            _ => {}
                        }
                    }
                    self.sp = sp;
                    match self.call_value(callee, argc) {
                        Ok(true) => entered!(),
                        Ok(false) => {}
                        Err(e) => throw!(e),
                    }
                    sp = self.sp;
                }
                Op::InvokeIC(name, cid, packed) => {
                    let argc = (packed & 0xff) as usize;
                    let recv = sp - argc - 1;
                    save_ip!();
                    if self.heap.should_collect() {
                        self.sp = sp;
                        self.collect_garbage();
                    }
                    if let Value::Obj(g) = unsafe { *st.add(recv) } {
                        if let Obj::Instance(inst) = g.get() {
                            if inst.shape() == cid {
                                let cls = unsafe { &*inst.cls };
                                let m = unsafe { *cls.method_vals.get_unchecked((packed >> 8) as usize) };
                                if let Value::Obj(m) = m {
                                    match m.get() {
                                        Obj::Func(f) => {
                                            enter_fast!(m, f, std::ptr::null(), argc + 1, recv, recv)
                                        }
                                        Obj::Closure(c) => {
                                            enter_fast!(m, c.func.as_func(), c.upvals.as_ptr(), argc + 1, recv, recv)
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                    self.sp = sp;
                    match self.invoke(recv, name, argc) {
                        Ok(true) => entered!(),
                        Ok(false) => {}
                        Err(e) => throw!(e),
                    }
                    sp = self.sp;
                }
                Op::InvokeAppend(name) => {
                    let mut done = false;
                    if let Value::Obj(g) = *sref!(sp - 2) {
                        if let Obj::List(l) = g.get_mut() {
                            l.push(*sref!(sp - 1));
                            sp -= 1;
                            *top!() = Value::None;
                            done = true;
                        }
                    }
                    if !done {
                        save_ip!();
                        self.sp = sp;
                        match self.invoke(sp - 2, name, 1) {
                            Ok(true) => entered!(),
                            Ok(false) => {}
                            Err(e) => throw!(e),
                        }
                        sp = self.sp;
                    }
                }
                Op::SetupTry(t) => self.handlers.push(Handler {
                    frames: self.frames.len(),
                    sp,
                    target: t,
                }),
                Op::PopTry => {
                    self.handlers.pop();
                }
                Op::Return => {
                    let result = pop!();
                    let fr = self.frames.pop().unwrap();
                    if !self.open_upvals.is_empty() {
                        self.close_upvalues(fr.base);
                    }
                    sp = fr.ret;
                    if self.frames.len() == stop {
                        self.sp = sp;
                        return Ok(result);
                    }
                    push!(result);
                    reload!();
                }
                // A generator function was called: package the frame (its arguments are
                // already in its locals) into a generator object and return that.
                Op::Gen => {
                    let fr = self.frames.pop().unwrap();
                    let n = fr.func().as_func().num_locals as usize;
                    let saved = self.stack[fr.base..fr.base + n].to_vec();
                    let resume_at = unsafe { ip.offset_from(code) } as usize;
                    let g = self.heap.alloc(Obj::Generator(Box::new(Generator {
                        kind: GenKind::Frame { owner: fr.owner, saved, ip: resume_at, handlers: Vec::new() },
                        state: gen_state::CREATED,
                    })));
                    sp = fr.ret;
                    if self.frames.len() == stop {
                        self.sp = sp;
                        return Ok(Value::Obj(g));
                    }
                    push!(Value::Obj(g));
                    reload!();
                }
                // Suspend the running generator: save its frame (slots, ip, try blocks)
                // into the generator object and hand the value to `resume_gen`.
                Op::Yield => {
                    let v = pop!();
                    let g = match self.gen_stack.last() {
                        Some(&g) => g,
                        None => throw!("RuntimeError: 'yield' outside of a running generator".to_string()),
                    };
                    let depth = self.frames.len();
                    let fr = self.frames.pop().unwrap();
                    // Closures made inside the generator keep their own copy from here on.
                    if !self.open_upvals.is_empty() {
                        self.close_upvalues(fr.base);
                    }
                    let saved = self.stack[fr.base..sp].to_vec();
                    let resume_at = unsafe { ip.offset_from(code) } as usize;
                    let mut hs = Vec::new();
                    while self.handlers.last().is_some_and(|h| h.frames == depth) {
                        let h = self.handlers.pop().unwrap();
                        hs.push((h.sp - fr.base, h.target));
                    }
                    hs.reverse();
                    if let Obj::Generator(gen) = g.get_mut() {
                        gen.kind = GenKind::Frame { owner: fr.owner, saved, ip: resume_at, handlers: hs };
                        gen.state = gen_state::SUSPENDED;
                    }
                    sp = fr.ret;
                    if self.frames.len() == stop {
                        self.sp = sp;
                        return Ok(v);
                    }
                    // (not reached: generator frames are only entered by resume_gen)
                    push!(v);
                    reload!();
                }

                // ---------------------------------------------------- everything else
                cold => {
                    self.sp = sp;
                    save_ip!();
                    let depth = self.frames.len();
                    if let Err(e) = self.exec_cold(cold) {
                        throw!(e);
                    }
                    if self.frames.len() > depth && self.top_is_native() {
                        match self.run() {
                            Ok(v) => self.spush(v),
                            Err(e) => throw!(e),
                        }
                    }
                    reload!();
                    sp = self.sp;
                }
            }
        };

        // Exception: record where we were, then unwind to a handler in this run
        // (or return the error to our caller, which may have its own handlers).
        self.sp = sp;
        save_ip!();
        match self.handle_exception(err, stop) {
            Ok(target) => {
                reload!();
                jump_to!(target);
                sp = self.sp;
            }
            Err(e) => return Err(e),
        }
        }
    }
}
