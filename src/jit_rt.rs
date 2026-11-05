//! The part of native code support that a program needs at run time: the
//! descriptors and runtime helpers compiled code calls, running it, and loading
//! machine code that `faston build` compiled ahead of time (so built programs
//! don't need the code generator). The code generator itself is jit.rs.

use super::*;
use crate::value::Layout;

/// Compiled function: (vm, its frame, its descriptor) -> status.
pub(super) type Entry = unsafe extern "C" fn(*mut Vm, *mut Frame, *const JitInfo) -> u64;

/// Per-function native code descriptor (`Function::jit` points at one).
#[repr(C)]
pub struct JitInfo {
    pub entry: Entry,
    pub code: *const Op,
    pub consts: *const Value,
    pub globals: *mut Value,
    pub fast_arity: u64,
    pub num_locals: u64,
    /// Stack slots the frame may use: locals + operand stack.
    pub frame_need: u64,
    /// Addresses ahead-of-time compiled code loads instead of embedding them
    /// (null for code compiled in this process).
    pub(super) table: *const u64,
    pub(super) table_store: Box<[u64]>,
    /// Pristine copy of the bytecode (the interpreter rewrites the real one with
    /// inline-cache forms); helpers are handed instructions from here.
    pub(super) ops: Box<[Op]>,
    /// Inline-cache cells, two `u32`s per site.
    pub(super) cells: Box<[u32]>,
}

/// Byte offsets compiled code needs.
#[derive(Clone, Copy, Debug)]
pub(super) struct Off {
    pub(super) l: Layout,
    pub(super) fr_owner: i32,
    pub(super) fr_code: i32,
    pub(super) fr_consts: i32,
    pub(super) fr_ip: i32,
    pub(super) fr_base: i32,
    pub(super) fr_ret: i32,
    pub(super) fr_upvals: i32,
    pub(super) fr_globals: i32,
    pub(super) frame_size: i64,
    pub(super) vm_stack_ptr: i32,
    pub(super) vm_frames_ptr: i32,
    pub(super) vm_frames_len: i32,
    pub(super) vm_since_gc: i32,
    pub(super) vm_threshold: i32,
    pub(super) info_entry: i32,
    pub(super) info_code: i32,
    pub(super) info_consts: i32,
    pub(super) info_globals: i32,
    pub(super) info_fast_arity: i32,
    pub(super) info_frame_need: i32,
    pub(super) info_table: i32,
}

impl Off {
    pub(super) fn new() -> Off {
        use std::mem::offset_of;
        crate::value::check_value_encoding();
        let l = crate::value::layout();
        let frames: Vec<Frame> = Vec::with_capacity(4);
        let (fp, fl) = crate::value::vec_offsets(&frames);
        let heap = offset_of!(Vm, heap) as i32;
        Off {
            l,
            fr_owner: offset_of!(Frame, owner) as i32,
            fr_code: offset_of!(Frame, code) as i32,
            fr_consts: offset_of!(Frame, consts) as i32,
            fr_ip: offset_of!(Frame, ip) as i32,
            fr_base: offset_of!(Frame, base) as i32,
            fr_ret: offset_of!(Frame, ret) as i32,
            fr_upvals: offset_of!(Frame, upvals) as i32,
            fr_globals: offset_of!(Frame, globals) as i32,
            frame_size: std::mem::size_of::<Frame>() as i64,
            vm_stack_ptr: offset_of!(Vm, stack_ptr) as i32,
            vm_frames_ptr: offset_of!(Vm, frames) as i32 + fp,
            vm_frames_len: offset_of!(Vm, frames) as i32 + fl,
            vm_since_gc: heap + l.heap_since_gc,
            vm_threshold: heap + l.heap_threshold,
            info_entry: offset_of!(JitInfo, entry) as i32,
            info_code: offset_of!(JitInfo, code) as i32,
            info_consts: offset_of!(JitInfo, consts) as i32,
            info_globals: offset_of!(JitInfo, globals) as i32,
            info_fast_arity: offset_of!(JitInfo, fast_arity) as i32,
            info_frame_need: offset_of!(JitInfo, frame_need) as i32,
            info_table: offset_of!(JitInfo, table) as i32,
        }
    }
}

pub(super) fn set_state(g: Gc, st: u8) {
    if let Obj::Func(f) = g.get_mut() {
        f.native_state = st;
    }
}

// ---------------------------------------------------------------------------- runtime
//
// Functions compiled code calls. They return 0 on success and 1 when an exception
// was raised (its message parked in `vm.jit_err`); `sp` arguments are absolute
// stack heights that make the caller's live values visible to the GC.

pub(super) const OK: u64 = 0;
pub(super) const ERR: u64 = 1;

#[inline]
pub(super) fn status(vm: &mut Vm, r: Result<(), String>) -> u64 {
    match r {
        Ok(()) => OK,
        Err(e) => {
            vm.jit_err = e;
            ERR
        }
    }
}

pub(super) fn binop_kind(k: u64) -> BinOp {
    match k {
        0 => BinOp::Add,
        1 => BinOp::Sub,
        2 => BinOp::Mul,
        3 => BinOp::Div,
        4 => BinOp::FloorDiv,
        5 => BinOp::Mod,
        6 => BinOp::Pow,
        7 => BinOp::BitAnd,
        8 => BinOp::BitOr,
        9 => BinOp::BitXor,
        10 => BinOp::Shl,
        _ => BinOp::Shr,
    }
}

pub(super) fn binop_code(k: BinOp) -> i64 {
    match k {
        BinOp::Add => 0,
        BinOp::Sub => 1,
        BinOp::Mul => 2,
        BinOp::Div => 3,
        BinOp::FloorDiv => 4,
        BinOp::Mod => 5,
        BinOp::Pow => 6,
        BinOp::BitAnd => 7,
        BinOp::BitOr => 8,
        BinOp::BitXor => 9,
        BinOp::Shl => 10,
        BinOp::Shr => 11,
    }
}

pub(super) fn cmp_kind(k: u64) -> CmpOp {
    match k {
        0 => CmpOp::Eq,
        1 => CmpOp::Ne,
        2 => CmpOp::Lt,
        3 => CmpOp::Le,
        4 => CmpOp::Gt,
        _ => CmpOp::Ge,
    }
}

pub(super) fn cmp_code(k: CmpOp) -> i64 {
    match k {
        CmpOp::Eq => 0,
        CmpOp::Ne => 1,
        CmpOp::Lt => 2,
        CmpOp::Le => 3,
        CmpOp::Gt => 4,
        CmpOp::Ge => 5,
    }
}

pub(super) extern "C" fn rt_binop(vm: &mut Vm, sp: u64, k: u64, a: *const Value, b: *const Value, out: *mut Value) -> u64 {
    vm.sp = sp as usize;
    let r = unsafe { vm.binop(binop_kind(k), *a, *b) }.map(|v| unsafe { *out = v });
    status(vm, r)
}

/// 0 / 1 = false / true, 2 = exception.
pub(super) extern "C" fn rt_compare(vm: &mut Vm, sp: u64, k: u64, a: *const Value, b: *const Value) -> u64 {
    vm.sp = sp as usize;
    match unsafe { vm.compare(cmp_kind(k), *a, *b) } {
        Ok(r) => r as u64,
        Err(e) => {
            vm.jit_err = e;
            2
        }
    }
}

/// Next item of the generator in loop state `*state` into `*out`:
/// 1 = got one, 0 = exhausted (or not a generator), 2 = exception.
pub(super) extern "C" fn rt_gen_next(vm: &mut Vm, sp: u64, state: *const Value, out: *mut Value) -> u64 {
    vm.sp = sp as usize;
    let g = match unsafe { *state } {
        Value::Obj(g) if matches!(g.get(), Obj::Generator(_)) => g,
        _ => return 0,
    };
    match vm.resume_gen(g, Value::None) {
        Ok(Some(v)) => {
            unsafe { *out = v };
            1
        }
        Ok(None) => 0,
        Err(e) => {
            vm.jit_err = e;
            2
        }
    }
}

/// 0 / 1 = false / true, 2 = exception.
pub(super) extern "C" fn rt_truthy(vm: &mut Vm, sp: u64, a: *const Value) -> u64 {
    vm.sp = sp as usize;
    let v = unsafe { *a };
    let r = match v {
        Value::Obj(g) if matches!(g.get(), Obj::Instance(_)) => vm.truthy(v),
        _ => Ok(v.truthy()),
    };
    match r {
        Ok(r) => r as u64,
        Err(e) => {
            vm.jit_err = e;
            2
        }
    }
}

pub(super) extern "C" fn rt_neg(vm: &mut Vm, sp: u64, a: *const Value, out: *mut Value) -> u64 {
    vm.sp = sp as usize;
    let v = unsafe { *a };
    let r = match dunder(v, sym::D_NEG) {
        Some(m) => vm.call(m, &[v]),
        None => ops::neg(v),
    };
    status(vm, r.map(|v| unsafe { *out = v }))
}

/// Runs an instruction the interpreter keeps out of line (`exec_cold`) on the
/// frame's stack, finishing any call it starts.
pub(super) extern "C" fn rt_cold(vm: &mut Vm, sp: u64, op: *const Op) -> u64 {
    vm.sp = sp as usize;
    let depth = vm.frames.len();
    let mut r = vm.exec_cold(unsafe { *op });
    if r.is_ok() && vm.frames.len() > depth {
        r = vm.run().map(|v| vm.spush(v));
    }
    status(vm, r)
}

/// `stack[callee](args)`: the result replaces the callee.
pub(super) extern "C" fn rt_call(vm: &mut Vm, callee: u64, argc: u64) -> u64 {
    let (callee, argc) = (callee as usize, argc as usize);
    vm.sp = callee + argc + 1;
    if vm.heap.should_collect() {
        vm.collect_garbage();
    }
    let r = match vm.call_value(callee, argc) {
        Ok(true) => vm.run().map(|v| vm.stack[callee] = v),
        Ok(false) => Ok(()),
        Err(e) => Err(e),
    };
    status(vm, r)
}

/// `stack[recv].name(args)`: the result replaces the receiver. With an inline-cache
/// cell, records the (class shape, method index) seen so compiled code can call
/// the method directly next time.
pub(super) extern "C" fn rt_invoke(vm: &mut Vm, recv: u64, name: u64, argc: u64, cell: *mut u32) -> u64 {
    let (recv, name, argc) = (recv as usize, name as u32, argc as usize);
    vm.sp = recv + argc + 1;
    if vm.heap.should_collect() {
        vm.collect_garbage();
    }
    if !cell.is_null() {
        if let Value::Obj(g) = vm.stack[recv] {
            if let Obj::Instance(inst) = g.get() {
                let cls = inst.class.as_class();
                if !cls.slots.contains_key(&name) {
                    if let Some(&midx) = cls.methods.get(&name) {
                        unsafe {
                            *cell = cls.id;
                            *cell.add(1) = midx;
                        }
                    }
                }
            }
        }
    }
    let r = match vm.invoke(recv, name, argc) {
        Ok(true) => vm.run().map(|v| vm.stack[recv] = v),
        Ok(false) => Ok(()),
        Err(e) => Err(e),
    };
    status(vm, r)
}

/// `*out = obj.name`, filling the inline-cache cell (class shape, field slot).
pub(super) extern "C" fn rt_getattr(vm: &mut Vm, sp: u64, obj: *const Value, name: u64, out: *mut Value, cell: *mut u32) -> u64 {
    vm.sp = sp as usize;
    let r = vm.load_attr(unsafe { *obj }, name as u32).map(|(v, cache)| {
        if let Some((cid, slot)) = cache {
            unsafe {
                *cell = cid;
                *cell.add(1) = slot;
            }
        }
        unsafe { *out = v };
    });
    status(vm, r)
}

/// `obj.name = *v`, filling the inline-cache cell.
pub(super) extern "C" fn rt_setattr(vm: &mut Vm, sp: u64, obj: *const Value, name: u64, v: *const Value, cell: *mut u32) -> u64 {
    vm.sp = sp as usize;
    let r = vm.store_attr(unsafe { *obj }, name as u32, unsafe { *v }).map(|cache| {
        if let Some((cid, slot)) = cache {
            unsafe {
                *cell = cid;
                *cell.add(1) = slot;
            }
        }
    });
    status(vm, r)
}

pub(super) extern "C" fn rt_getindex(vm: &mut Vm, sp: u64, obj: *const Value, idx: *const Value, out: *mut Value) -> u64 {
    vm.sp = sp as usize;
    let r = unsafe { vm.get_item(*obj, *idx) }.map(|v| unsafe { *out = v });
    status(vm, r)
}

pub(super) extern "C" fn rt_name_error(vm: &mut Vm, g: u64) -> u64 {
    vm.jit_err = vm.name_error(g as u32);
    ERR
}

pub(super) extern "C" fn rt_setup_try(vm: &mut Vm, sp: u64, target: u64) -> u64 {
    vm.handlers.push(Handler {
        frames: vm.frames.len(),
        sp: sp as usize,
        target: target as u32,
    });
    OK
}

pub(super) extern "C" fn rt_pop_try(vm: &mut Vm) -> u64 {
    vm.handlers.pop();
    OK
}

/// An exception reached compiled code: unwinds to this frame's innermost handler
/// and returns its code index, or `u64::MAX` when the frame has none.
pub(super) extern "C" fn rt_catch(vm: &mut Vm, frame: *mut Frame) -> u64 {
    let idx = (frame as usize - vm.frames.as_ptr() as usize) / std::mem::size_of::<Frame>();
    let err = std::mem::take(&mut vm.jit_err);
    match vm.handle_exception(err, idx) {
        Ok(target) => target as u64,
        Err(e) => {
            vm.jit_err = e;
            u64::MAX
        }
    }
}

pub(super) extern "C" fn rt_gc(vm: &mut Vm, sp: u64) -> u64 {
    vm.sp = sp as usize;
    if vm.heap.should_collect() {
        vm.collect_garbage();
    }
    OK
}

pub(super) extern "C" fn rt_close(vm: &mut Vm, from: u64) -> u64 {
    vm.close_upvalues(from as usize);
    OK
}

/// Leaves compiled code: the interpreter finishes the call from instruction `ip`.
pub(super) extern "C" fn rt_deopt(vm: &mut Vm, frame: *mut Frame, ip: u64, sp: u64) -> u64 {
    let ret = unsafe {
        (*frame).ip = (*frame).code.add(ip as usize);
        (*frame).ret
    };
    vm.sp = sp as usize;
    let r = vm.interp().map(|v| vm.stack[ret] = v);
    status(vm, r)
}

impl Vm {
    /// Runs the top frame's native code (see `run`).
    pub(super) fn run_native(&mut self, info: *const JitInfo) -> Result<Value, String> {
        let fr = self.frames.last_mut().unwrap() as *mut Frame;
        let ret = unsafe { (*fr).ret };
        let st = unsafe { ((*info).entry)(self, fr, info) };
        if st == OK {
            self.sp = ret;
            Ok(self.stack[ret])
        } else {
            let e = std::mem::take(&mut self.jit_err);
            Err(self.record_exception(e))
        }
    }

    /// Makes sure an error leaving compiled code has its exception object and its
    /// traceback (taken while the failing frames are still on the frame stack).
    fn record_exception(&mut self, err: String) -> String {
        let exc = match self.exc.take() {
            Some((v, m)) if m == err => v,
            _ => self.make_exception(&err),
        };
        if let Value::Obj(g) = exc {
            if self.trace_exc != Some(g) {
                self.capture_trace();
                self.trace_exc = Some(g);
            }
        }
        self.exc = Some((exc, err.clone()));
        err
    }

    /// (compiled, interpreted) function counts.
    pub fn native_stats(&self) -> Option<(usize, usize)> {
        self.jit.as_ref().map(|j| (j.compiled, j.interpreted))
    }
}

/// The base form of an inline-cached instruction (compiled code has its own caches).
pub(super) fn canon(op: Op) -> Op {
    match op {
        Op::GetAttrIC(n, _, _) => Op::GetAttr(n),
        Op::SetAttrIC(n, _, _) => Op::SetAttr(n),
        Op::GetLocalAttrIC(n, _, packed) => Op::GetLocalAttr(packed & 0xffff, n),
        Op::SetLocalAttrIC(n, _, packed) => Op::SetLocalAttr(packed & 0xffff, n),
        Op::InvokeIC(name, _, packed) => Op::Invoke(name, packed & 0xff),
        Op::InvokeAppend(name) => Op::Invoke(name, 1),
        other => other,
    }
}

// ---------------------------------------------------------------------------- inline caches & calls

/// A fresh instance of class `cls` into `*out` (the constructor's frame slot).
pub(super) extern "C" fn rt_new_instance(vm: &mut Vm, cls: u64, out: *mut Value) -> u64 {
    let g: Gc = unsafe { std::mem::transmute::<u64, Gc>(cls) };
    let n = g.as_class().slot_count as usize;
    let inst = vm.heap.alloc(Obj::Instance(Instance::new(g, vec![Value::Undef; n])));
    unsafe { *out = Value::Obj(inst) };
    OK
}

/// `list.append(v)` from the `Invoke` fast path.
pub(super) extern "C" fn rt_list_push(_vm: &mut Vm, list: u64, v: *const Value) -> u64 {
    let g: Gc = unsafe { std::mem::transmute::<u64, Gc>(list) };
    if let Obj::List(l) = g.get_mut() {
        l.push(unsafe { *v });
    }
    OK
}


// ---------------------------------------------------------------------------- ahead-of-time code
//
// `faston build` compiles every function of the program and saves the machine
// code in the executable; the runtime maps it into executable memory and attaches
// it to the functions as it compiles the same sources to bytecode. Saved code
// can't contain addresses from the build process, so it reads every address it
// needs (runtime helpers, inline-cache cells, instructions, constant objects)
// from a per-function table filled in at load time.

/// What one entry of a function's address table holds.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum Slot {
    /// Runtime helper `rt_fns()[n]`.
    Rt(u32),
    /// Inline-cache cell of instruction `n`.
    Cell(u32),
    /// The function's bytecode at instruction `n`.
    Ip(u32),
    /// The pristine copy of instruction `n`.
    Op(u32),
    /// Constant `n` (an object).
    Const(u32),
}

/// The runtime helpers compiled code calls, by table index.
pub(super) fn rt_fns() -> [usize; 20] {
    [
        rt_binop as *const () as usize,
        rt_call as *const () as usize,
        rt_catch as *const () as usize,
        rt_close as *const () as usize,
        rt_cold as *const () as usize,
        rt_compare as *const () as usize,
        rt_deopt as *const () as usize,
        rt_gc as *const () as usize,
        rt_gen_next as *const () as usize,
        rt_getattr as *const () as usize,
        rt_getindex as *const () as usize,
        rt_invoke as *const () as usize,
        rt_list_push as *const () as usize,
        rt_name_error as *const () as usize,
        rt_neg as *const () as usize,
        rt_new_instance as *const () as usize,
        rt_pop_try as *const () as usize,
        rt_setattr as *const () as usize,
        rt_setup_try as *const () as usize,
        rt_truthy as *const () as usize,
    ]
}

/// One function's saved machine code.
pub struct AotFunc {
    /// Hash of the bytecode it was compiled from.
    pub(super) hash: u64,
    pub(super) code: Vec<u8>,
    pub(super) slots: Vec<Slot>,
}

/// Saved code of one module: its functions in `unit_funcs` order (`None`: left
/// to the interpreter).
pub struct AotUnit {
    pub(super) src: u64,
    pub(super) funcs: Vec<Option<AotFunc>>,
}

/// Loaded ahead-of-time code: per module (keyed by a hash of its source), each
/// function's entry point and address table.
pub struct Aot {
    units: HashMap<u64, Vec<Option<(usize, AotFunc)>>>,
}

const AOT_MAGIC: &[u8; 8] = b"FSTNAOT1";

/// 64-bit FNV-1a.
pub(super) struct Fnv(pub(super) u64);

impl Fnv {
    pub(super) fn new() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
}

impl std::hash::Hasher for Fnv {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ b as u64).wrapping_mul(0x0100_0000_01b3);
        }
    }
}

pub(super) fn hash_str(s: &str) -> u64 {
    use std::hash::Hasher;
    let mut h = Fnv::new();
    h.write(s.as_bytes());
    h.finish()
}

/// Hash of a function's (base-form) bytecode.
pub(super) fn hash_code(code: &[Op]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = Fnv::new();
    for &op in code {
        canon(op).hash(&mut h);
    }
    h.finish()
}

/// Everything saved code depends on besides its bytecode: if any of it differs
/// between the compiler and the runtime, the code is not used.
pub(super) fn fingerprint() -> u64 {
    let s = format!(
        "{} {} {:?} {:?} {} {} {} {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::ARCH,
        Off::new(),
        crate::value::layout(),
        std::mem::size_of::<Op>(),
        std::mem::size_of::<Frame>(),
        STACK_SLOTS,
        MAX_FRAMES,
    );
    hash_str(&s)
}

/// The functions of a module in the order their saved code is listed: the
/// top-level function and, depth first, every function nested in it.
pub(super) fn unit_funcs(script: Gc) -> Vec<Gc> {
    let mut todo = vec![script];
    let mut funcs = Vec::new();
    while let Some(g) = todo.pop() {
        for c in g.as_func().consts.iter() {
            if let Value::Obj(cg) = c {
                if let Obj::Func(_) = cg.get() {
                    todo.push(*cg);
                }
            }
        }
        funcs.push(g);
    }
    funcs
}

/// Serializes saved code (the `\0aot` entry of a built program).
/// `syms` are the symbol names in number order, `env` identifies the built-ins.
pub fn aot_encode(units: &[AotUnit], syms: &[String], env: u64) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(AOT_MAGIC);
    out.extend_from_slice(&fingerprint().to_le_bytes());
    out.extend_from_slice(&env.to_le_bytes());
    out.extend_from_slice(&(syms.len() as u32).to_le_bytes());
    for s in syms {
        out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        out.extend_from_slice(s.as_bytes());
    }
    out.extend_from_slice(&(units.len() as u32).to_le_bytes());
    for u in units {
        out.extend_from_slice(&u.src.to_le_bytes());
        out.extend_from_slice(&(u.funcs.len() as u32).to_le_bytes());
        for f in u.funcs.iter() {
            let Some(f) = f else {
                out.push(0);
                continue;
            };
            out.push(1);
            out.extend_from_slice(&f.hash.to_le_bytes());
            out.extend_from_slice(&(f.code.len() as u32).to_le_bytes());
            out.extend_from_slice(&f.code);
            out.extend_from_slice(&(f.slots.len() as u32).to_le_bytes());
            for s in f.slots.iter() {
                let (k, n) = match *s {
                    Slot::Rt(n) => (0u8, n),
                    Slot::Cell(n) => (1, n),
                    Slot::Ip(n) => (2, n),
                    Slot::Op(n) => (3, n),
                    Slot::Const(n) => (4, n),
                };
                out.push(k);
                out.extend_from_slice(&n.to_le_bytes());
            }
        }
    }
    out
}

struct Rd<'a>(&'a [u8]);

impl Rd<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        if self.0.len() < n {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
}

impl Aot {
    /// Loads saved code. `None` if it is malformed, was made by a different
    /// Faston, or can't be mapped executable here. Also gives the symbol names
    /// and the built-ins hash it was compiled with.
    pub fn load(data: &[u8]) -> Option<(Aot, Vec<String>, u64)> {
        let mut r = Rd(data);
        if r.take(8)? != AOT_MAGIC || r.u64()? != fingerprint() {
            return None;
        }
        let env = r.u64()?;
        let ns = r.u32()? as usize;
        let mut syms = Vec::with_capacity(ns.min(1 << 20));
        for _ in 0..ns {
            let n = r.u32()? as usize;
            syms.push(std::str::from_utf8(r.take(n)?).ok()?.to_string());
        }
        let nu = r.u32()? as usize;
        let mut units = Vec::with_capacity(nu.min(1 << 16));
        let mut total = 0usize;
        for _ in 0..nu {
            let src = r.u64()?;
            let nf = r.u32()? as usize;
            let mut funcs = Vec::with_capacity(nf.min(1 << 16));
            for _ in 0..nf {
                if r.u8()? == 0 {
                    funcs.push(None);
                    continue;
                }
                let hash = r.u64()?;
                let len = r.u32()? as usize;
                let code = r.take(len)?.to_vec();
                let ns = r.u32()? as usize;
                let mut slots = Vec::with_capacity(ns.min(1 << 16));
                for _ in 0..ns {
                    let k = r.u8()?;
                    let n = r.u32()?;
                    slots.push(match k {
                        0 if (n as usize) < rt_fns().len() => Slot::Rt(n),
                        1 => Slot::Cell(n),
                        2 => Slot::Ip(n),
                        3 => Slot::Op(n),
                        4 => Slot::Const(n),
                        _ => return None,
                    });
                }
                total = (total + 15) & !15;
                let at = total;
                total += code.len();
                funcs.push(Some((at, AotFunc { hash, code, slots })));
            }
            units.push((src, funcs));
        }
        let size = total.max(1);
        let mem = exec::alloc(size)?;
        for (_, funcs) in units.iter() {
            for (at, f) in funcs.iter().flatten() {
                unsafe { std::ptr::copy_nonoverlapping(f.code.as_ptr(), mem.add(*at), f.code.len()) };
            }
        }
        if !exec::protect(mem, size) {
            return None;
        }
        let mut map = HashMap::new();
        for (src, funcs) in units {
            let funcs = funcs
                .into_iter()
                .map(|f| {
                    f.map(|(at, mut f)| {
                        f.code = Vec::new(); // it lives in executable memory now
                        (mem as usize + at, f)
                    })
                })
                .collect();
            map.insert(src, funcs);
        }
        Some((Aot { units: map }, syms, env))
    }
}

impl Vm {
    /// Attaches saved machine code to the functions of a module just compiled
    /// from `src` (whose top-level function is `script`).
    pub(super) fn install_aot(&mut self, src: &str, script: Gc) {
        let Some(aot) = self.aot.take() else { return };
        if let Some(saved) = aot.units.get(&hash_str(src)) {
            let funcs = unit_funcs(script);
            let before = self.aot_count;
            if funcs.len() == saved.len() {
                for (&g, s) in funcs.iter().zip(saved.iter()) {
                    if let Some((entry, af)) = s {
                        self.attach_aot(g, *entry, af);
                    }
                }
            }
            if std::env::var_os("FASTON_JIT_DEBUG").is_some() {
                let have = saved.iter().filter(|s| s.is_some()).count();
                eprintln!("[aot] {}: {} of {} saved functions attached", script.as_func().name, self.aot_count - before, have);
            }
        }
        self.aot = Some(aot);
    }

    fn attach_aot(&mut self, g: Gc, entry: usize, af: &AotFunc) {
        if g.as_func().native_state != 0 || hash_code(&g.as_func().code) != af.hash {
            return;
        }
        let globals = self.unit_globals(g.as_func().unit);
        let f = match g.get_mut() {
            Obj::Func(f) => f,
            _ => unreachable!(),
        };
        let n = f.code.len() as u32;
        let valid = af.slots.iter().all(|s| match *s {
            Slot::Rt(_) => true,
            Slot::Cell(i) | Slot::Op(i) => i < n,
            Slot::Ip(i) => i <= n,
            Slot::Const(k) => matches!(f.consts.get(k as usize), Some(Value::Obj(_))),
        });
        if !valid {
            return;
        }
        let ops: Box<[Op]> = f.code.iter().map(|&o| canon(o)).collect();
        let cells: Box<[u32]> = vec![0u32; 2 * f.code.len()].into_boxed_slice();
        let rt = rt_fns();
        let table: Box<[u64]> = af
            .slots
            .iter()
            .map(|s| match *s {
                Slot::Rt(k) => rt[k as usize] as u64,
                Slot::Cell(i) => (unsafe { cells.as_ptr().add(2 * i as usize) }) as u64,
                Slot::Ip(i) => (unsafe { f.code.as_ptr().add(i as usize) }) as u64,
                Slot::Op(i) => (unsafe { ops.as_ptr().add(i as usize) }) as u64,
                Slot::Const(k) => match f.consts[k as usize] {
                    Value::Obj(c) => c.addr() as u64,
                    _ => unreachable!(),
                },
            })
            .collect();
        let info = Box::new(JitInfo {
            entry: unsafe { std::mem::transmute::<usize, Entry>(entry) },
            code: f.code.as_ptr(),
            consts: f.consts.as_ptr(),
            globals,
            fast_arity: f.fast_arity as u64,
            num_locals: f.num_locals as u64,
            frame_need: f.num_locals as u64 + f.max_stack as u64,
            table: table.as_ptr(),
            table_store: table,
            ops,
            cells,
        });
        f.jit = Box::leak(info);
        f.native_state = 1;
        self.aot_count += 1;
    }
}

/// Executable memory for loaded code (never freed: it lives as long as the program).
mod exec {
    #[cfg(windows)]
    mod sys {
        use std::ffi::c_void;
        pub const MEM_COMMIT: u32 = 0x1000;
        pub const MEM_RESERVE: u32 = 0x2000;
        pub const PAGE_READWRITE: u32 = 0x04;
        pub const PAGE_EXECUTE_READ: u32 = 0x20;
        #[link(name = "kernel32")]
        extern "system" {
            pub fn VirtualAlloc(addr: *mut c_void, size: usize, ty: u32, protect: u32) -> *mut c_void;
            pub fn VirtualProtect(addr: *mut c_void, size: usize, protect: u32, old: *mut u32) -> i32;
            pub fn GetCurrentProcess() -> *mut c_void;
            pub fn FlushInstructionCache(process: *mut c_void, addr: *const c_void, size: usize) -> i32;
        }
    }

    #[cfg(windows)]
    pub fn alloc(size: usize) -> Option<*mut u8> {
        let p = unsafe { sys::VirtualAlloc(std::ptr::null_mut(), size, sys::MEM_COMMIT | sys::MEM_RESERVE, sys::PAGE_READWRITE) };
        (!p.is_null()).then_some(p as *mut u8)
    }

    #[cfg(windows)]
    pub fn protect(p: *mut u8, size: usize) -> bool {
        let mut old = 0u32;
        unsafe {
            sys::VirtualProtect(p as _, size, sys::PAGE_EXECUTE_READ, &mut old) != 0
                && sys::FlushInstructionCache(sys::GetCurrentProcess(), p as _, size) != 0
        }
    }

    #[cfg(unix)]
    mod sys {
        use std::ffi::c_void;
        pub const PROT_READ: i32 = 1;
        pub const PROT_WRITE: i32 = 2;
        pub const PROT_EXEC: i32 = 4;
        pub const MAP_PRIVATE: i32 = 2;
        #[cfg(target_os = "macos")]
        pub const MAP_ANON: i32 = 0x1000;
        #[cfg(not(target_os = "macos"))]
        pub const MAP_ANON: i32 = 0x20;
        extern "C" {
            pub fn mmap(addr: *mut c_void, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut c_void;
            pub fn mprotect(addr: *mut c_void, len: usize, prot: i32) -> i32;
        }
    }

    #[cfg(unix)]
    pub fn alloc(size: usize) -> Option<*mut u8> {
        let p = unsafe {
            sys::mmap(std::ptr::null_mut(), size, sys::PROT_READ | sys::PROT_WRITE, sys::MAP_PRIVATE | sys::MAP_ANON, -1, 0)
        };
        (p as isize != -1 && !p.is_null()).then_some(p as *mut u8)
    }

    #[cfg(unix)]
    pub fn protect(p: *mut u8, size: usize) -> bool {
        // saved code is only produced for x86-64, whose instruction cache is coherent
        unsafe { sys::mprotect(p as _, size, sys::PROT_READ | sys::PROT_EXEC) == 0 }
    }

    #[cfg(not(any(windows, unix)))]
    pub fn alloc(_size: usize) -> Option<*mut u8> {
        None
    }

    #[cfg(not(any(windows, unix)))]
    pub fn protect(_p: *mut u8, _size: usize) -> bool {
        false
    }
}
