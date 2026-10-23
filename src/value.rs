//! Runtime values and the garbage-collected heap.
//!
//! `Value` is a 16-byte `Copy` type: immediates (None/Bool/Int/Float) are stored
//! inline and everything else is a pointer to a heap object. Copying a value never
//! touches a reference count; unreachable objects (including cycles) are freed by a
//! mark-and-sweep collector that only runs at VM safe points.

use std::cell::Cell;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::ptr::NonNull;

use crate::bytecode::Op;
use crate::fx::FxMap;
use crate::vm::Vm;

pub type NativeFn = fn(&mut Vm, &[Value]) -> Result<Value, String>;

#[derive(Clone, Copy)]
#[repr(u64)] // full-width tag: no padding bytes to copy, two plain stores per write
pub enum Value {
    /// Internal marker for "no value" (unset globals / fields). Never user-visible.
    Undef,
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Obj(Gc),
}

/// Pointer to a heap object. Valid for as long as it is reachable from a GC root.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Gc(NonNull<GcBox>);

/// `repr(C)` so native code can read `kind` at a fixed offset.
#[repr(C)]
pub struct GcBox {
    marked: Cell<bool>,
    /// Which `Obj` variant this box holds (see `kind`), readable from native code.
    kind: u8,
    obj: Obj,
}

/// `GcBox::kind` values, one per `Obj` variant.
pub mod kind {
    pub const STR: u8 = 0;
    pub const LIST: u8 = 1;
    pub const DICT: u8 = 2;
    pub const RANGE: u8 = 3;
    pub const FUNC: u8 = 4;
    pub const NATIVE: u8 = 5;
    pub const CLASS: u8 = 6;
    pub const INSTANCE: u8 = 7;
    pub const BOUND: u8 = 8;
    pub const CLOSURE: u8 = 9;
    pub const UPVALUE: u8 = 10;
    pub const MODULE: u8 = 11;
    pub const TUPLE: u8 = 12;
    pub const SET: u8 = 13;
}

impl Obj {
    fn kind(&self) -> u8 {
        match self {
            Obj::Str(_) => kind::STR,
            Obj::List(_) => kind::LIST,
            Obj::Dict(_) => kind::DICT,
            Obj::Range(_) => kind::RANGE,
            Obj::Func(_) => kind::FUNC,
            Obj::Native(_) => kind::NATIVE,
            Obj::Class(_) => kind::CLASS,
            Obj::Instance(_) => kind::INSTANCE,
            Obj::Bound(_) => kind::BOUND,
            Obj::Closure(_) => kind::CLOSURE,
            Obj::Upvalue(_) => kind::UPVALUE,
            Obj::Module(_) => kind::MODULE,
            Obj::Tuple(_) => kind::TUPLE,
            Obj::Set(_) => kind::SET,
        }
    }
}

impl Gc {
    #[inline(always)]
    pub fn get<'a>(self) -> &'a Obj {
        unsafe { &(*self.0.as_ptr()).obj }
    }

    /// Mutable access. Callers keep the borrow short and never hold two
    /// overlapping borrows of the same object.
    #[inline(always)]
    #[allow(clippy::mut_from_ref)]
    pub fn get_mut<'a>(self) -> &'a mut Obj {
        unsafe { &mut (*self.0.as_ptr()).obj }
    }

    #[inline(always)]
    pub fn addr(self) -> usize {
        self.0.as_ptr() as usize
    }

    #[inline(always)]
    pub fn as_func<'a>(self) -> &'a Function {
        match self.get() {
            Obj::Func(f) => f,
            _ => unreachable!("not a function"),
        }
    }

    /// The function and upvalue array behind a callable bytecode value
    /// (plain function or closure).
    #[inline(always)]
    pub fn as_callable(self) -> Option<(Gc, *const Gc)> {
        match self.get() {
            Obj::Func(_) => Some((self, std::ptr::null())),
            Obj::Closure(c) => Some((c.func, c.upvals.as_ptr())),
            _ => None,
        }
    }

    #[inline(always)]
    #[allow(clippy::mut_from_ref)]
    pub fn as_upval_mut<'a>(self) -> &'a mut Upval {
        match self.get_mut() {
            Obj::Upvalue(u) => u,
            _ => unreachable!("not an upvalue"),
        }
    }

    #[inline(always)]
    pub fn as_class<'a>(self) -> &'a Class {
        match self.get() {
            Obj::Class(c) => c,
            _ => unreachable!("not a class"),
        }
    }

    #[inline(always)]
    #[allow(clippy::mut_from_ref)]
    pub fn as_class_mut<'a>(self) -> &'a mut Class {
        match self.get_mut() {
            Obj::Class(c) => c,
            _ => unreachable!("not a class"),
        }
    }
}

/// Heap object. Large payloads are boxed so every `GcBox` stays small (40 bytes)
/// and boxes can be recycled through a free list.
pub enum Obj {
    Str(String),
    List(Vec<Value>),
    Dict(Box<Dict>),
    Range(Range),
    Func(Box<Function>),
    Native(Native),
    Class(Box<Class>),
    Instance(Instance),
    Bound(Bound),
    Closure(Closure),
    Upvalue(Upval),
    Module(ModuleRef),
    /// Immutable sequence; hashable when its items are.
    Tuple(Box<[Value]>),
    /// Insertion-ordered set (a `Dict` whose values are unused).
    Set(Box<Dict>),
}

/// A function together with the variables it captured from enclosing functions.
pub struct Closure {
    pub func: Gc,
    pub upvals: Vec<Gc>,
    /// Defaults evaluated when the `def` ran (only for `dynamic_defaults` functions).
    pub defaults: Vec<Value>,
}

/// A captured variable. While the variable's frame is alive `ptr` points at its
/// stack slot ("open"); when the frame or block ends, the value is moved into
/// `closed` and `ptr` points there instead.
pub struct Upval {
    pub ptr: *mut Value,
    pub closed: Value,
    /// Absolute stack slot while open.
    pub slot: usize,
}

impl Upval {
    #[inline(always)]
    pub fn get(&self) -> Value {
        unsafe { *self.ptr }
    }

    #[inline(always)]
    pub fn set(&mut self, v: Value) {
        unsafe { *self.ptr = v }
    }

    pub fn close(&mut self) {
        self.closed = self.get();
        self.ptr = &mut self.closed;
    }
}

/// A module loaded from a `.fst` file; its variables live in the VM's unit table.
pub struct ModuleRef {
    pub unit: u32,
    pub name: String,
}

pub struct Function {
    pub name: String,
    pub arity: u32,
    /// Default values for the last `defaults.len()` parameters.
    pub defaults: Vec<Value>,
    pub code: Vec<Op>,
    pub lines: Vec<u32>,
    pub consts: Vec<Value>,
    pub num_locals: u32,
    pub max_stack: u32,
    /// Variables captured from enclosing functions: (from_parent_local, index).
    /// `true` = the parent's local slot, `false` = the parent's own upvalue.
    pub captures: Vec<(bool, u32)>,
    /// Compilation unit (module) whose globals this function uses.
    pub unit: u32,
    /// Default values are evaluated each time the `def` runs and stored on the closure.
    pub dynamic_defaults: bool,
    /// `arity` when a call with exactly that many positional arguments can enter the
    /// frame directly; u32::MAX for functions with `*args` / `**kwargs`.
    pub fast_arity: u32,
    /// Parameter names as symbols (for keyword arguments).
    pub params: Vec<u32>,
    /// Has `*args` (a tuple in the slot after the parameters).
    pub varargs: bool,
    /// Has `**kwargs` (a dict in the slot after that).
    pub varkw: bool,
    /// Native code for this function (null until the compiler has built it).
    pub jit: *const crate::vm::jit::JitInfo,
    /// 0 = not compiled yet, 1 = compiled, 2 = left to the interpreter.
    pub native_state: u8,
}

pub struct Native {
    pub name: &'static str,
    pub func: NativeFn,
    /// Keyword arguments it accepts (symbols); anything else is a TypeError.
    pub kwargs: &'static [u32],
}

pub struct Class {
    pub name: String,
    /// Shape id used by inline caches. Unique per class; replaced with a fresh id
    /// whenever the layout changes in a way that could invalidate a cached lookup.
    pub id: u32,
    /// Method name (symbol) -> index into `method_vals`.
    pub methods: FxMap<u32, u32>,
    pub method_vals: Vec<Value>,
    /// Field name (symbol) -> slot index in `Instance::fields`. Shared by all instances.
    pub slots: FxMap<u32, u32>,
    pub slot_count: u32,
    pub init: Option<Gc>,
    pub parent: Option<Gc>,
}

impl Class {
    pub fn new(name: String, id: u32) -> Class {
        Class {
            name,
            id,
            methods: FxMap::default(),
            method_vals: Vec::new(),
            slots: FxMap::default(),
            slot_count: 0,
            init: None,
            parent: None,
        }
    }

    #[inline]
    pub fn method(&self, name: u32) -> Option<Value> {
        self.methods.get(&name).map(|&i| self.method_vals[i as usize])
    }

    pub fn set_method(&mut self, name: u32, v: Value) {
        match self.methods.get(&name) {
            Some(&i) => self.method_vals[i as usize] = v,
            None => {
                self.methods.insert(name, self.method_vals.len() as u32);
                self.method_vals.push(v);
            }
        }
        if name == sym::INIT {
            self.init = match v {
                Value::Obj(g) if g.as_callable().is_some() => Some(g),
                _ => None,
            };
        }
    }

    /// Slot index for field `name`, adding it to the layout if needed.
    #[inline]
    pub fn slot_for(&mut self, name: u32, heap: &mut Heap) -> u32 {
        if let Some(&i) = self.slots.get(&name) {
            return i;
        }
        let i = self.slot_count;
        self.slots.insert(name, i);
        self.slot_count += 1;
        if self.methods.contains_key(&name) {
            // A field now shadows a method: cached method lookups must miss.
            self.id = heap.new_class_id();
        }
        i
    }
}

pub struct Instance {
    pub class: Gc,
    /// Direct pointer to the class data (the `Box<Class>` never moves and lives
    /// as long as `class`), so inline-cache checks are one load shorter.
    pub cls: *const Class,
    pub fields: Vec<Value>,
}

impl Instance {
    pub fn new(class: Gc, fields: Vec<Value>) -> Instance {
        let cls: *const Class = class.as_class();
        Instance { class, cls, fields }
    }

    /// Current shape id of this instance's class.
    #[inline(always)]
    pub fn shape(&self) -> u32 {
        unsafe { (*self.cls).id }
    }
}

pub struct Bound {
    pub recv: Value,
    pub method: Gc,
}

#[derive(Clone, Copy, PartialEq)]
pub struct Range {
    pub start: i64,
    pub stop: i64,
    pub step: i64,
}

impl Range {
    pub fn len(&self) -> i64 {
        if self.step > 0 && self.start < self.stop {
            (self.stop - self.start + self.step - 1) / self.step
        } else if self.step < 0 && self.start > self.stop {
            (self.start - self.stop - self.step - 1) / (-self.step)
        } else {
            0
        }
    }
}

// ---------------------------------------------------------------- symbols

/// Interned identifiers for attribute / method names. The first entries are
/// fixed so the builtins can match on them as integer constants.
pub mod sym {
    pub const INIT: u32 = 0;
    pub const APPEND: u32 = 1;
    pub const EXTEND: u32 = 2;
    pub const POP: u32 = 3;
    pub const INSERT: u32 = 4;
    pub const REMOVE: u32 = 5;
    pub const INDEX: u32 = 6;
    pub const COUNT: u32 = 7;
    pub const CLEAR: u32 = 8;
    pub const COPY: u32 = 9;
    pub const REVERSE: u32 = 10;
    pub const SORT: u32 = 11;
    pub const UPPER: u32 = 12;
    pub const LOWER: u32 = 13;
    pub const STRIP: u32 = 14;
    pub const LSTRIP: u32 = 15;
    pub const RSTRIP: u32 = 16;
    pub const SPLIT: u32 = 17;
    pub const JOIN: u32 = 18;
    pub const REPLACE: u32 = 19;
    pub const STARTSWITH: u32 = 20;
    pub const ENDSWITH: u32 = 21;
    pub const FIND: u32 = 22;
    pub const ISDIGIT: u32 = 23;
    pub const ISALPHA: u32 = 24;
    pub const ISSPACE: u32 = 25;
    pub const KEYS: u32 = 26;
    pub const VALUES: u32 = 27;
    pub const ITEMS: u32 = 28;
    pub const GET: u32 = 29;
    pub const UPDATE: u32 = 30;
    pub const D_ADD: u32 = 31;
    pub const D_SUB: u32 = 32;
    pub const D_MUL: u32 = 33;
    pub const D_TRUEDIV: u32 = 34;
    pub const D_FLOORDIV: u32 = 35;
    pub const D_MOD: u32 = 36;
    pub const D_POW: u32 = 37;
    pub const D_RADD: u32 = 38;
    pub const D_RSUB: u32 = 39;
    pub const D_RMUL: u32 = 40;
    pub const D_RTRUEDIV: u32 = 41;
    pub const D_RFLOORDIV: u32 = 42;
    pub const D_RMOD: u32 = 43;
    pub const D_RPOW: u32 = 44;
    pub const D_NEG: u32 = 45;
    pub const D_EQ: u32 = 46;
    pub const D_NE: u32 = 47;
    pub const D_LT: u32 = 48;
    pub const D_LE: u32 = 49;
    pub const D_GT: u32 = 50;
    pub const D_GE: u32 = 51;
    pub const D_STR: u32 = 52;
    pub const D_REPR: u32 = 53;
    pub const D_LEN: u32 = 54;
    pub const D_GETITEM: u32 = 55;
    pub const D_SETITEM: u32 = 56;
    pub const D_CONTAINS: u32 = 57;
    pub const D_CALL: u32 = 58;
    pub const D_BOOL: u32 = 59;
    pub const D_ITER: u32 = 60;
    pub const D_AND: u32 = 61;
    pub const D_OR: u32 = 62;
    pub const D_XOR: u32 = 63;
    pub const D_RAND: u32 = 64;
    pub const D_ROR: u32 = 65;
    pub const D_RXOR: u32 = 66;
    pub const MESSAGE: u32 = 67;
    pub const KEY: u32 = 68;
    pub const SEP: u32 = 69;
    pub const END: u32 = 70;
    pub const START: u32 = 71;
    pub const ADD: u32 = 72;
    pub const DISCARD: u32 = 73;
    pub const UNION: u32 = 74;
    pub const INTERSECTION: u32 = 75;
    pub const DIFFERENCE: u32 = 76;
    pub const SYMMETRIC_DIFFERENCE: u32 = 77;
    pub const ISSUBSET: u32 = 78;
    pub const ISSUPERSET: u32 = 79;
    pub const ISDISJOINT: u32 = 80;
    pub const DEFAULT: u32 = 81;

    pub const PREDEFINED: &[&str] = &[
        "init", "append", "extend", "pop", "insert", "remove", "index", "count", "clear",
        "copy", "reverse", "sort", "upper", "lower", "strip", "lstrip", "rstrip", "split",
        "join", "replace", "startswith", "endswith", "find", "isdigit", "isalpha", "isspace",
        "keys", "values", "items", "get", "update",
        "__add__", "__sub__", "__mul__", "__truediv__", "__floordiv__", "__mod__",
        "__pow__", "__radd__", "__rsub__", "__rmul__", "__rtruediv__", "__rfloordiv__",
        "__rmod__", "__rpow__", "__neg__", "__eq__", "__ne__", "__lt__",
        "__le__", "__gt__", "__ge__", "__str__", "__repr__", "__len__",
        "__getitem__", "__setitem__", "__contains__", "__call__", "__bool__", "__iter__",
        "__and__", "__or__", "__xor__", "__rand__", "__ror__", "__rxor__",
        "message", "key", "sep", "end", "start",
        "add", "discard", "union", "intersection", "difference", "symmetric_difference",
        "issubset", "issuperset", "isdisjoint", "default",
    ];
}

pub struct Interner {
    map: HashMap<String, u32>,
    names: Vec<String>,
}

impl Default for Interner {
    fn default() -> Self {
        let mut i = Interner {
            map: HashMap::new(),
            names: Vec::new(),
        };
        for (n, name) in sym::PREDEFINED.iter().enumerate() {
            let id = i.intern(name);
            debug_assert_eq!(id as usize, n);
        }
        i
    }
}

impl Interner {
    pub fn intern(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.map.get(s) {
            return id;
        }
        let id = self.names.len() as u32;
        self.map.insert(s.to_string(), id);
        self.names.push(s.to_string());
        id
    }

    pub fn name(&self, id: u32) -> &str {
        &self.names[id as usize]
    }
}

// ---------------------------------------------------------------- heap / GC

pub struct Heap {
    objects: Vec<NonNull<GcBox>>,
    /// Dead boxes (payload already dropped) ready for reuse.
    free: Vec<NonNull<GcBox>>,
    since_gc: usize,
    threshold: usize,
    gray: Vec<Gc>,
    next_class_id: u32,
}

const MIN_GC_THRESHOLD: usize = 1 << 17;

/// `FASTON_GC_STRESS=1` collects at nearly every safe point (for testing the GC).
fn min_threshold() -> usize {
    static STRESS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *STRESS.get_or_init(|| std::env::var_os("FASTON_GC_STRESS").is_some()) {
        1
    } else {
        MIN_GC_THRESHOLD
    }
}

impl Default for Heap {
    fn default() -> Self {
        Heap {
            objects: Vec::with_capacity(1024),
            free: Vec::new(),
            since_gc: 0,
            threshold: min_threshold(),
            gray: Vec::new(),
            next_class_id: 1,
        }
    }
}

impl Heap {
    #[inline]
    pub fn alloc(&mut self, obj: Obj) -> Gc {
        let fresh = GcBox {
            marked: Cell::new(false),
            kind: obj.kind(),
            obj,
        };
        let p = match self.free.pop() {
            Some(p) => {
                unsafe { std::ptr::write(p.as_ptr(), fresh) };
                p
            }
            None => NonNull::from(Box::leak(Box::new(fresh))),
        };
        self.objects.push(p);
        self.since_gc += 1;
        Gc(p)
    }

    pub fn new_class_id(&mut self) -> u32 {
        let id = self.next_class_id;
        self.next_class_id += 1;
        id
    }

    pub fn string(&mut self, s: impl Into<String>) -> Value {
        Value::Obj(self.alloc(Obj::Str(s.into())))
    }

    pub fn list(&mut self, v: Vec<Value>) -> Value {
        Value::Obj(self.alloc(Obj::List(v)))
    }

    pub fn dict(&mut self, d: Dict) -> Value {
        Value::Obj(self.alloc(Obj::Dict(Box::new(d))))
    }

    pub fn tuple(&mut self, v: Vec<Value>) -> Value {
        Value::Obj(self.alloc(Obj::Tuple(v.into_boxed_slice())))
    }

    pub fn set(&mut self, d: Dict) -> Value {
        Value::Obj(self.alloc(Obj::Set(Box::new(d))))
    }

    #[inline(always)]
    pub fn should_collect(&self) -> bool {
        self.since_gc >= self.threshold
    }

    #[inline]
    pub fn mark(&mut self, v: Value) {
        if let Value::Obj(g) = v {
            self.mark_gc(g);
        }
    }

    #[inline]
    pub fn mark_gc(&mut self, g: Gc) {
        let b = unsafe { &*g.0.as_ptr() };
        if !b.marked.get() {
            b.marked.set(true);
            self.gray.push(g);
        }
    }

    /// Marks everything reachable from the already-marked roots, then frees the rest.
    pub fn finish_collection(&mut self) {
        while let Some(g) = self.gray.pop() {
            match g.get() {
                Obj::Str(_) | Obj::Range(_) | Obj::Native(_) => {}
                Obj::List(v) => {
                    for x in v.iter() {
                        self.mark(*x);
                    }
                }
                Obj::Dict(d) => {
                    for (k, v) in d.entries.iter() {
                        self.mark(*k);
                        self.mark(*v);
                    }
                }
                Obj::Func(f) => {
                    for c in f.consts.iter().chain(f.defaults.iter()) {
                        self.mark(*c);
                    }
                }
                Obj::Class(c) => {
                    for m in c.method_vals.iter() {
                        self.mark(*m);
                    }
                    if let Some(p) = c.parent {
                        self.mark_gc(p);
                    }
                }
                Obj::Instance(i) => {
                    self.mark_gc(i.class);
                    for f in i.fields.iter() {
                        self.mark(*f);
                    }
                }
                Obj::Bound(b) => {
                    self.mark(b.recv);
                    self.mark_gc(b.method);
                }
                Obj::Closure(c) => {
                    self.mark_gc(c.func);
                    for u in c.upvals.iter() {
                        self.mark_gc(*u);
                    }
                    for d in c.defaults.iter() {
                        self.mark(*d);
                    }
                }
                // An open upvalue's variable is on the stack (already a root).
                Obj::Upvalue(u) => self.mark(u.closed),
                Obj::Module(_) => {}
                Obj::Tuple(t) => {
                    for x in t.iter() {
                        self.mark(*x);
                    }
                }
                Obj::Set(d) => {
                    for (k, _) in d.entries.iter() {
                        self.mark(*k);
                    }
                }
            }
        }

        let free = &mut self.free;
        self.objects.retain(|p| {
            let b = unsafe { &mut *p.as_ptr() };
            if b.marked.get() {
                b.marked.set(false);
                true
            } else {
                unsafe { std::ptr::drop_in_place(&mut b.obj) };
                free.push(*p);
                false
            }
        });
        self.since_gc = 0;
        let min = min_threshold();
        self.threshold = if min == 1 { 1 } else { (self.objects.len() * 2).max(min) };
    }
}

impl Drop for Heap {
    fn drop(&mut self) {
        for p in self.objects.drain(..) {
            unsafe { drop(Box::from_raw(p.as_ptr())) };
        }
        // Free-list boxes hold no live payload: release their memory only.
        for p in self.free.drain(..) {
            unsafe { std::alloc::dealloc(p.as_ptr() as *mut u8, std::alloc::Layout::new::<GcBox>()) };
        }
    }
}

// ---------------------------------------------------------------- dicts

/// Hashable form of a value. Numbers that compare equal hash equal (1 == 1.0 == True),
/// strings hash by content, functions / classes / instances by identity.
#[derive(Clone, Copy)]
pub enum Key {
    None,
    Int(i64),
    Float(u64),
    Str(Gc),
    Ptr(usize),
    /// A tuple whose items are all hashable (checked when the key is made).
    Tuple(Gc),
}

impl Key {
    fn str_of(g: Gc) -> &'static str {
        match g.get() {
            Obj::Str(s) => s.as_str(),
            _ => "",
        }
    }

    fn items_of(g: Gc) -> &'static [Value] {
        match g.get() {
            Obj::Tuple(t) => t,
            _ => &[],
        }
    }
}

impl PartialEq for Key {
    fn eq(&self, other: &Key) -> bool {
        match (self, other) {
            (Key::None, Key::None) => true,
            (Key::Int(a), Key::Int(b)) => a == b,
            (Key::Float(a), Key::Float(b)) => a == b,
            (Key::Str(a), Key::Str(b)) => a == b || Key::str_of(*a) == Key::str_of(*b),
            (Key::Ptr(a), Key::Ptr(b)) => a == b,
            (Key::Tuple(a), Key::Tuple(b)) => {
                let (x, y) = (Key::items_of(*a), Key::items_of(*b));
                a == b
                    || (x.len() == y.len()
                        && x.iter().zip(y.iter()).all(|(p, q)| match (to_key(*p), to_key(*q)) {
                            (Ok(kp), Ok(kq)) => kp == kq,
                            _ => false,
                        }))
            }
            _ => false,
        }
    }
}

impl Eq for Key {}

impl Hash for Key {
    fn hash<H: Hasher>(&self, h: &mut H) {
        match self {
            Key::None => h.write_u8(0),
            Key::Int(i) => {
                h.write_u8(1);
                h.write_i64(*i);
            }
            Key::Float(f) => {
                h.write_u8(2);
                h.write_u64(*f);
            }
            Key::Str(g) => {
                h.write_u8(3);
                Key::str_of(*g).hash(h);
            }
            Key::Ptr(p) => {
                h.write_u8(4);
                h.write_usize(*p);
            }
            Key::Tuple(g) => {
                h.write_u8(5);
                for x in Key::items_of(*g) {
                    if let Ok(k) = to_key(*x) {
                        k.hash(h);
                    }
                }
            }
        }
    }
}

pub fn to_key(v: Value) -> Result<Key, String> {
    Ok(match v {
        Value::None => Key::None,
        Value::Bool(b) => Key::Int(b as i64),
        Value::Int(i) => Key::Int(i),
        Value::Float(f) => {
            if f.fract() == 0.0 && f.abs() < 9.0e18 {
                Key::Int(f as i64)
            } else {
                Key::Float(f.to_bits())
            }
        }
        Value::Obj(g) => match g.get() {
            Obj::Str(_) => Key::Str(g),
            Obj::Func(_) | Obj::Native(_) | Obj::Class(_) | Obj::Instance(_) | Obj::Closure(_)
            | Obj::Module(_) => Key::Ptr(g.addr()),
            Obj::Tuple(t) => {
                for x in t.iter() {
                    to_key(*x)?;
                }
                Key::Tuple(g)
            }
            _ => return Err(format!("TypeError: unhashable type: '{}'", v.type_name())),
        },
        Value::Undef => return Err("TypeError: unhashable value".into()),
    })
}

/// Insertion-ordered dictionary.
#[derive(Default, Clone)]
pub struct Dict {
    map: FxMap<Key, usize>,
    pub entries: Vec<(Value, Value)>,
}

impl Dict {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn get(&self, key: &Key) -> Option<Value> {
        self.map.get(key).map(|&i| self.entries[i].1)
    }

    pub fn contains(&self, key: &Key) -> bool {
        self.map.contains_key(key)
    }

    pub fn insert(&mut self, key: Key, k: Value, v: Value) {
        if let Some(&i) = self.map.get(&key) {
            self.entries[i].1 = v;
        } else {
            self.map.insert(key, self.entries.len());
            self.entries.push((k, v));
        }
    }

    pub fn remove(&mut self, key: &Key) -> Option<Value> {
        let i = self.map.remove(key)?;
        let (_, v) = self.entries.remove(i);
        for idx in self.map.values_mut() {
            if *idx > i {
                *idx -= 1;
            }
        }
        Some(v)
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.entries.clear();
    }
}

// ---------------------------------------------------------------- helpers

impl Value {
    #[inline]
    pub fn as_str<'a>(self) -> Option<&'a str> {
        match self {
            Value::Obj(g) => match g.get() {
                Obj::Str(s) => Some(s.as_str()),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn type_name(&self) -> String {
        match self {
            Value::Undef => "undefined".into(),
            Value::None => "NoneType".into(),
            Value::Bool(_) => "bool".into(),
            Value::Int(_) => "int".into(),
            Value::Float(_) => "float".into(),
            Value::Obj(g) => match g.get() {
                Obj::Str(_) => "str".into(),
                Obj::List(_) => "list".into(),
                Obj::Dict(_) => "dict".into(),
                Obj::Tuple(_) => "tuple".into(),
                Obj::Set(_) => "set".into(),
                Obj::Range(_) => "range".into(),
                Obj::Func(_) => "function".into(),
                Obj::Native(_) => "builtin_function".into(),
                Obj::Class(_) => "class".into(),
                Obj::Instance(i) => i.class.as_class().name.clone(),
                Obj::Bound(_) => "method".into(),
                Obj::Closure(_) => "function".into(),
                Obj::Upvalue(_) => "upvalue".into(),
                Obj::Module(_) => "module".into(),
            },
        }
    }

    #[inline]
    pub fn truthy(self) -> bool {
        match self {
            Value::Undef | Value::None => false,
            Value::Bool(b) => b,
            Value::Int(i) => i != 0,
            Value::Float(f) => f != 0.0,
            Value::Obj(g) => match g.get() {
                Obj::Str(s) => !s.is_empty(),
                Obj::List(l) => !l.is_empty(),
                Obj::Dict(d) | Obj::Set(d) => d.len() != 0,
                Obj::Tuple(t) => !t.is_empty(),
                Obj::Range(r) => r.len() != 0,
                _ => true,
            },
        }
    }

    /// `str(x)` / print form.
    pub fn to_str(&self) -> String {
        match self.as_str() {
            Some(s) => s.to_string(),
            None => self.repr(),
        }
    }

    /// Appends the `str(x)` form to `out` without an intermediate allocation.
    pub fn write_str(&self, out: &mut String) {
        match self.as_str() {
            Some(s) => out.push_str(s),
            None => self.write_repr(out, 0),
        }
    }

    /// `repr(x)` form: strings are quoted.
    pub fn repr(&self) -> String {
        let mut out = String::new();
        self.write_repr(&mut out, 0);
        out
    }

    pub fn write_repr(&self, out: &mut String, depth: usize) {
        if depth > 64 {
            out.push_str("...");
            return;
        }
        match self {
            Value::Undef => out.push_str("<undefined>"),
            Value::None => out.push_str("None"),
            Value::Bool(true) => out.push_str("True"),
            Value::Bool(false) => out.push_str("False"),
            Value::Int(i) => push_int(out, *i),
            Value::Float(f) => out.push_str(&fmt_float(*f)),
            Value::Obj(g) => match g.get() {
                Obj::Str(s) => {
                    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
                    out.push(q);
                    for c in s.chars() {
                        match c {
                            '\n' => out.push_str("\\n"),
                            '\t' => out.push_str("\\t"),
                            '\r' => out.push_str("\\r"),
                            '\\' => out.push_str("\\\\"),
                            c if c == q => {
                                out.push('\\');
                                out.push(c);
                            }
                            c => out.push(c),
                        }
                    }
                    out.push(q);
                }
                Obj::List(l) => {
                    out.push('[');
                    for (i, v) in l.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        v.write_repr(out, depth + 1);
                    }
                    out.push(']');
                }
                Obj::Dict(d) => {
                    out.push('{');
                    for (i, (k, v)) in d.entries.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        k.write_repr(out, depth + 1);
                        out.push_str(": ");
                        v.write_repr(out, depth + 1);
                    }
                    out.push('}');
                }
                Obj::Tuple(t) => {
                    out.push('(');
                    for (i, v) in t.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        v.write_repr(out, depth + 1);
                    }
                    if t.len() == 1 {
                        out.push(',');
                    }
                    out.push(')');
                }
                Obj::Set(d) => {
                    if d.len() == 0 {
                        out.push_str("set()");
                    } else {
                        out.push('{');
                        for (i, (k, _)) in d.entries.iter().enumerate() {
                            if i > 0 {
                                out.push_str(", ");
                            }
                            k.write_repr(out, depth + 1);
                        }
                        out.push('}');
                    }
                }
                Obj::Range(r) => {
                    if r.step == 1 {
                        let _ = write!(out, "range({}, {})", r.start, r.stop);
                    } else {
                        let _ = write!(out, "range({}, {}, {})", r.start, r.stop, r.step);
                    }
                }
                Obj::Func(f) => {
                    let _ = write!(out, "<function {}>", f.name);
                }
                Obj::Native(n) => {
                    let _ = write!(out, "<built-in function {}>", n.name);
                }
                Obj::Class(c) => {
                    let _ = write!(out, "<class '{}'>", c.name);
                }
                Obj::Instance(i) => {
                    let _ = write!(out, "<{} object>", i.class.as_class().name);
                }
                Obj::Bound(b) => {
                    let _ = write!(
                        out,
                        "<bound method {}.{}>",
                        b.recv.type_name(),
                        callable_name(b.method)
                    );
                }
                Obj::Closure(c) => {
                    let _ = write!(out, "<function {}>", c.func.as_func().name);
                }
                Obj::Upvalue(_) => out.push_str("<upvalue>"),
                Obj::Module(m) => {
                    let _ = write!(out, "<module '{}'>", m.name);
                }
            },
        }
    }
}

/// Name of a function or closure (for messages).
pub fn callable_name(g: Gc) -> String {
    match g.as_callable() {
        Some((f, _)) => f.as_func().name.clone(),
        None => "?".into(),
    }
}

/// Formats a float exactly like Python's `repr`: shortest round-trip digits,
/// positional for exponents in [-4, 16), scientific (`1e+16`, `1e-05`) otherwise.
/// Fast decimal formatting for ints (avoids the generic `fmt` machinery).
#[inline]
pub fn push_int(out: &mut String, i: i64) {
    let mut buf = [0u8; 20];
    let mut pos = buf.len();
    let neg = i < 0;
    let mut n = i.unsigned_abs();
    loop {
        pos -= 1;
        buf[pos] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    if neg {
        out.push('-');
    }
    // SAFETY: only ASCII digits were written.
    out.push_str(unsafe { std::str::from_utf8_unchecked(&buf[pos..]) });
}

pub fn fmt_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    // Shortest correctly-rounded digits that round-trip (Python's repr rule; ties
    // go to even, which `{:e}` alone doesn't guarantee).
    let mut sci = format!("{:e}", f);
    for p in 0..17 {
        let cand = format!("{:.*e}", p, f);
        if cand.parse::<f64>() == Ok(f) {
            sci = cand;
            break;
        }
    }
    let (mant, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let neg = mant.starts_with('-');
    let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
    let sign = if neg { "-" } else { "" };
    if (-4..16).contains(&exp) {
        let point = exp + 1; // digits before the decimal point
        let body = if point <= 0 {
            format!("0.{}{}", "0".repeat((-point) as usize), digits)
        } else if point as usize >= digits.len() {
            format!("{}{}.0", digits, "0".repeat(point as usize - digits.len()))
        } else {
            format!("{}.{}", &digits[..point as usize], &digits[point as usize..])
        };
        format!("{}{}", sign, body)
    } else {
        let m = if digits.len() > 1 {
            format!("{}.{}", &digits[..1], &digits[1..])
        } else {
            digits
        };
        format!("{}{}e{}{:02}", sign, m, if exp < 0 { '-' } else { '+' }, exp.abs())
    }
}

// ---------------------------------------------------------------- layout for native code

/// Byte offsets of the object fields that compiled code reads directly. Rust lays
/// out most of these types as it likes, so the offsets are measured at startup on
/// real values instead of being assumed.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub kind: i32,
    pub inst_cls: i32,
    pub inst_fields_ptr: i32,
    pub inst_fields_len: i32,
    pub class_id: i32,
    pub class_methods_ptr: i32,
    pub list_ptr: i32,
    pub list_len: i32,
    pub list_cap: i32,
    pub tuple_ptr: i32,
    pub tuple_len: i32,
    pub func_box: i32,
    pub fn_jit: i32,
    pub closure_func: i32,
    pub closure_upvals_ptr: i32,
    pub upval_ptr: i32,
    pub range_start: i32,
    pub range_stop: i32,
    pub range_step: i32,
    pub heap_since_gc: i32,
    pub heap_threshold: i32,
    pub class_box: i32,
    pub class_init: i32,
}

/// (pointer, length) byte offsets inside a `Vec`. Needs `len != capacity`.
pub fn vec_offsets<T>(v: &Vec<T>) -> (i32, i32) {
    let (p, l, _) = vec_offsets3(v);
    (p, l)
}

/// (pointer, length, capacity) byte offsets inside a `Vec`.
pub fn vec_offsets3<T>(v: &Vec<T>) -> (i32, i32, i32) {
    assert_eq!(std::mem::size_of::<Vec<T>>(), 24);
    let words: [usize; 3] = unsafe { std::mem::transmute_copy(v) };
    let (ptr, cap, len) = (v.as_ptr() as usize, v.capacity(), v.len());
    assert!(len != cap && ptr != len && ptr != cap);
    let p = words.iter().position(|&w| w == ptr).expect("vec layout");
    let c = words.iter().position(|&w| w == cap).expect("vec layout");
    let l = (0..3).find(|&i| i != p && i != c).unwrap();
    assert_eq!(words[l], len);
    ((p * 8) as i32, (l * 8) as i32, (c * 8) as i32)
}

fn off<T, U>(base: &T, field: &U) -> i32 {
    (field as *const U as usize - base as *const T as usize) as i32
}

pub fn layout() -> Layout {
    use std::mem::{offset_of, ManuallyDrop};
    let mut vals: Vec<Value> = Vec::with_capacity(7);
    vals.push(Value::None);
    vals.push(Value::None);
    let (vptr, vlen, vcap) = vec_offsets3(&vals);
    let boxed = |obj: Obj| ManuallyDrop::new(GcBox { marked: Cell::new(false), kind: 0, obj });
    let fake = Gc(NonNull::dangling());

    let b = boxed(Obj::Str(String::new()));
    let kind = off(&*b, &b.kind);

    let b = boxed(Obj::Instance(Instance { class: fake, cls: std::ptr::null(), fields: Vec::new() }));
    let (inst_cls, inst_fields) = match &b.obj {
        Obj::Instance(i) => (off(&*b, &i.cls), off(&*b, &i.fields)),
        _ => unreachable!(),
    };
    let b = boxed(Obj::List(Vec::new()));
    let list = match &b.obj {
        Obj::List(l) => off(&*b, l),
        _ => unreachable!(),
    };
    let t: Box<[Value]> = vec![Value::None; 3].into_boxed_slice();
    let words: [usize; 2] = unsafe { std::mem::transmute_copy(&t) };
    let tp = words.iter().position(|&w| w == t.as_ptr() as usize).expect("slice layout");
    assert_eq!(words[1 - tp], 3);
    let b = boxed(Obj::Tuple(t));
    let tuple = match &b.obj {
        Obj::Tuple(t) => off(&*b, t),
        _ => unreachable!(),
    };
    let b = boxed(Obj::Closure(Closure { func: fake, upvals: Vec::new(), defaults: Vec::new() }));
    let (closure_func, closure_upvals) = match &b.obj {
        Obj::Closure(c) => (off(&*b, &c.func), off(&*b, &c.upvals)),
        _ => unreachable!(),
    };
    let b = boxed(Obj::Upvalue(Upval { ptr: std::ptr::null_mut(), closed: Value::None, slot: 0 }));
    let upval_ptr = match &b.obj {
        Obj::Upvalue(u) => off(&*b, &u.ptr),
        _ => unreachable!(),
    };
    let b = boxed(Obj::Range(Range { start: 0, stop: 0, step: 0 }));
    let (rs, rp, rt) = match &b.obj {
        Obj::Range(r) => (off(&*b, &r.start), off(&*b, &r.stop), off(&*b, &r.step)),
        _ => unreachable!(),
    };
    // A Func box holds a Box<Function>: find the pointer word by value.
    let f = Function {
        name: String::new(),
        arity: 0,
        defaults: Vec::new(),
        code: Vec::new(),
        lines: Vec::new(),
        consts: Vec::new(),
        num_locals: 0,
        max_stack: 0,
        captures: Vec::new(),
        unit: 0,
        dynamic_defaults: false,
        fast_arity: 0,
        params: Vec::new(),
        varargs: false,
        varkw: false,
        jit: std::ptr::null(),
        native_state: 0,
    };
    let b = boxed(Obj::Func(Box::new(f)));
    let func_box = match &b.obj {
        Obj::Func(f) => off(&*b, f),
        _ => unreachable!(),
    };
    let fptr: usize = unsafe { *((&*b as *const GcBox as *const u8).add(func_box as usize) as *const usize) };
    match &b.obj {
        Obj::Func(f) => assert_eq!(fptr, &**f as *const Function as usize),
        _ => unreachable!(),
    }
    // release the probes' heap memory
    for mut b in [b] {
        unsafe { ManuallyDrop::drop(&mut b) };
    }
    let b = boxed(Obj::Class(Box::new(Class::new(String::new(), 0))));
    let class_box = match &b.obj {
        Obj::Class(c) => off(&*b, c),
        _ => unreachable!(),
    };
    let none: Option<Gc> = None;
    assert_eq!(unsafe { std::mem::transmute::<Option<Gc>, usize>(none) }, 0);
    Layout {
        kind,
        inst_cls,
        inst_fields_ptr: inst_fields + vptr,
        inst_fields_len: inst_fields + vlen,
        class_id: offset_of!(Class, id) as i32,
        class_methods_ptr: offset_of!(Class, method_vals) as i32 + vptr,
        list_ptr: list + vptr,
        list_len: list + vlen,
        list_cap: list + vcap,
        tuple_ptr: tuple + (tp * 8) as i32,
        tuple_len: tuple + ((1 - tp) * 8) as i32,
        func_box,
        fn_jit: offset_of!(Function, jit) as i32,
        closure_func,
        closure_upvals_ptr: closure_upvals + vptr,
        upval_ptr,
        range_start: rs,
        range_stop: rp,
        range_step: rt,
        heap_since_gc: offset_of!(Heap, since_gc) as i32,
        heap_threshold: offset_of!(Heap, threshold) as i32,
        class_box,
        class_init: offset_of!(Class, init) as i32,
    }
}

/// In-memory tags of `Value` variants (`repr(u64)`: tag word, then payload word).
pub mod tag {
    pub const UNDEF: i64 = 0;
    pub const NONE: i64 = 1;
    pub const BOOL: i64 = 2;
    pub const INT: i64 = 3;
    pub const FLOAT: i64 = 4;
    pub const OBJ: i64 = 5;
}

/// Checks the `Value` encoding the native code generator assumes.
pub fn check_value_encoding() {
    let tag_of = |v: Value| -> u64 { unsafe { *(&v as *const Value as *const u64) } };
    let words = |v: Value| -> [u64; 2] { unsafe { std::mem::transmute::<Value, [u64; 2]>(v) } };
    assert_eq!(std::mem::size_of::<Value>(), 16);
    assert_eq!(tag_of(Value::Undef), tag::UNDEF as u64);
    assert_eq!(tag_of(Value::None), tag::NONE as u64);
    assert_eq!(tag_of(Value::Bool(true)), tag::BOOL as u64);
    let b = Value::Bool(true);
    assert_eq!(unsafe { *(&b as *const Value as *const u8).add(8) }, 1);
    assert_eq!(words(Value::Int(-7)), [tag::INT as u64, -7i64 as u64]);
    assert_eq!(words(Value::Float(1.5)), [tag::FLOAT as u64, 1.5f64.to_bits()]);
    let g = Gc(NonNull::dangling());
    assert_eq!(words(Value::Obj(g)), [tag::OBJ as u64, g.addr() as u64]);
}
