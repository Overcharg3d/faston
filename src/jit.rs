//! Native code generation: every bytecode function is compiled to machine code
//! with Cranelift before the program runs (`faston build`, `faston run --native`).
//!
//! Compiled code uses the same frames, value stack, globals and heap as the
//! interpreter, so the two can call each other freely and share all the runtime
//! code (built-ins, operators, exceptions, the GC).
//!
//! How a function is compiled:
//!
//! * **Type inference.** A data-flow pass works out, for every instruction, what
//!   is known about each local and stack slot: int, float, bool, None, or "any".
//!   Locals that only ever hold one scalar type (loop counters, accumulators,
//!   flags) are *promoted*: they live in machine registers as raw `i64` / `f64` /
//!   `bool`, and arithmetic on them is plain machine arithmetic (with an overflow
//!   check, since Faston ints are 64-bit and overflow is an error).
//! * **Everything else stays dynamic.** Unknown values are tagged 16-byte values in
//!   their stack slots; operations on them get inline fast paths for ints/floats
//!   and call into the runtime (`rt_*` below) for strings, lists, instances,
//!   operator methods and errors. Attribute access and method calls use inline
//!   caches.
//! * **Speculation with a fallback.** `for i in range(...)` is compiled assuming
//!   `range` is the built-in (so `i` is an int). If that ever turns out wrong at
//!   runtime, the function *deoptimizes*: its state is written back to the frame
//!   and the interpreter finishes that call.

use std::collections::HashMap;

use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::types::{F64, I32, I64, I8};
use cranelift_codegen::ir::{AbiParam, Block, InstBuilder, MemFlagsData, SigRef, Signature, TrapCode};
use cranelift_codegen::ir::{Value as V};
use cranelift_codegen::isa::CallConv;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Switch, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::Module;

use super::*;
use crate::value::{kind, tag, Layout};

type Entry = unsafe extern "C" fn(*mut Vm, *mut Frame) -> u64;

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
    /// Pristine copy of the bytecode (the interpreter rewrites the real one with
    /// inline-cache forms); helpers are handed instructions from here.
    ops: Box<[Op]>,
    /// Inline-cache cells, two `u32`s per site.
    cells: Box<[u32]>,
}

/// Byte offsets compiled code needs.
#[derive(Clone, Copy)]
struct Off {
    l: Layout,
    fr_owner: i32,
    fr_code: i32,
    fr_consts: i32,
    fr_ip: i32,
    fr_base: i32,
    fr_ret: i32,
    fr_upvals: i32,
    fr_globals: i32,
    frame_size: i64,
    vm_stack_ptr: i32,
    vm_frames_ptr: i32,
    vm_frames_len: i32,
    vm_since_gc: i32,
    vm_threshold: i32,
    info_entry: i32,
    info_code: i32,
    info_consts: i32,
    info_globals: i32,
    info_fast_arity: i32,
    info_frame_need: i32,
}

impl Off {
    fn new() -> Off {
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
        }
    }
}

pub struct Jit {
    module: JITModule,
    ctx: cranelift_codegen::Context,
    fctx: FunctionBuilderContext,
    off: Off,
    call_conv: CallConv,
    frontend: cranelift_codegen::isa::TargetFrontendConfig,
    debug: bool,
    /// Functions compiled / left to the interpreter.
    pub compiled: usize,
    pub interpreted: usize,
    /// Compile whole units up front (`faston build`) instead of at first call.
    pub eager: bool,
}

impl Jit {
    pub fn new() -> Jit {
        let mut flags = settings::builder();
        flags.set("opt_level", "speed").unwrap();
        flags.set("use_colocated_libcalls", "false").unwrap();
        flags.set("is_pic", "false").unwrap();
        let verify = if cfg!(debug_assertions) { "true" } else { "false" };
        flags.set("enable_verifier", verify).unwrap();
        let isa = cranelift_native::builder()
            .unwrap_or_else(|e| panic!("faston: native code generation is not supported on this machine: {}", e))
            .finish(settings::Flags::new(flags))
            .expect("faston: failed to set up the code generator");
        let call_conv = isa.default_call_conv();
        let frontend = isa.frontend_config();
        let module = JITModule::new(JITBuilder::with_isa(isa, cranelift_module::default_libcall_names()));
        let ctx = module.make_context();
        Jit {
            module,
            ctx,
            fctx: FunctionBuilderContext::new(),
            off: Off::new(),
            call_conv,
            frontend,
            debug: std::env::var_os("FASTON_JIT_DEBUG").is_some(),
            compiled: 0,
            interpreted: 0,
            eager: false,
        }
    }

    /// Compiles `funcs` (those not compiled yet) and installs their native code.
    pub fn compile(&mut self, vm: &mut Vm, funcs: &[Gc]) {
        let range_slot = vm.builtin_names.index.get("range").copied();
        let mut done = Vec::new();
        for &g in funcs {
            let f = g.as_func();
            if f.native_state != 0 {
                continue;
            }
            let an = match analyze(f, range_slot) {
                Some(a) => a,
                None => {
                    if self.debug {
                        eprintln!("[jit] {}: left to the interpreter", f.name);
                    }
                    self.interpreted += 1;
                    set_state(g, 2);
                    continue;
                }
            };
            if self.debug {
                let regs: Vec<String> = an
                    .promoted
                    .iter()
                    .enumerate()
                    .filter_map(|(l, t)| t.map(|t| format!("{}:{:?}", l, t)))
                    .collect();
                eprintln!("[jit] {}: locals in registers: [{}]", f.name, regs.join(", "));
            }
            let ops: Box<[Op]> = an.ops.clone().into_boxed_slice();
            let cells: Box<[u32]> = vec![0u32; 2 * f.code.len()].into_boxed_slice();
            let mut sig = Signature::new(self.call_conv);
            sig.params.push(AbiParam::new(I64));
            sig.params.push(AbiParam::new(I64));
            sig.returns.push(AbiParam::new(I64));
            self.ctx.func.signature = sig;
            {
                let b = FunctionBuilder::new(&mut self.ctx.func, &mut self.fctx);
                let cg = Cg::new(b, f, &an, self.off, self.call_conv, ops.as_ptr(), cells.as_ptr());
                cg.translate(self.frontend);
            }
            if self.debug {
                eprintln!("[jit] {}:\n{}", f.name, self.ctx.func.display());
            }
            self.ctx.set_disasm(self.debug);
            let id = self.module.declare_anonymous_function(&self.ctx.func.signature).unwrap();
            if let Err(e) = self.module.define_function(id, &mut self.ctx) {
                if self.debug {
                    eprintln!("[jit] {}: code generation failed: {:?}", f.name, e);
                }
                self.module.clear_context(&mut self.ctx);
                self.interpreted += 1;
                set_state(g, 2);
                continue;
            }
            if self.debug {
                let size = self.ctx.compiled_code().map_or(0, |c| c.code_info().total_size);
                eprintln!("[jit] {}: {} bytes of machine code; optimized:\n{}", f.name, size, self.ctx.func.display());
                if let Some(d) = self.ctx.compiled_code().and_then(|c| c.vcode.as_ref()) {
                    eprintln!("[jit] {} machine code:\n{}", f.name, d);
                }
            }
            self.module.clear_context(&mut self.ctx);
            done.push((g, id, ops, cells));
        }
        self.module.finalize_definitions().expect("faston: failed to finalize native code");
        for (g, id, ops, cells) in done {
            let ptr = self.module.get_finalized_function(id);
            let unit = g.as_func().unit;
            let globals = vm.unit_globals(unit);
            let f = match g.get_mut() {
                Obj::Func(f) => f,
                _ => unreachable!(),
            };
            let info = Box::new(JitInfo {
                entry: unsafe { std::mem::transmute::<*const u8, Entry>(ptr) },
                code: f.code.as_ptr(),
                consts: f.consts.as_ptr(),
                globals,
                fast_arity: f.fast_arity as u64,
                num_locals: f.num_locals as u64,
                frame_need: f.num_locals as u64 + f.max_stack as u64,
                ops,
                cells,
            });
            f.jit = Box::leak(info);
            f.native_state = 1;
            self.compiled += 1;
        }
    }
}

fn set_state(g: Gc, st: u8) {
    if let Obj::Func(f) = g.get_mut() {
        f.native_state = st;
    }
}

// ---------------------------------------------------------------------------- runtime
//
// Functions compiled code calls. They return 0 on success and 1 when an exception
// was raised (its message parked in `vm.jit_err`); `sp` arguments are absolute
// stack heights that make the caller's live values visible to the GC.

const OK: u64 = 0;
const ERR: u64 = 1;

#[inline]
fn status(vm: &mut Vm, r: Result<(), String>) -> u64 {
    match r {
        Ok(()) => OK,
        Err(e) => {
            vm.jit_err = e;
            ERR
        }
    }
}

fn binop_kind(k: u64) -> BinOp {
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

fn binop_code(k: BinOp) -> i64 {
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

fn cmp_kind(k: u64) -> CmpOp {
    match k {
        0 => CmpOp::Eq,
        1 => CmpOp::Ne,
        2 => CmpOp::Lt,
        3 => CmpOp::Le,
        4 => CmpOp::Gt,
        _ => CmpOp::Ge,
    }
}

fn cmp_code(k: CmpOp) -> i64 {
    match k {
        CmpOp::Eq => 0,
        CmpOp::Ne => 1,
        CmpOp::Lt => 2,
        CmpOp::Le => 3,
        CmpOp::Gt => 4,
        CmpOp::Ge => 5,
    }
}

extern "C" fn rt_binop(vm: &mut Vm, sp: u64, k: u64, a: *const Value, b: *const Value, out: *mut Value) -> u64 {
    vm.sp = sp as usize;
    let r = unsafe { vm.binop(binop_kind(k), *a, *b) }.map(|v| unsafe { *out = v });
    status(vm, r)
}

/// 0 / 1 = false / true, 2 = exception.
extern "C" fn rt_compare(vm: &mut Vm, sp: u64, k: u64, a: *const Value, b: *const Value) -> u64 {
    vm.sp = sp as usize;
    match unsafe { vm.compare(cmp_kind(k), *a, *b) } {
        Ok(r) => r as u64,
        Err(e) => {
            vm.jit_err = e;
            2
        }
    }
}

/// 0 / 1 = false / true, 2 = exception.
extern "C" fn rt_truthy(vm: &mut Vm, sp: u64, a: *const Value) -> u64 {
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

extern "C" fn rt_neg(vm: &mut Vm, sp: u64, a: *const Value, out: *mut Value) -> u64 {
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
extern "C" fn rt_cold(vm: &mut Vm, sp: u64, op: *const Op) -> u64 {
    vm.sp = sp as usize;
    let depth = vm.frames.len();
    let mut r = vm.exec_cold(unsafe { *op });
    if r.is_ok() && vm.frames.len() > depth {
        r = vm.run().map(|v| vm.spush(v));
    }
    status(vm, r)
}

/// `stack[callee](args)`: the result replaces the callee.
extern "C" fn rt_call(vm: &mut Vm, callee: u64, argc: u64) -> u64 {
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
extern "C" fn rt_invoke(vm: &mut Vm, recv: u64, name: u64, argc: u64, cell: *mut u32) -> u64 {
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
extern "C" fn rt_getattr(vm: &mut Vm, sp: u64, obj: *const Value, name: u64, out: *mut Value, cell: *mut u32) -> u64 {
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
extern "C" fn rt_setattr(vm: &mut Vm, sp: u64, obj: *const Value, name: u64, v: *const Value, cell: *mut u32) -> u64 {
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

extern "C" fn rt_getindex(vm: &mut Vm, sp: u64, obj: *const Value, idx: *const Value, out: *mut Value) -> u64 {
    vm.sp = sp as usize;
    let r = unsafe { vm.get_item(*obj, *idx) }.map(|v| unsafe { *out = v });
    status(vm, r)
}

extern "C" fn rt_name_error(vm: &mut Vm, g: u64) -> u64 {
    vm.jit_err = vm.name_error(g as u32);
    ERR
}

extern "C" fn rt_setup_try(vm: &mut Vm, sp: u64, target: u64) -> u64 {
    vm.handlers.push(Handler {
        frames: vm.frames.len(),
        sp: sp as usize,
        target: target as u32,
    });
    OK
}

extern "C" fn rt_pop_try(vm: &mut Vm) -> u64 {
    vm.handlers.pop();
    OK
}

/// An exception reached compiled code: unwinds to this frame's innermost handler
/// and returns its code index, or `u64::MAX` when the frame has none.
extern "C" fn rt_catch(vm: &mut Vm, frame: *mut Frame) -> u64 {
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

extern "C" fn rt_gc(vm: &mut Vm, sp: u64) -> u64 {
    vm.sp = sp as usize;
    if vm.heap.should_collect() {
        vm.collect_garbage();
    }
    OK
}

extern "C" fn rt_close(vm: &mut Vm, from: u64) -> u64 {
    vm.close_upvalues(from as usize);
    OK
}

/// Leaves compiled code: the interpreter finishes the call from instruction `ip`.
extern "C" fn rt_deopt(vm: &mut Vm, frame: *mut Frame, ip: u64, sp: u64) -> u64 {
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
        let st = unsafe { ((*info).entry)(self, fr) };
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

// ---------------------------------------------------------------------------- analysis

/// What is statically known about a value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ty {
    /// Not reached (yet).
    Bot,
    Int,
    Float,
    Bool,
    NoneT,
    /// The global `range` (assumed to be the built-in; checked when used).
    RangeFn,
    /// The result of calling `RangeFn`.
    RangeObj,
    Any,
}

impl Ty {
    fn join(self, o: Ty) -> Ty {
        if self == o || o == Ty::Bot {
            self
        } else if self == Ty::Bot {
            o
        } else {
            Ty::Any
        }
    }

    fn num(self) -> bool {
        matches!(self, Ty::Int | Ty::Float)
    }
}

/// Abstract state before an instruction.
#[derive(Clone, PartialEq, Debug)]
struct St {
    locals: Vec<Ty>,
    stack: Vec<Ty>,
    /// Active `try` handlers of this frame (their code indices), innermost last.
    hs: Vec<u32>,
}

enum Use {
    Read(u32, Ty),
    Write(u32, Ty),
    /// The runtime touches this local's stack slot directly: keep it in memory.
    Pin(u32),
}

/// Where an operand of an arithmetic / comparison instruction comes from.
#[derive(Clone, Copy, PartialEq)]
enum Src {
    Pop,
    Local(u32),
    Const(u32),
}

#[derive(Clone, Copy)]
enum Dst {
    Push,
    Local(u32),
}

#[derive(Clone, Copy)]
enum CmpUse {
    Push,
    JumpIfNot(u32),
    JumpIf(u32),
}

/// Arithmetic instructions as (operator, left, right, destination).
fn arith_parts(op: Op) -> Option<(BinOp, Src, Src, Dst)> {
    use Src::*;
    let p = Dst::Push;
    Some(match op {
        Op::Add => (BinOp::Add, Pop, Pop, p),
        Op::Sub => (BinOp::Sub, Pop, Pop, p),
        Op::Mul => (BinOp::Mul, Pop, Pop, p),
        Op::Div => (BinOp::Div, Pop, Pop, p),
        Op::FloorDiv => (BinOp::FloorDiv, Pop, Pop, p),
        Op::Mod => (BinOp::Mod, Pop, Pop, p),
        Op::Pow => (BinOp::Pow, Pop, Pop, p),
        Op::BitAnd => (BinOp::BitAnd, Pop, Pop, p),
        Op::BitOr => (BinOp::BitOr, Pop, Pop, p),
        Op::BitXor => (BinOp::BitXor, Pop, Pop, p),
        Op::Shl => (BinOp::Shl, Pop, Pop, p),
        Op::Shr => (BinOp::Shr, Pop, Pop, p),
        Op::AddC(k) => (BinOp::Add, Pop, Const(k), p),
        Op::SubC(k) => (BinOp::Sub, Pop, Const(k), p),
        Op::MulC(k) => (BinOp::Mul, Pop, Const(k), p),
        Op::ModC(k) => (BinOp::Mod, Pop, Const(k), p),
        Op::AddLC(l, k) => (BinOp::Add, Local(l), Const(k), p),
        Op::SubLC(l, k) => (BinOp::Sub, Local(l), Const(k), p),
        Op::MulLC(l, k) => (BinOp::Mul, Local(l), Const(k), p),
        Op::ModLC(l, k) => (BinOp::Mod, Local(l), Const(k), p),
        Op::AddL(l) => (BinOp::Add, Pop, Local(l), p),
        Op::SubL(l) => (BinOp::Sub, Pop, Local(l), p),
        Op::MulL(l) => (BinOp::Mul, Pop, Local(l), p),
        Op::AddLL(a, b) => (BinOp::Add, Local(a), Local(b), p),
        Op::SubLL(a, b) => (BinOp::Sub, Local(a), Local(b), p),
        Op::MulLL(a, b) => (BinOp::Mul, Local(a), Local(b), p),
        Op::AddLocal(l) => (BinOp::Add, Local(l), Pop, Dst::Local(l)),
        Op::AddLocalL(d, s) => (BinOp::Add, Local(d), Local(s), Dst::Local(d)),
        Op::AddLocalC(d, k) => (BinOp::Add, Local(d), Const(k), Dst::Local(d)),
        _ => return None,
    })
}

/// Comparison instructions as (operator, left, right, what happens with the result).
fn cmp_parts(op: Op) -> Option<(CmpOp, Src, Src, CmpUse)> {
    use CmpOp::*;
    use Src::*;
    let p = CmpUse::Push;
    let n = CmpUse::JumpIfNot;
    let y = CmpUse::JumpIf;
    Some(match op {
        Op::Eq => (Eq, Pop, Pop, p),
        Op::Ne => (Ne, Pop, Pop, p),
        Op::Lt => (Lt, Pop, Pop, p),
        Op::Le => (Le, Pop, Pop, p),
        Op::Gt => (Gt, Pop, Pop, p),
        Op::Ge => (Ge, Pop, Pop, p),
        Op::EqC(k) => (Eq, Pop, Const(k), p),
        Op::NeC(k) => (Ne, Pop, Const(k), p),
        Op::LtC(k) => (Lt, Pop, Const(k), p),
        Op::LeC(k) => (Le, Pop, Const(k), p),
        Op::GtC(k) => (Gt, Pop, Const(k), p),
        Op::GeC(k) => (Ge, Pop, Const(k), p),
        Op::JumpIfNotEq(t) => (Eq, Pop, Pop, n(t)),
        Op::JumpIfNotNe(t) => (Ne, Pop, Pop, n(t)),
        Op::JumpIfNotLt(t) => (Lt, Pop, Pop, n(t)),
        Op::JumpIfNotLe(t) => (Le, Pop, Pop, n(t)),
        Op::JumpIfNotGt(t) => (Gt, Pop, Pop, n(t)),
        Op::JumpIfNotGe(t) => (Ge, Pop, Pop, n(t)),
        Op::JumpIfNotEqC(k, t) => (Eq, Pop, Const(k), n(t)),
        Op::JumpIfNotNeC(k, t) => (Ne, Pop, Const(k), n(t)),
        Op::JumpIfNotLtC(k, t) => (Lt, Pop, Const(k), n(t)),
        Op::JumpIfNotLeC(k, t) => (Le, Pop, Const(k), n(t)),
        Op::JumpIfNotGtC(k, t) => (Gt, Pop, Const(k), n(t)),
        Op::JumpIfNotGeC(k, t) => (Ge, Pop, Const(k), n(t)),
        Op::JumpIfEqC(k, t) => (Eq, Pop, Const(k), y(t)),
        Op::JumpIfNeC(k, t) => (Ne, Pop, Const(k), y(t)),
        Op::JumpIfLtC(k, t) => (Lt, Pop, Const(k), y(t)),
        Op::JumpIfLeC(k, t) => (Le, Pop, Const(k), y(t)),
        Op::JumpIfGtC(k, t) => (Gt, Pop, Const(k), y(t)),
        Op::JumpIfGeC(k, t) => (Ge, Pop, Const(k), y(t)),
        Op::JumpIfNotEqLC(a, k, t) => (Eq, Local(a), Const(k), n(t)),
        Op::JumpIfNotNeLC(a, k, t) => (Ne, Local(a), Const(k), n(t)),
        Op::JumpIfNotLtLC(a, k, t) => (Lt, Local(a), Const(k), n(t)),
        Op::JumpIfNotLeLC(a, k, t) => (Le, Local(a), Const(k), n(t)),
        Op::JumpIfNotGtLC(a, k, t) => (Gt, Local(a), Const(k), n(t)),
        Op::JumpIfNotGeLC(a, k, t) => (Ge, Local(a), Const(k), n(t)),
        Op::JumpIfNotEqLL(a, b, t) => (Eq, Local(a), Local(b), n(t)),
        Op::JumpIfNotNeLL(a, b, t) => (Ne, Local(a), Local(b), n(t)),
        Op::JumpIfNotLtLL(a, b, t) => (Lt, Local(a), Local(b), n(t)),
        Op::JumpIfNotLeLL(a, b, t) => (Le, Local(a), Local(b), n(t)),
        Op::JumpIfNotGtLL(a, b, t) => (Gt, Local(a), Local(b), n(t)),
        Op::JumpIfNotGeLL(a, b, t) => (Ge, Local(a), Local(b), n(t)),
        _ => return None,
    })
}

/// The base form of an inline-cached instruction (compiled code has its own caches).
fn canon(op: Op) -> Op {
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

fn const_ty(v: &Value) -> Ty {
    match v {
        Value::Int(_) => Ty::Int,
        Value::Float(_) => Ty::Float,
        Value::Bool(_) => Ty::Bool,
        Value::None => Ty::NoneT,
        _ => Ty::Any,
    }
}

fn arith_ty(k: BinOp, a: Ty, b: Ty) -> Ty {
    use Ty::*;
    match k {
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::FloorDiv | BinOp::Mod => match (a, b) {
            (Int, Int) => Int,
            _ if a.num() && b.num() => Float,
            _ => Any,
        },
        BinOp::Div => {
            if a.num() && b.num() {
                Float
            } else {
                Any
            }
        }
        BinOp::Pow => Any,
        BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr => {
            if a == Int && b == Int {
                Int
            } else {
                Any
            }
        }
    }
}

struct Analysis {
    ops: Vec<Op>,
    /// State before each instruction (`None` = unreachable).
    states: Vec<Option<St>>,
    /// Instructions that start a basic block.
    leader: Vec<bool>,
    /// Locals kept in registers, with their type.
    promoted: Vec<Option<Ty>>,
    /// Code indices of `try` handlers.
    handlers: Vec<u32>,
    /// Some closure captures a local: close upvalues on return.
    closes: bool,
    /// Loop back-edges (by instruction) whose loop never allocates, so they
    /// need no garbage-collection check.
    pure_loop: Vec<bool>,
}

struct Ctx<'a> {
    f: &'a Function,
    range_slot: Option<u32>,
}

impl Ctx<'_> {
    /// Applies instruction `op` to `st`, reporting local uses. `None` = can't analyze.
    fn step(&self, op: Op, st: &mut St, u: &mut dyn FnMut(Use)) -> Option<()> {
        let nl = st.locals.len();
        macro_rules! pop {
            () => {
                st.stack.pop()?
            };
        }
        macro_rules! push {
            ($t:expr) => {{
                let t = $t;
                st.stack.push(t)
            }};
        }
        macro_rules! local {
            ($l:expr) => {{
                let l = $l as usize;
                if l >= nl {
                    return None;
                }
                st.locals[l]
            }};
        }
        let src = |s: Src, st: &mut St, u: &mut dyn FnMut(Use)| -> Option<Ty> {
            match s {
                Src::Pop => st.stack.pop(),
                Src::Local(l) => {
                    let t = *st.locals.get(l as usize)?;
                    u(Use::Read(l, t));
                    Some(t)
                }
                Src::Const(k) => Some(const_ty(self.f.consts.get(k as usize)?)),
            }
        };
        if let Some((k, sa, sb, dst)) = arith_parts(op) {
            let tb = src(sb, st, u)?;
            let ta = src(sa, st, u)?;
            let r = arith_ty(k, ta, tb);
            match dst {
                Dst::Push => push!(r),
                Dst::Local(l) => {
                    u(Use::Write(l, r));
                    st.locals[l as usize] = r;
                }
            }
            return Some(());
        }
        if let Some((_, sa, sb, use_)) = cmp_parts(op) {
            src(sb, st, u)?;
            src(sa, st, u)?;
            if let CmpUse::Push = use_ {
                push!(Ty::Bool);
            }
            return Some(());
        }
        match op {
            Op::Const(k) => push!(const_ty(self.f.consts.get(k as usize)?)),
            Op::None => push!(Ty::NoneT),
            Op::True | Op::False => push!(Ty::Bool),
            Op::Pop => {
                pop!();
            }
            Op::Dup => {
                let t = *st.stack.last()?;
                push!(t);
            }
            Op::Dup2 => {
                let n = st.stack.len();
                if n < 2 {
                    return None;
                }
                let (a, b) = (st.stack[n - 2], st.stack[n - 1]);
                push!(a);
                push!(b);
            }
            Op::Swap => {
                let n = st.stack.len();
                if n < 2 {
                    return None;
                }
                st.stack.swap(n - 1, n - 2);
            }
            Op::Rot3 => {
                let c = pop!();
                let b = pop!();
                let a = pop!();
                push!(c);
                push!(a);
                push!(b);
            }
            Op::GetLocal(l) => {
                let t = local!(l);
                u(Use::Read(l, t));
                push!(t);
            }
            Op::GetLocal2(a, b) => {
                let ta = local!(a);
                let tb = local!(b);
                u(Use::Read(a, ta));
                u(Use::Read(b, tb));
                push!(ta);
                push!(tb);
            }
            Op::SetLocal(l) => {
                local!(l);
                let t = pop!();
                u(Use::Write(l, t));
                st.locals[l as usize] = t;
            }
            Op::GetGlobal(g) => push!(if Some(g) == self.range_slot { Ty::RangeFn } else { Ty::Any }),
            Op::SetGlobal(_) | Op::SetUpval(_) => {
                pop!();
            }
            Op::GetUpval(_) | Op::Import(_) | Op::Class(_) => push!(Ty::Any),
            Op::CloseUpvals(_) => {}
            Op::Closure(k) | Op::ClosureDefaults(k, _) => {
                if let Op::ClosureDefaults(_, n) = op {
                    for _ in 0..n {
                        pop!();
                    }
                }
                match self.f.consts.get(k as usize)? {
                    Value::Obj(g) if matches!(g.get(), Obj::Func(_)) => {
                        for &(from_local, idx) in g.as_func().captures.iter() {
                            if from_local {
                                local!(idx);
                                u(Use::Pin(idx));
                            }
                        }
                    }
                    _ => return None,
                }
                push!(Ty::Any);
            }
            Op::GetAttr(_) => {
                pop!();
                push!(Ty::Any);
            }
            Op::SetAttr(_) => {
                pop!();
                pop!();
            }
            Op::GetLocalAttr(a, _) => {
                let t = local!(a);
                u(Use::Read(a, t));
                push!(Ty::Any);
            }
            Op::SetLocalAttr(a, _) => {
                let t = local!(a);
                pop!();
                u(Use::Read(a, t));
            }
            Op::GetIndex => {
                pop!();
                pop!();
                push!(Ty::Any);
            }
            Op::SetIndex => {
                pop!();
                pop!();
                pop!();
            }
            Op::Neg => {
                let t = pop!();
                push!(if t.num() { t } else { Ty::Any });
            }
            Op::Invert => {
                let t = pop!();
                push!(if t == Ty::Int { Ty::Int } else { Ty::Any });
            }
            Op::Not => {
                pop!();
                push!(Ty::Bool);
            }
            Op::In => {
                pop!();
                pop!();
                push!(Ty::Bool);
            }
            Op::Jump(_) | Op::JumpIfFalseOrPop(_) | Op::JumpIfTrueOrPop(_) => {
                if !matches!(op, Op::Jump(_)) && st.stack.is_empty() {
                    return None;
                }
            }
            Op::JumpIfFalse(_) | Op::JumpIfTrue(_) => {
                pop!();
            }
            Op::GetIter(s) => {
                let t = pop!();
                local!(s + 2);
                let ty = if t == Ty::RangeObj { Ty::Int } else { Ty::Any };
                for l in s..s + 3 {
                    u(Use::Write(l, ty));
                    if ty == Ty::Any {
                        u(Use::Pin(l));
                    }
                    st.locals[l as usize] = ty;
                }
            }
            Op::ForIter(s, v, _) | Op::ForLoop(s, v, _) => {
                local!(s + 2);
                local!(v);
                for l in s..s + 3 {
                    u(Use::Read(l, st.locals[l as usize]));
                }
                if st.locals[s as usize] == Ty::Int {
                    u(Use::Write(s, Ty::Int));
                    u(Use::Write(v, Ty::Int));
                } else {
                    u(Use::Write(s + 1, Ty::Any));
                    u(Use::Write(v, Ty::Any));
                }
            }
            Op::ListAppend(s) | Op::SetAdd(s) => {
                let t = local!(s);
                pop!();
                u(Use::Read(s, t));
                u(Use::Pin(s));
            }
            Op::DictSet(s) => {
                let t = local!(s);
                pop!();
                pop!();
                u(Use::Read(s, t));
                u(Use::Pin(s));
            }
            Op::Call(n) => {
                for _ in 0..n {
                    pop!();
                }
                let callee = pop!();
                push!(if callee == Ty::RangeFn { Ty::RangeObj } else { Ty::Any });
            }
            Op::CallKw(n, _) | Op::InvokeKw(_, n, _) | Op::Invoke(_, n) => {
                for _ in 0..=n {
                    pop!();
                }
                push!(Ty::Any);
            }
            Op::CallEx => {
                pop!();
                pop!();
                pop!();
                push!(Ty::Any);
            }
            Op::ArgPush | Op::ArgExtend | Op::ArgMerge | Op::Return | Op::Raise => {
                pop!();
            }
            Op::ArgKw => {
                pop!();
                pop!();
            }
            Op::BuildList(n) | Op::BuildTuple(n) | Op::BuildSet(n) => {
                for _ in 0..n {
                    pop!();
                }
                push!(Ty::Any);
            }
            Op::BuildDict(n) => {
                for _ in 0..2 * n {
                    pop!();
                }
                push!(Ty::Any);
            }
            Op::Unpack(n) => {
                pop!();
                for _ in 0..n {
                    push!(Ty::Any);
                }
            }
            Op::GetSlice => {
                for _ in 0..4 {
                    pop!();
                }
                push!(Ty::Any);
            }
            Op::SetupTry(_) => {}
            Op::PopTry => {
                st.hs.pop()?;
            }
            Op::ExcMatch(n) => {
                for _ in 0..=n {
                    pop!();
                }
                push!(Ty::Bool);
            }
            Op::Inherit | Op::Method(_) => {
                pop!();
            }
            _ => return None,
        }
        Some(())
    }

    /// Successors of instruction `i` with the state each one starts with.
    fn edges(&self, i: usize, op: Op, mut st: St) -> Vec<(usize, St)> {
        match op {
            Op::Return | Op::Raise => vec![],
            Op::Jump(t) => vec![(t as usize, st)],
            Op::JumpIfFalseOrPop(t) | Op::JumpIfTrueOrPop(t) => {
                let jump = st.clone();
                st.stack.pop();
                vec![(t as usize, jump), (i + 1, st)]
            }
            Op::ForIter(s, v, exit) => {
                let exit_st = st.clone();
                st.locals[v as usize] = if st.locals[s as usize] == Ty::Int { Ty::Int } else { Ty::Any };
                vec![(i + 1, st), (exit as usize, exit_st)]
            }
            Op::ForLoop(s, v, body) => {
                let done = st.clone();
                st.locals[v as usize] = if st.locals[s as usize] == Ty::Int { Ty::Int } else { Ty::Any };
                vec![(body as usize, st), (i + 1, done)]
            }
            Op::SetupTry(t) => {
                st.hs.push(t);
                vec![(i + 1, st)]
            }
            _ => match op.jump_target() {
                Some(t) => vec![(t as usize, st.clone()), (i + 1, st)],
                None => vec![(i + 1, st)],
            },
        }
    }
}

/// Joins `s` into `slot`; `None` if the shapes disagree (malformed for us).
fn merge(slot: &mut Option<St>, s: St) -> Option<bool> {
    match slot {
        None => {
            *slot = Some(s);
            Some(true)
        }
        Some(old) => {
            if old.stack.len() != s.stack.len() || old.hs != s.hs || old.locals.len() != s.locals.len() {
                return None;
            }
            let mut changed = false;
            for (a, b) in old.locals.iter_mut().chain(old.stack.iter_mut()).zip(s.locals.iter().chain(s.stack.iter())) {
                let j = a.join(*b);
                if j != *a {
                    *a = j;
                    changed = true;
                }
            }
            Some(changed)
        }
    }
}

fn analyze(f: &Function, range_slot: Option<u32>) -> Option<Analysis> {
    let ops: Vec<Op> = f.code.iter().map(|&o| canon(o)).collect();
    let n = ops.len();
    if n == 0 {
        return None;
    }
    let nl = f.num_locals as usize;
    let nparams = f.arity as usize + f.varargs as usize + f.varkw as usize;
    let cx = Ctx { f, range_slot };
    let entry = St {
        locals: (0..nl).map(|i| if i < nparams { Ty::Any } else { Ty::NoneT }).collect(),
        stack: Vec::new(),
        hs: Vec::new(),
    };
    let mut states: Vec<Option<St>> = vec![None; n];
    // handler target -> (stack depth, handlers) at its SetupTry
    let mut setups: HashMap<u32, (usize, Vec<u32>)> = HashMap::new();
    states[0] = Some(entry);
    let mut work = vec![0usize];
    let mut iterations = 0usize;
    while let Some(i) = work.pop() {
        iterations += 1;
        if iterations > 200 * n + 10_000 {
            return None;
        }
        let pre = states[i].clone().unwrap();
        let op = ops[i];
        if let Op::SetupTry(t) = op {
            let info = (pre.stack.len(), pre.hs.clone());
            match setups.get(&t) {
                Some(old) if *old != info => return None,
                _ => {
                    setups.insert(t, info);
                }
            }
        }
        // anything may raise: the innermost handler can see these locals
        if let Some(&t) = pre.hs.last() {
            let (depth, hs) = setups.get(&t)?.clone();
            let hst = St { locals: pre.locals.clone(), stack: vec![Ty::Any; depth + 1], hs };
            if (t as usize) >= n {
                return None;
            }
            if merge(&mut states[t as usize], hst)? {
                work.push(t as usize);
            }
        }
        let mut st = pre;
        cx.step(op, &mut st, &mut |_| {})?;
        for (t, s) in cx.edges(i, op, st) {
            if t >= n {
                return None;
            }
            if merge(&mut states[t], s)? {
                work.push(t);
            }
        }
    }

    let mut leader = vec![false; n];
    leader[0] = true;
    let mut reads = vec![Ty::Bot; nl];
    let mut writes = vec![Ty::Bot; nl];
    let mut pinned = vec![false; nl];
    let mut closes = false;
    let mut handlers: Vec<u32> = setups.keys().copied().collect();
    handlers.sort();
    for &h in handlers.iter() {
        leader[h as usize] = true;
    }
    for i in 0..n {
        let Some(pre) = &states[i] else { continue };
        let op = ops[i];
        if let Some(t) = op.jump_target() {
            leader[t as usize] = true;
            if i + 1 < n {
                leader[i + 1] = true;
            }
        }
        if matches!(op, Op::Return | Op::Raise) && i + 1 < n {
            leader[i + 1] = true;
        }
        if let Op::Closure(k) | Op::ClosureDefaults(k, _) = op {
            if let Value::Obj(g) = f.consts[k as usize] {
                closes |= g.as_func().captures.iter().any(|c| c.0);
            }
        }
        let mut st = pre.clone();
        cx.step(op, &mut st, &mut |u| match u {
            Use::Read(l, t) => reads[l as usize] = reads[l as usize].join(t),
            Use::Write(l, t) => writes[l as usize] = writes[l as usize].join(t),
            Use::Pin(l) => pinned[l as usize] = true,
        })?;
    }
    let promoted = (0..nl)
        .map(|l| {
            let w = writes[l];
            let ok = !pinned[l]
                && matches!(w, Ty::Int | Ty::Float | Ty::Bool)
                && (reads[l] == Ty::Bot || reads[l] == w);
            if ok {
                Some(w)
            } else {
                None
            }
        })
        .collect();
    let mut pure_loop = vec![false; n];
    for i in 0..n {
        let target = match ops[i] {
            Op::ForLoop(_, _, t) => t as usize,
            Op::Jump(t) if (t as usize) <= i => t as usize,
            _ => continue,
        };
        pure_loop[i] = (target..=i).all(|j| match &states[j] {
            Some(st) => is_pure(ops[j], st, f),
            None => true,
        });
    }
    Some(Analysis { ops, states, leader, promoted, handlers, closes, pure_loop })
}

/// Instructions whose compiled fast path can't allocate (slow paths only raise).
fn is_pure(op: Op, st: &St, f: &Function) -> bool {
    let ty = |s: Src, depth_from_top: usize| -> Ty {
        match s {
            Src::Pop => st.stack.get(st.stack.len().wrapping_sub(1 + depth_from_top)).copied().unwrap_or(Ty::Any),
            Src::Local(l) => st.locals[l as usize],
            Src::Const(k) => f.consts.get(k as usize).map_or(Ty::Any, const_ty),
        }
    };
    let scalar = |t: Ty| matches!(t, Ty::Int | Ty::Float | Ty::Bool | Ty::NoneT);
    if let Some((_, a, b, _)) = arith_parts(op) {
        let (tb, ta) = (ty(b, 0), ty(a, if b == Src::Pop { 1 } else { 0 }));
        return ta.num() && tb.num();
    }
    if let Some((_, a, b, _)) = cmp_parts(op) {
        let (tb, ta) = (ty(b, 0), ty(a, if b == Src::Pop { 1 } else { 0 }));
        return scalar(ta) && scalar(tb);
    }
    match op {
        Op::Const(_) | Op::None | Op::True | Op::False | Op::Pop | Op::Dup | Op::Swap | Op::Rot3 => true,
        Op::GetLocal(_) | Op::GetLocal2(..) | Op::SetLocal(_) | Op::Jump(_) | Op::GetGlobal(_) => true,
        Op::ForIter(s, ..) | Op::ForLoop(s, ..) => st.locals[s as usize] == Ty::Int,
        Op::JumpIfFalse(_) | Op::JumpIfTrue(_) | Op::JumpIfFalseOrPop(_) | Op::JumpIfTrueOrPop(_) | Op::Not => {
            st.stack.last().is_some_and(|&t| scalar(t))
        }
        Op::Neg => st.stack.last().is_some_and(|&t| t.num()),
        _ => false,
    }
}

// ---------------------------------------------------------------------------- code generation

/// Where a local lives.
#[derive(Clone, Copy)]
enum Rep {
    /// Its stack slot (tagged value).
    Mem,
    /// A register holding a raw int / float / bool.
    Reg(Variable, Ty),
}

/// A value on the compile-time operand stack.
#[derive(Clone, Copy)]
enum SV {
    /// Already stored in its stack slot; statically of this type.
    Mem(Ty),
    /// Raw int (i64), float (f64), bool (i8) or None in registers.
    I(V),
    F(V),
    B(V),
    N,
    /// Any value as (tag, payload) registers.
    D(V, V),
}

impl SV {
    fn ty(self) -> Ty {
        match self {
            SV::Mem(t) => match t {
                Ty::Int | Ty::Float | Ty::Bool | Ty::NoneT => t,
                _ => Ty::Any,
            },
            SV::I(_) => Ty::Int,
            SV::F(_) => Ty::Float,
            SV::B(_) => Ty::Bool,
            SV::N => Ty::NoneT,
            SV::D(..) => Ty::Any,
        }
    }
}

fn flags() -> MemFlagsData {
    MemFlagsData::trusted()
}

struct Cg<'a> {
    b: FunctionBuilder<'a>,
    f: &'a Function,
    an: &'a Analysis,
    o: Off,
    cc: CallConv,
    ops_ptr: *const Op,
    cells: *const u32,
    nl: usize,
    vm: V,
    frame: V,
    base: V,
    base_idx: V,
    stack_ptr: V,
    globals: V,
    upv: V,
    reps: Vec<Rep>,
    stack: Vec<SV>,
    blocks: Vec<Option<Block>>,
    catch: Block,
    sigs: HashMap<usize, SigRef>,
    /// Instruction being compiled.
    i: usize,
    /// The current block is still open.
    live: bool,
}

impl<'a> Cg<'a> {
    fn new(
        mut b: FunctionBuilder<'a>,
        f: &'a Function,
        an: &'a Analysis,
        o: Off,
        cc: CallConv,
        ops_ptr: *const Op,
        cells: *const u32,
    ) -> Cg<'a> {
        let n = an.ops.len();
        let blocks = (0..n).map(|i| if an.leader[i] && an.states[i].is_some() { Some(b.create_block()) } else { None }).collect();
        let catch = b.create_block();
        b.set_cold_block(catch);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let params = b.block_params(entry).to_vec();
        let (vm, frame) = (params[0], params[1]);
        let base_idx = b.ins().load(I64, flags(), frame, o.fr_base);
        let stack_ptr = b.ins().load(I64, flags(), vm, o.vm_stack_ptr);
        let off = b.ins().ishl_imm_s(base_idx, 4);
        let base = b.ins().iadd(stack_ptr, off);
        let globals = b.ins().load(I64, flags(), frame, o.fr_globals);
        let upv = b.ins().load(I64, flags(), frame, o.fr_upvals);
        // locals that aren't parameters start as None (direct native calls don't
        // go through `push_frame`, which would do this)
        let nparams = f.arity as usize + f.varargs as usize + f.varkw as usize;
        if nparams < f.num_locals as usize {
            let none = b.ins().iconst(I64, tag::NONE);
            for l in nparams..f.num_locals as usize {
                b.ins().store(flags(), none, base, (l * 16) as i32);
            }
        }
        let mut reps = Vec::new();
        for p in an.promoted.iter() {
            reps.push(match p {
                Some(t) => {
                    let ty = match t {
                        Ty::Int => I64,
                        Ty::Float => F64,
                        _ => I8,
                    };
                    let var = b.declare_var(ty);
                    let zero = match t {
                        Ty::Float => b.ins().f64const(0.0),
                        _ => b.ins().iconst(ty, 0),
                    };
                    b.def_var(var, zero);
                    Rep::Reg(var, *t)
                }
                None => Rep::Mem,
            });
        }
        Cg {
            b,
            f,
            an,
            o,
            cc,
            ops_ptr,
            cells,
            nl: f.num_locals as usize,
            vm,
            frame,
            base,
            base_idx,
            stack_ptr,
            globals,
            upv,
            reps,
            stack: Vec::new(),
            blocks,
            catch,
            sigs: HashMap::new(),
            i: 0,
            live: true,
        }
    }

    fn translate(mut self, frontend: cranelift_codegen::isa::TargetFrontendConfig) {
        let first = self.blocks[0].unwrap();
        self.b.ins().jump(first, &[]);
        self.live = false;
        let an = self.an;
        for i in 0..an.ops.len() {
            let Some(pre) = &an.states[i] else { continue };
            if an.leader[i] {
                let blk = self.blocks[i].unwrap();
                if self.live {
                    self.flush();
                    self.b.ins().jump(blk, &[]);
                }
                self.b.switch_to_block(blk);
                self.stack = pre.stack.iter().map(|&t| SV::Mem(t)).collect();
                self.live = true;
            }
            if !self.live {
                continue;
            }
            self.i = i;
            self.op(an.ops[i], pre);
        }
        if self.live {
            self.b.ins().trap(TrapCode::unwrap_user(1));
        }
        self.emit_catch();
        self.b.seal_all_blocks();
        self.b.finalize(frontend);
    }

    // ------------------------------------------------------------ small helpers

    fn ic(&mut self, v: i64) -> V {
        self.b.ins().iconst(I64, v)
    }

    fn ld(&mut self, p: V, off: i32) -> V {
        self.b.ins().load(I64, flags(), p, off)
    }

    fn sto(&mut self, v: V, p: V, off: i32) {
        self.b.ins().store(flags(), v, p, off);
    }

    fn soff(&self, d: usize) -> i32 {
        ((self.nl + d) * 16) as i32
    }

    fn loff(&self, l: u32) -> i32 {
        (l as usize * 16) as i32
    }

    /// Absolute stack index of operand-stack depth `d`.
    fn abs(&mut self, d: usize) -> V {
        self.b.ins().iadd_imm_s(self.base_idx, (self.nl + d) as i64)
    }

    fn slot_ptr(&mut self, d: usize) -> V {
        let off = self.soff(d);
        self.b.ins().iadd_imm_s(self.base, off as i64)
    }


    fn cell(&mut self) -> V {
        let p = unsafe { self.cells.add(2 * self.i) } as i64;
        self.ic(p)
    }

    /// Publishes the current instruction (for tracebacks and the runtime).
    fn set_ip(&mut self) {
        let ip = unsafe { self.f.code.as_ptr().add(self.i + 1) } as i64;
        let v = self.ic(ip);
        self.sto(v, self.frame, self.o.fr_ip);
    }

    fn call_rt(&mut self, f: usize, args: &[V]) -> V {
        let n = args.len();
        let sig = match self.sigs.get(&n) {
            Some(&s) => s,
            None => {
                let mut s = Signature::new(self.cc);
                for _ in 0..n {
                    s.params.push(AbiParam::new(I64));
                }
                s.returns.push(AbiParam::new(I64));
                let r = self.b.import_signature(s);
                self.sigs.insert(n, r);
                r
            }
        };
        let fp = self.ic(f as i64);
        let inst = self.b.ins().call_indirect(sig, fp, args);
        self.b.inst_results(inst)[0]
    }

    /// Continues only if `st` (a runtime status) is 0; otherwise unwinds.
    fn check(&mut self, st: V) {
        let cont = self.b.create_block();
        self.b.ins().brif(st, self.catch, &[], cont, &[]);
        self.b.switch_to_block(cont);
    }

    /// For helpers returning 0/1 or 2 on error: unwinds on 2, else gives the bool.
    fn check_bool(&mut self, st: V) -> V {
        let err = self.b.ins().icmp_imm_s(IntCC::Equal, st, 2);
        let cont = self.b.create_block();
        self.b.ins().brif(err, self.catch, &[], cont, &[]);
        self.b.switch_to_block(cont);
        self.b.ins().ireduce(I8, st)
    }

    fn block(&self, i: usize) -> Block {
        self.blocks[i].expect("jump to a non-leader")
    }

    // ------------------------------------------------------------ values

    fn konst(&mut self, k: u32) -> SV {
        match self.f.consts[k as usize] {
            Value::Int(i) => SV::I(self.ic(i)),
            Value::Float(x) => SV::F(self.b.ins().f64const(x)),
            Value::Bool(b) => SV::B(self.b.ins().iconst(I8, b as i64)),
            Value::None => SV::N,
            Value::Obj(g) => {
                let t = self.ic(tag::OBJ);
                let p = self.ic(g.addr() as i64);
                SV::D(t, p)
            }
            Value::Undef => {
                let t = self.ic(tag::UNDEF);
                let p = self.ic(0);
                SV::D(t, p)
            }
        }
    }

    /// Reads a tagged value of static type `ty` from memory into registers.
    fn load_val(&mut self, ty: Ty, p: V, off: i32) -> SV {
        match ty {
            Ty::Int => SV::I(self.ld(p, off + 8)),
            Ty::Float => SV::F(self.b.ins().load(F64, flags(), p, off + 8)),
            Ty::Bool => SV::B(self.b.ins().load(I8, flags(), p, off + 8)),
            Ty::NoneT => SV::N,
            _ => {
                let t = self.ld(p, off);
                let v = self.ld(p, off + 8);
                SV::D(t, v)
            }
        }
    }

    /// Stores a register value as a tagged value.
    fn write(&mut self, v: SV, p: V, off: i32) {
        let tag_of = |c: &mut Self, t: i64| {
            let t = c.ic(t);
            c.sto(t, p, off);
        };
        match v {
            SV::Mem(_) => unreachable!("write of an in-memory value"),
            SV::I(x) => {
                tag_of(self, tag::INT);
                self.sto(x, p, off + 8);
            }
            SV::F(x) => {
                tag_of(self, tag::FLOAT);
                self.sto(x, p, off + 8);
            }
            SV::B(x) => {
                tag_of(self, tag::BOOL);
                self.sto(x, p, off + 8);
            }
            SV::N => tag_of(self, tag::NONE),
            SV::D(t, x) => {
                self.sto(t, p, off);
                self.sto(x, p, off + 8);
            }
        }
    }

    /// (tag, payload) registers for any value.
    fn dynv(&mut self, v: SV) -> (V, V) {
        match v {
            SV::I(x) => (self.ic(tag::INT), x),
            SV::F(x) => {
                let p = self.b.ins().bitcast(I64, MemFlagsData::new(), x);
                (self.ic(tag::FLOAT), p)
            }
            SV::B(x) => {
                let p = self.b.ins().uextend(I64, x);
                (self.ic(tag::BOOL), p)
            }
            SV::N => (self.ic(tag::NONE), self.ic(0)),
            SV::D(t, p) => (t, p),
            SV::Mem(_) => unreachable!(),
        }
    }

    fn push(&mut self, v: SV) {
        self.stack.push(v);
    }

    /// Pops into registers.
    fn pop(&mut self) -> SV {
        let v = self.stack.pop().expect("stack underflow");
        match v {
            SV::Mem(t) => {
                let off = self.soff(self.stack.len());
                self.load_val(t, self.base, off)
            }
            v => v,
        }
    }

    /// Reads the value `k` places below the top (0 = top) into registers.
    fn peek(&mut self, k: usize) -> SV {
        let d = self.stack.len() - 1 - k;
        match self.stack[d] {
            SV::Mem(t) => {
                let off = self.soff(d);
                self.load_val(t, self.base, off)
            }
            v => v,
        }
    }

    /// Stores every register-held stack value to its slot (state unchanged), so the
    /// stack is complete for the runtime / GC on a slow path.
    fn spill(&mut self) {
        for d in 0..self.stack.len() {
            let v = self.stack[d];
            if !matches!(v, SV::Mem(_)) {
                let off = self.soff(d);
                self.write(v, self.base, off);
            }
        }
    }

    /// Like `spill`, and from now on the values are known to be in memory.
    fn flush(&mut self) {
        for d in 0..self.stack.len() {
            let v = self.stack[d];
            if !matches!(v, SV::Mem(_)) {
                let off = self.soff(d);
                self.write(v, self.base, off);
                self.stack[d] = SV::Mem(v.ty());
            }
        }
    }

    fn get_local(&mut self, l: u32, ty: Ty) -> SV {
        match self.reps[l as usize] {
            Rep::Reg(var, t) => {
                let v = self.b.use_var(var);
                match t {
                    Ty::Int => SV::I(v),
                    Ty::Float => SV::F(v),
                    _ => SV::B(v),
                }
            }
            Rep::Mem => {
                let off = self.loff(l);
                self.load_val(ty, self.base, off)
            }
        }
    }

    fn set_local(&mut self, l: u32, v: SV) {
        match self.reps[l as usize] {
            Rep::Reg(var, t) => {
                let x = match (v, t) {
                    (SV::I(x), Ty::Int) | (SV::F(x), Ty::Float) | (SV::B(x), Ty::Bool) => x,
                    (SV::D(_, p), Ty::Int) => p,
                    (SV::D(_, p), Ty::Float) => self.b.ins().bitcast(F64, MemFlagsData::new(), p),
                    (SV::D(_, p), _) => self.b.ins().ireduce(I8, p),
                    _ => unreachable!("promoted local {} assigned a value of another type", l),
                };
                self.b.def_var(var, x);
            }
            Rep::Mem => {
                let off = self.loff(l);
                self.write(v, self.base, off);
            }
        }
    }

    fn fetch(&mut self, s: Src, pre: &St) -> SV {
        match s {
            Src::Pop => self.pop(),
            Src::Local(l) => self.get_local(l, pre.locals[l as usize]),
            Src::Const(k) => self.konst(k),
        }
    }

    fn tag_is(&mut self, t: V, want: i64) -> V {
        self.b.ins().icmp_imm_s(IntCC::Equal, t, want)
    }

    /// int or float tag
    fn tag_num(&mut self, t: V) -> V {
        let x = self.b.ins().iadd_imm_s(t, -tag::INT);
        self.b.ins().icmp_imm_s(IntCC::UnsignedLessThanOrEqual, x, 1)
    }

    /// Payload of a dynamic int/float as f64.
    fn as_f64(&mut self, t: V, p: V) -> V {
        let is_int = self.tag_is(t, tag::INT);
        let fi = self.b.ins().fcvt_from_sint(F64, p);
        let ff = self.b.ins().bitcast(F64, MemFlagsData::new(), p);
        self.b.ins().select(is_int, fi, ff)
    }

    fn to_f64(&mut self, v: SV) -> V {
        match v {
            SV::I(x) => self.b.ins().fcvt_from_sint(F64, x),
            SV::F(x) => x,
            _ => unreachable!(),
        }
    }

    // ------------------------------------------------------------ arithmetic

    /// Calls the runtime for `a OP b` with the operands stored at depths r, r+1;
    /// the result lands in slot r.
    fn slow_binop(&mut self, k: BinOp, a: SV, b: SV, r: usize) {
        self.spill();
        let (oa, ob) = (self.soff(r), self.soff(r + 1));
        self.write(a, self.base, oa);
        self.write(b, self.base, ob);
        self.set_ip();
        let sp = self.abs(r + 2);
        let kc = self.ic(binop_code(k));
        let (pa, pb) = (self.slot_ptr(r), self.slot_ptr(r + 1));
        let st = self.call_rt(rt_binop as *const () as usize, &[self.vm, sp, kc, pa, pb, pa]);
        self.check(st);
    }

    /// A slow path that can only end in an exception (e.g. int overflow).
    fn slow_binop_raise(&mut self, k: BinOp, a: SV, b: SV, r: usize) {
        self.slow_binop(k, a, b, r);
        self.b.ins().trap(TrapCode::unwrap_user(2));
    }

    fn arith(&mut self, k: BinOp, a: SV, b: SV, r: usize) -> SV {
        let rty = arith_ty(k, a.ty(), b.ty());
        match (a, b) {
            (SV::I(x), SV::I(y)) => self.int_arith(k, x, y, a, b, r),
            _ if a.ty().num() && b.ty().num() => self.float_arith(k, a, b, r, rty),
            _ => self.dyn_arith(k, a, b, r, rty),
        }
    }

    fn int_arith(&mut self, k: BinOp, x: V, y: V, a: SV, b: SV, r: usize) -> SV {
        match k {
            BinOp::Add | BinOp::Sub | BinOp::Mul => {
                let (res, of) = match k {
                    BinOp::Add => self.b.ins().sadd_overflow(x, y),
                    BinOp::Sub => self.b.ins().ssub_overflow(x, y),
                    _ => self.b.ins().smul_overflow(x, y),
                };
                let slow = self.b.create_block();
                let cont = self.b.create_block();
                self.b.set_cold_block(slow);
                self.b.ins().brif(of, slow, &[], cont, &[]);
                self.b.switch_to_block(slow);
                self.slow_binop_raise(k, a, b, r);
                self.b.switch_to_block(cont);
                SV::I(res)
            }
            BinOp::BitAnd => SV::I(self.b.ins().band(x, y)),
            BinOp::BitOr => SV::I(self.b.ins().bor(x, y)),
            BinOp::BitXor => SV::I(self.b.ins().bxor(x, y)),
            BinOp::Div => {
                let zero = self.b.ins().icmp_imm_s(IntCC::Equal, y, 0);
                let slow = self.b.create_block();
                let cont = self.b.create_block();
                self.b.set_cold_block(slow);
                self.b.ins().brif(zero, slow, &[], cont, &[]);
                self.b.switch_to_block(slow);
                self.slow_binop_raise(k, a, b, r);
                self.b.switch_to_block(cont);
                let fx = self.b.ins().fcvt_from_sint(F64, x);
                let fy = self.b.ins().fcvt_from_sint(F64, y);
                SV::F(self.b.ins().fdiv(fx, fy))
            }
            BinOp::FloorDiv | BinOp::Mod | BinOp::Shr => {
                // fast when the divisor is positive (the shift amount in 0..64)
                let ok = if k == BinOp::Shr {
                    self.b.ins().icmp_imm_s(IntCC::UnsignedLessThan, y, 64)
                } else {
                    self.b.ins().icmp_imm_s(IntCC::SignedGreaterThan, y, 0)
                };
                let fast = self.b.create_block();
                let slow = self.b.create_block();
                let merge = self.b.create_block();
                self.b.set_cold_block(slow);
                let res = self.b.append_block_param(merge, I64);
                self.b.ins().brif(ok, fast, &[], slow, &[]);
                self.b.switch_to_block(fast);
                let v = self.int_divmod(k, x, y);
                self.b.ins().jump(merge, &[v.into()]);
                self.b.switch_to_block(slow);
                self.slow_binop(k, a, b, r);
                let off = self.soff(r);
                let v = self.ld(self.base, off + 8);
                self.b.ins().jump(merge, &[v.into()]);
                self.b.switch_to_block(merge);
                SV::I(res)
            }
            BinOp::Shl | BinOp::Pow => {
                self.slow_binop(k, a, b, r);
                let off = self.soff(r);
                let rty = if k == BinOp::Shl { Ty::Int } else { Ty::Any };
                self.load_val(rty, self.base, off)
            }
        }
    }

    /// x // y, x % y (y > 0) or x >> y (0 <= y < 64).
    fn int_divmod(&mut self, k: BinOp, x: V, y: V) -> V {
        match k {
            BinOp::FloorDiv => {
                let q = self.b.ins().sdiv(x, y);
                let m = self.b.ins().imul(q, y);
                let rem = self.b.ins().isub(x, m);
                let adj = self.b.ins().sshr_imm_s(rem, 63);
                self.b.ins().iadd(q, adj)
            }
            BinOp::Mod => {
                let rem = self.b.ins().srem(x, y);
                let neg = self.b.ins().sshr_imm_s(rem, 63);
                let add = self.b.ins().band(neg, y);
                self.b.ins().iadd(rem, add)
            }
            _ => self.b.ins().sshr(x, y),
        }
    }

    fn float_arith(&mut self, k: BinOp, a: SV, b: SV, r: usize, rty: Ty) -> SV {
        match k {
            BinOp::Add | BinOp::Sub | BinOp::Mul => {
                let (x, y) = (self.to_f64(a), self.to_f64(b));
                SV::F(match k {
                    BinOp::Add => self.b.ins().fadd(x, y),
                    BinOp::Sub => self.b.ins().fsub(x, y),
                    _ => self.b.ins().fmul(x, y),
                })
            }
            BinOp::Div => {
                let (x, y) = (self.to_f64(a), self.to_f64(b));
                let zero = self.b.ins().f64const(0.0);
                let is_zero = self.b.ins().fcmp(FloatCC::Equal, y, zero);
                let slow = self.b.create_block();
                let cont = self.b.create_block();
                self.b.set_cold_block(slow);
                self.b.ins().brif(is_zero, slow, &[], cont, &[]);
                self.b.switch_to_block(slow);
                self.slow_binop_raise(k, a, b, r);
                self.b.switch_to_block(cont);
                SV::F(self.b.ins().fdiv(x, y))
            }
            _ => {
                self.slow_binop(k, a, b, r);
                let off = self.soff(r);
                self.load_val(rty, self.base, off)
            }
        }
    }

    fn dyn_arith(&mut self, k: BinOp, a: SV, b: SV, r: usize, rty: Ty) -> SV {
        let inline_int = !matches!(k, BinOp::Shl | BinOp::Pow);
        let inline_float = matches!(k, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div);
        if !inline_int || rty != Ty::Any {
            self.slow_binop(k, a, b, r);
            let off = self.soff(r);
            return self.load_val(rty, self.base, off);
        }
        let (ta, pa) = self.dynv(a);
        let (tb, pb) = self.dynv(b);
        let merge = self.b.create_block();
        let rt = self.b.append_block_param(merge, I64);
        let rp = self.b.append_block_param(merge, I64);
        let int_b = self.b.create_block();
        let not_int = self.b.create_block();
        let slow = self.b.create_block();
        self.b.set_cold_block(slow);
        let ia = self.tag_is(ta, tag::INT);
        let ib = self.tag_is(tb, tag::INT);
        let both = self.b.ins().band(ia, ib);
        self.b.ins().brif(both, int_b, &[], not_int, &[]);

        // int x int
        self.b.switch_to_block(int_b);
        let it = self.ic(tag::INT);
        match k {
            BinOp::Add | BinOp::Sub | BinOp::Mul => {
                let (res, of) = match k {
                    BinOp::Add => self.b.ins().sadd_overflow(pa, pb),
                    BinOp::Sub => self.b.ins().ssub_overflow(pa, pb),
                    _ => self.b.ins().smul_overflow(pa, pb),
                };
                self.b.ins().brif(of, slow, &[], merge, &[it.into(), res.into()]);
            }
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                let res = match k {
                    BinOp::BitAnd => self.b.ins().band(pa, pb),
                    BinOp::BitOr => self.b.ins().bor(pa, pb),
                    _ => self.b.ins().bxor(pa, pb),
                };
                self.b.ins().jump(merge, &[it.into(), res.into()]);
            }
            BinOp::Div => {
                let zero = self.b.ins().icmp_imm_s(IntCC::Equal, pb, 0);
                let ok = self.b.create_block();
                self.b.ins().brif(zero, slow, &[], ok, &[]);
                self.b.switch_to_block(ok);
                let fx = self.b.ins().fcvt_from_sint(F64, pa);
                let fy = self.b.ins().fcvt_from_sint(F64, pb);
                let q = self.b.ins().fdiv(fx, fy);
                let bits = self.b.ins().bitcast(I64, MemFlagsData::new(), q);
                let ft = self.ic(tag::FLOAT);
                self.b.ins().jump(merge, &[ft.into(), bits.into()]);
            }
            _ => {
                let okc = if k == BinOp::Shr {
                    self.b.ins().icmp_imm_s(IntCC::UnsignedLessThan, pb, 64)
                } else {
                    self.b.ins().icmp_imm_s(IntCC::SignedGreaterThan, pb, 0)
                };
                let ok = self.b.create_block();
                self.b.ins().brif(okc, ok, &[], slow, &[]);
                self.b.switch_to_block(ok);
                let res = self.int_divmod(k, pa, pb);
                let it = self.ic(tag::INT);
                self.b.ins().jump(merge, &[it.into(), res.into()]);
            }
        }

        // float (or int/float mix)
        self.b.switch_to_block(not_int);
        if inline_float {
            let na = self.tag_num(ta);
            let nb = self.tag_num(tb);
            let both = self.b.ins().band(na, nb);
            let fl = self.b.create_block();
            self.b.ins().brif(both, fl, &[], slow, &[]);
            self.b.switch_to_block(fl);
            let x = self.as_f64(ta, pa);
            let y = self.as_f64(tb, pb);
            let ft = self.ic(tag::FLOAT);
            if k == BinOp::Div {
                let zero = self.b.ins().f64const(0.0);
                let is_zero = self.b.ins().fcmp(FloatCC::Equal, y, zero);
                let ok = self.b.create_block();
                self.b.ins().brif(is_zero, slow, &[], ok, &[]);
                self.b.switch_to_block(ok);
                let q = self.b.ins().fdiv(x, y);
                let bits = self.b.ins().bitcast(I64, MemFlagsData::new(), q);
                self.b.ins().jump(merge, &[ft.into(), bits.into()]);
            } else {
                let res = match k {
                    BinOp::Add => self.b.ins().fadd(x, y),
                    BinOp::Sub => self.b.ins().fsub(x, y),
                    _ => self.b.ins().fmul(x, y),
                };
                let bits = self.b.ins().bitcast(I64, MemFlagsData::new(), res);
                self.b.ins().jump(merge, &[ft.into(), bits.into()]);
            }
        } else {
            self.b.ins().jump(slow, &[]);
        }

        // anything else: the runtime
        self.b.switch_to_block(slow);
        self.slow_binop(k, SV::D(ta, pa), SV::D(tb, pb), r);
        let off = self.soff(r);
        let t = self.ld(self.base, off);
        let p = self.ld(self.base, off + 8);
        self.b.ins().jump(merge, &[t.into(), p.into()]);

        self.b.switch_to_block(merge);
        SV::D(rt, rp)
    }

    // ------------------------------------------------------------ comparisons

    fn slow_compare(&mut self, k: CmpOp, a: SV, b: SV, r: usize) -> V {
        self.spill();
        let (oa, ob) = (self.soff(r), self.soff(r + 1));
        self.write(a, self.base, oa);
        self.write(b, self.base, ob);
        self.set_ip();
        let sp = self.abs(r + 2);
        let kc = self.ic(cmp_code(k));
        let (pa, pb) = (self.slot_ptr(r), self.slot_ptr(r + 1));
        let st = self.call_rt(rt_compare as *const () as usize, &[self.vm, sp, kc, pa, pb]);
        self.check_bool(st)
    }

    fn intcc(k: CmpOp) -> IntCC {
        match k {
            CmpOp::Eq => IntCC::Equal,
            CmpOp::Ne => IntCC::NotEqual,
            CmpOp::Lt => IntCC::SignedLessThan,
            CmpOp::Le => IntCC::SignedLessThanOrEqual,
            CmpOp::Gt => IntCC::SignedGreaterThan,
            CmpOp::Ge => IntCC::SignedGreaterThanOrEqual,
        }
    }

    fn floatcc(k: CmpOp) -> FloatCC {
        match k {
            CmpOp::Eq => FloatCC::Equal,
            CmpOp::Ne => FloatCC::NotEqual,
            CmpOp::Lt => FloatCC::LessThan,
            CmpOp::Le => FloatCC::LessThanOrEqual,
            CmpOp::Gt => FloatCC::GreaterThan,
            CmpOp::Ge => FloatCC::GreaterThanOrEqual,
        }
    }

    /// `a OP b` as an i8 bool.
    fn compare(&mut self, k: CmpOp, a: SV, b: SV, r: usize) -> V {
        let ordering = !matches!(k, CmpOp::Eq | CmpOp::Ne);
        match (a, b) {
            (SV::I(x), SV::I(y)) => return self.b.ins().icmp(Self::intcc(k), x, y),
            (SV::F(_), SV::F(_)) | (SV::I(_), SV::F(_)) | (SV::F(_), SV::I(_))
                if ordering || matches!((a, b), (SV::F(_), SV::F(_))) =>
            {
                let (x, y) = (self.to_f64(a), self.to_f64(b));
                return self.b.ins().fcmp(Self::floatcc(k), x, y);
            }
            (SV::B(x), SV::B(y)) if !ordering => return self.b.ins().icmp(Self::intcc(k), x, y),
            (SV::N, SV::N) if !ordering => return self.b.ins().iconst(I8, (k == CmpOp::Eq) as i64),
            _ => {}
        }
        // x == None / x != None
        if !ordering && (matches!(b, SV::N) || matches!(a, SV::N)) {
            let other = if matches!(b, SV::N) { a } else { b };
            return self.none_compare(k, a, b, other, r);
        }
        let (ta, pa) = self.dynv(a);
        let (tb, pb) = self.dynv(b);
        let merge = self.b.create_block();
        let res = self.b.append_block_param(merge, I8);
        let int_b = self.b.create_block();
        let not_int = self.b.create_block();
        let slow = self.b.create_block();
        self.b.set_cold_block(slow);
        let ia = self.tag_is(ta, tag::INT);
        let ib = self.tag_is(tb, tag::INT);
        let both = self.b.ins().band(ia, ib);
        self.b.ins().brif(both, int_b, &[], not_int, &[]);
        self.b.switch_to_block(int_b);
        let c = self.b.ins().icmp(Self::intcc(k), pa, pb);
        self.b.ins().jump(merge, &[c.into()]);

        self.b.switch_to_block(not_int);
        let fl_ok = if ordering {
            let na = self.tag_num(ta);
            let nb = self.tag_num(tb);
            self.b.ins().band(na, nb)
        } else {
            let fa = self.tag_is(ta, tag::FLOAT);
            let fb = self.tag_is(tb, tag::FLOAT);
            self.b.ins().band(fa, fb)
        };
        let fl = self.b.create_block();
        self.b.ins().brif(fl_ok, fl, &[], slow, &[]);
        self.b.switch_to_block(fl);
        let x = self.as_f64(ta, pa);
        let y = self.as_f64(tb, pb);
        let c = self.b.ins().fcmp(Self::floatcc(k), x, y);
        self.b.ins().jump(merge, &[c.into()]);

        self.b.switch_to_block(slow);
        let c = self.slow_compare(k, SV::D(ta, pa), SV::D(tb, pb), r);
        self.b.ins().jump(merge, &[c.into()]);
        self.b.switch_to_block(merge);
        res
    }

    /// Equality against None: only instances (which may define `__eq__`) need the runtime.
    fn none_compare(&mut self, k: CmpOp, a: SV, b: SV, other: SV, r: usize) -> V {
        let eq = k == CmpOp::Eq;
        let (t, p) = self.dynv(other);
        let merge = self.b.create_block();
        let res = self.b.append_block_param(merge, I8);
        let objb = self.b.create_block();
        let slow = self.b.create_block();
        self.b.set_cold_block(slow);
        let is_none = self.tag_is(t, tag::NONE);
        let v = if eq { is_none } else { self.b.ins().bxor_imm_s(is_none, 1) };
        let is_obj = self.tag_is(t, tag::OBJ);
        self.b.ins().brif(is_obj, objb, &[], merge, &[v.into()]);
        self.b.switch_to_block(objb);
        let kd = self.b.ins().uload8(I32, flags(), p, self.o.l.kind);
        let inst = self.b.ins().icmp_imm_s(IntCC::Equal, kd, kind::INSTANCE as i64);
        let plain = self.b.ins().iconst(I8, (!eq) as i64);
        self.b.ins().brif(inst, slow, &[], merge, &[plain.into()]);
        self.b.switch_to_block(slow);
        let (a, b) = (if matches!(a, SV::N) { a } else { SV::D(t, p) }, if matches!(b, SV::N) { b } else { SV::D(t, p) });
        let c = self.slow_compare(k, a, b, r);
        self.b.ins().jump(merge, &[c.into()]);
        self.b.switch_to_block(merge);
        res
    }

    /// Truthiness as an i8 bool; `r` = a free slot for the slow path.
    fn truth(&mut self, v: SV, r: usize) -> V {
        match v {
            SV::B(x) => return x,
            SV::I(x) => return self.b.ins().icmp_imm_s(IntCC::NotEqual, x, 0),
            SV::F(x) => {
                let z = self.b.ins().f64const(0.0);
                return self.b.ins().fcmp(FloatCC::NotEqual, x, z);
            }
            SV::N => return self.b.ins().iconst(I8, 0),
            _ => {}
        }
        let (t, p) = self.dynv(v);
        let merge = self.b.create_block();
        let res = self.b.append_block_param(merge, I8);
        let boolb = self.b.create_block();
        let c2 = self.b.create_block();
        let intb = self.b.create_block();
        let c3 = self.b.create_block();
        let slow = self.b.create_block();
        self.b.set_cold_block(slow);
        let is_bool = self.tag_is(t, tag::BOOL);
        self.b.ins().brif(is_bool, boolb, &[], c2, &[]);
        self.b.switch_to_block(boolb);
        let byte = self.b.ins().ireduce(I8, p);
        let x = self.b.ins().icmp_imm_s(IntCC::NotEqual, byte, 0);
        self.b.ins().jump(merge, &[x.into()]);
        self.b.switch_to_block(c2);
        let is_int = self.tag_is(t, tag::INT);
        self.b.ins().brif(is_int, intb, &[], c3, &[]);
        self.b.switch_to_block(intb);
        let x = self.b.ins().icmp_imm_s(IntCC::NotEqual, p, 0);
        self.b.ins().jump(merge, &[x.into()]);
        self.b.switch_to_block(c3);
        let is_none = self.tag_is(t, tag::NONE);
        let f = self.b.ins().iconst(I8, 0);
        self.b.ins().brif(is_none, merge, &[f.into()], slow, &[]);
        self.b.switch_to_block(slow);
        self.spill();
        let off = self.soff(r);
        self.write(SV::D(t, p), self.base, off);
        self.set_ip();
        let sp = self.abs(r + 1);
        let ptr = self.slot_ptr(r);
        let st = self.call_rt(rt_truthy as *const () as usize, &[self.vm, sp, ptr]);
        let x = self.check_bool(st);
        self.b.ins().jump(merge, &[x.into()]);
        self.b.switch_to_block(merge);
        res
    }

    // ------------------------------------------------------------ control flow

    /// Branches on `c` (both successors are block starts).
    fn branch(&mut self, c: V, yes: usize, no: usize) {
        self.flush();
        let (y, n) = (self.block(yes), self.block(no));
        self.b.ins().brif(c, y, &[], n, &[]);
        self.live = false;
    }

    /// Collects garbage if due (a safe point: loop back-edges).
    fn gc_check(&mut self, depth: usize) {
        let since = self.ld(self.vm, self.o.vm_since_gc);
        let thr = self.ld(self.vm, self.o.vm_threshold);
        let due = self.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, since, thr);
        let gc = self.b.create_block();
        let cont = self.b.create_block();
        self.b.set_cold_block(gc);
        self.b.ins().brif(due, gc, &[], cont, &[]);
        self.b.switch_to_block(gc);
        let sp = self.abs(depth);
        self.call_rt(rt_gc as *const () as usize, &[self.vm, sp]);
        self.b.ins().jump(cont, &[]);
        self.b.switch_to_block(cont);
    }

    /// Hands the rest of this call to the interpreter at instruction `ip`
    /// (the operand stack must already be in memory).
    fn deopt(&mut self, ip: usize, depth: usize) {
        for l in 0..self.nl {
            if let Rep::Reg(var, t) = self.reps[l] {
                let v = self.b.use_var(var);
                let sv = match t {
                    Ty::Int => SV::I(v),
                    Ty::Float => SV::F(v),
                    _ => SV::B(v),
                };
                let off = self.loff(l as u32);
                self.write(sv, self.base, off);
            }
        }
        let ipv = self.ic(ip as i64);
        let sp = self.abs(depth);
        let st = self.call_rt(rt_deopt as *const () as usize, &[self.vm, self.frame, ipv, sp]);
        self.b.ins().return_(&[st]);
    }

    fn emit_catch(&mut self) {
        self.b.switch_to_block(self.catch);
        if self.an.handlers.is_empty() {
            let one = self.ic(ERR as i64);
            self.b.ins().return_(&[one]);
            return;
        }
        let t = self.call_rt(rt_catch as *const () as usize, &[self.vm, self.frame]);
        let fail = self.b.create_block();
        let mut sw = Switch::new();
        for &h in self.an.handlers.iter() {
            if let Some(blk) = self.blocks[h as usize] {
                sw.set_entry(h as u128, blk);
            }
        }
        sw.emit(&mut self.b, t, fail);
        self.b.switch_to_block(fail);
        let one = self.ic(ERR as i64);
        self.b.ins().return_(&[one]);
    }

    /// Runs `op` through the interpreter's out-of-line implementation.
    fn cold(&mut self, op: Op, pre: &St) {
        self.flush();
        self.set_ip();
        let sp = self.abs(self.stack.len());
        let opp = self.ic(unsafe { self.ops_ptr.add(self.i) } as i64);
        let st = self.call_rt(rt_cold as *const () as usize, &[self.vm, sp, opp]);
        self.check(st);
        self.reset_stack(op, pre);
    }

    /// After a runtime call: the stack is in memory with the post-instruction types.
    fn reset_stack(&mut self, op: Op, pre: &St) {
        let mut post = pre.clone();
        let cx = Ctx { f: self.f, range_slot: None };
        let _ = cx.step(op, &mut post, &mut |_| {});
        let n = post.stack.len();
        self.stack.truncate(n.min(self.stack.len()));
        while self.stack.len() < n {
            self.stack.push(SV::Mem(Ty::Any));
        }
        for (d, t) in post.stack.iter().enumerate() {
            if let SV::Mem(_) = self.stack[d] {
                self.stack[d] = SV::Mem(match t {
                    Ty::RangeFn | Ty::RangeObj => Ty::Any,
                    t => *t,
                });
            }
        }
    }
}

impl Cg<'_> {
    fn op(&mut self, op: Op, pre: &St) {
        let d = pre.stack.len();
        debug_assert_eq!(d, self.stack.len());
        let pops = |a: Src, b: Src| (a == Src::Pop) as usize + (b == Src::Pop) as usize;
        if let Some((k, sa, sb, dst)) = arith_parts(op) {
            let r = d - pops(sa, sb);
            let b = self.fetch(sb, pre);
            let a = self.fetch(sa, pre);
            let v = self.arith(k, a, b, r);
            match dst {
                Dst::Push => self.push(v),
                Dst::Local(l) => self.set_local(l, v),
            }
            return;
        }
        if let Some((k, sa, sb, use_)) = cmp_parts(op) {
            let r = d - pops(sa, sb);
            let b = self.fetch(sb, pre);
            let a = self.fetch(sa, pre);
            let c = self.compare(k, a, b, r);
            match use_ {
                CmpUse::Push => self.push(SV::B(c)),
                CmpUse::JumpIfNot(t) => self.branch(c, self.i + 1, t as usize),
                CmpUse::JumpIf(t) => self.branch(c, t as usize, self.i + 1),
            }
            return;
        }
        match op {
            Op::Const(k) => {
                let v = self.konst(k);
                self.push(v);
            }
            Op::None => self.push(SV::N),
            Op::True | Op::False => {
                let v = self.b.ins().iconst(I8, (op == Op::True) as i64);
                self.push(SV::B(v));
            }
            Op::Pop => {
                self.stack.pop();
            }
            Op::Dup => {
                let v = self.peek(0);
                self.push(v);
            }
            Op::Dup2 => {
                let a = self.peek(1);
                let b = self.peek(0);
                self.push(a);
                self.push(b);
            }
            Op::Swap => {
                let b = self.pop();
                let a = self.pop();
                self.push(b);
                self.push(a);
            }
            Op::Rot3 => {
                let c = self.pop();
                let b = self.pop();
                let a = self.pop();
                self.push(c);
                self.push(a);
                self.push(b);
            }
            Op::GetLocal(l) => {
                let v = self.get_local(l, pre.locals[l as usize]);
                self.push(v);
            }
            Op::GetLocal2(a, b) => {
                let va = self.get_local(a, pre.locals[a as usize]);
                let vb = self.get_local(b, pre.locals[b as usize]);
                self.push(va);
                self.push(vb);
            }
            Op::SetLocal(l) => {
                let v = self.pop();
                self.set_local(l, v);
            }
            Op::GetGlobal(g) => {
                let off = (g as usize * 16) as i32;
                let t = self.ld(self.globals, off);
                let undef = self.b.ins().icmp_imm_s(IntCC::Equal, t, tag::UNDEF);
                let err = self.b.create_block();
                let cont = self.b.create_block();
                self.b.set_cold_block(err);
                self.b.ins().brif(undef, err, &[], cont, &[]);
                self.b.switch_to_block(err);
                self.spill();
                self.set_ip();
                let gv = self.ic(g as i64);
                self.call_rt(rt_name_error as *const () as usize, &[self.vm, gv]);
                self.b.ins().jump(self.catch, &[]);
                self.b.switch_to_block(cont);
                let p = self.ld(self.globals, off + 8);
                self.push(SV::D(t, p));
            }
            Op::SetGlobal(g) => {
                let v = self.pop();
                self.write(v, self.globals, (g as usize * 16) as i32);
            }
            Op::GetUpval(i) => {
                let u = self.ld(self.upv, (i * 8) as i32);
                let p = self.ld(u, self.o.l.upval_ptr);
                let t = self.ld(p, 0);
                let v = self.ld(p, 8);
                self.push(SV::D(t, v));
            }
            Op::SetUpval(i) => {
                let v = self.pop();
                let u = self.ld(self.upv, (i * 8) as i32);
                let p = self.ld(u, self.o.l.upval_ptr);
                self.write(v, p, 0);
            }
            Op::GetAttr(n) => {
                let obj = self.pop();
                let v = self.get_attr(obj, n, d - 1);
                self.push(v);
            }
            Op::SetAttr(n) => {
                let v = self.pop();
                let obj = self.pop();
                self.set_attr(obj, v, n, d - 2);
            }
            Op::GetLocalAttr(a, n) => {
                let obj = self.get_local(a, pre.locals[a as usize]);
                let v = self.get_attr(obj, n, d);
                self.push(v);
            }
            Op::SetLocalAttr(a, n) => {
                let v = self.pop();
                let obj = self.get_local(a, pre.locals[a as usize]);
                self.set_attr(obj, v, n, d - 1);
            }
            Op::GetIndex => {
                let idx = self.pop();
                let obj = self.pop();
                let v = self.get_index(obj, idx, d - 2);
                self.push(v);
            }
            Op::SetIndex => self.set_index(pre),
            Op::Neg => {
                let a = self.pop();
                let v = self.neg(a, d - 1);
                self.push(v);
            }
            Op::Invert if pre.stack[d - 1] == Ty::Int => {
                let a = self.pop();
                if let SV::I(x) = a {
                    let v = self.b.ins().bnot(x);
                    self.push(SV::I(v));
                } else {
                    unreachable!()
                }
            }
            Op::Not => {
                let a = self.pop();
                let c = self.truth(a, d - 1);
                let v = self.b.ins().bxor_imm_s(c, 1);
                self.push(SV::B(v));
            }
            Op::Jump(t) => {
                self.flush();
                if (t as usize) <= self.i && !self.an.pure_loop[self.i] {
                    self.gc_check(d);
                }
                let blk = self.block(t as usize);
                self.b.ins().jump(blk, &[]);
                self.live = false;
            }
            Op::JumpIfFalse(t) | Op::JumpIfTrue(t) => {
                let a = self.pop();
                let c = self.truth(a, d - 1);
                if let Op::JumpIfFalse(_) = op {
                    self.branch(c, self.i + 1, t as usize);
                } else {
                    self.branch(c, t as usize, self.i + 1);
                }
            }
            Op::JumpIfFalseOrPop(t) | Op::JumpIfTrueOrPop(t) => {
                let a = self.peek(0);
                let c = self.truth(a, d - 1);
                if let Op::JumpIfFalseOrPop(_) = op {
                    self.branch(c, self.i + 1, t as usize);
                } else {
                    self.branch(c, t as usize, self.i + 1);
                }
            }
            Op::GetIter(s) if pre.stack[d - 1] == Ty::RangeObj => self.range_iter(s, d),
            Op::ForIter(s, v, exit) => self.for_step(s, v, pre, self.i + 1, exit as usize, false),
            Op::ForLoop(s, v, body) => self.for_step(s, v, pre, body as usize, self.i + 1, true),
            Op::ListAppend(s) => {
                // comprehension target: always a list
                let v = self.pop();
                let off = self.loff(s);
                let g = self.ld(self.base, off + 8);
                let done = self.b.create_block();
                self.list_push(g, v, d - 1, done);
                self.b.switch_to_block(done);
            }
            Op::Call(n) => self.call(n as usize, d),
            Op::Invoke(name, n) => self.invoke(name, n as usize, d),
            Op::Return => {
                let v = self.pop();
                let ret = self.ld(self.frame, self.o.fr_ret);
                let off = self.b.ins().ishl_imm_s(ret, 4);
                let p = self.b.ins().iadd(self.stack_ptr, off);
                self.write(v, p, 0);
                if self.an.closes {
                    self.call_rt(rt_close as *const () as usize, &[self.vm, self.base_idx]);
                }
                let n = self.ld(self.vm, self.o.vm_frames_len);
                let n1 = self.b.ins().iadd_imm_s(n, -1);
                self.sto(n1, self.vm, self.o.vm_frames_len);
                let ok = self.ic(OK as i64);
                self.b.ins().return_(&[ok]);
                self.live = false;
            }
            Op::SetupTry(t) => {
                self.flush();
                let sp = self.abs(d);
                let tv = self.ic(t as i64);
                self.call_rt(rt_setup_try as *const () as usize, &[self.vm, sp, tv]);
            }
            Op::PopTry => {
                self.call_rt(rt_pop_try as *const () as usize, &[self.vm]);
            }
            Op::Raise => {
                self.flush();
                self.set_ip();
                let sp = self.abs(d);
                let opp = self.ic(unsafe { self.ops_ptr.add(self.i) } as i64);
                self.call_rt(rt_cold as *const () as usize, &[self.vm, sp, opp]);
                self.b.ins().jump(self.catch, &[]);
                self.live = false;
            }
            Op::CloseUpvals(_)
            | Op::Closure(_)
            | Op::ClosureDefaults(..)
            | Op::In
            | Op::Invert
            | Op::GetIter(_)
            | Op::BuildList(_)
            | Op::BuildTuple(_)
            | Op::BuildSet(_)
            | Op::BuildDict(_)
            | Op::Unpack(_)
            | Op::GetSlice
            | Op::Import(_)
            | Op::SetAdd(_)
            | Op::DictSet(_)
            | Op::CallKw(..)
            | Op::InvokeKw(..)
            | Op::CallEx
            | Op::ArgPush
            | Op::ArgExtend
            | Op::ArgKw
            | Op::ArgMerge
            | Op::ExcMatch(_)
            | Op::Class(_)
            | Op::Inherit
            | Op::Method(_) => self.cold(op, pre),
            other => unreachable!("instruction {:?} reached the code generator", other),
        }
    }

    fn neg(&mut self, a: SV, r: usize) -> SV {
        match a {
            SV::F(x) => return SV::F(self.b.ins().fneg(x)),
            SV::I(x) => {
                let min = self.b.ins().icmp_imm_s(IntCC::Equal, x, i64::MIN);
                let slow = self.b.create_block();
                let cont = self.b.create_block();
                self.b.set_cold_block(slow);
                self.b.ins().brif(min, slow, &[], cont, &[]);
                self.b.switch_to_block(slow);
                self.slow_neg(a, r);
                self.b.ins().trap(TrapCode::unwrap_user(3));
                self.b.switch_to_block(cont);
                return SV::I(self.b.ins().ineg(x));
            }
            _ => {}
        }
        let (t, p) = self.dynv(a);
        let merge = self.b.create_block();
        let rt = self.b.append_block_param(merge, I64);
        let rp = self.b.append_block_param(merge, I64);
        let intb = self.b.create_block();
        let c2 = self.b.create_block();
        let flb = self.b.create_block();
        let slow = self.b.create_block();
        self.b.set_cold_block(slow);
        let is_int = self.tag_is(t, tag::INT);
        self.b.ins().brif(is_int, intb, &[], c2, &[]);
        self.b.switch_to_block(intb);
        let min = self.b.ins().icmp_imm_s(IntCC::Equal, p, i64::MIN);
        let ok = self.b.create_block();
        self.b.ins().brif(min, slow, &[], ok, &[]);
        self.b.switch_to_block(ok);
        let n = self.b.ins().ineg(p);
        self.b.ins().jump(merge, &[t.into(), n.into()]);
        self.b.switch_to_block(c2);
        let is_f = self.tag_is(t, tag::FLOAT);
        self.b.ins().brif(is_f, flb, &[], slow, &[]);
        self.b.switch_to_block(flb);
        let sign = self.ic(i64::MIN);
        let n = self.b.ins().bxor(p, sign);
        self.b.ins().jump(merge, &[t.into(), n.into()]);
        self.b.switch_to_block(slow);
        self.slow_neg(SV::D(t, p), r);
        let off = self.soff(r);
        let t2 = self.ld(self.base, off);
        let p2 = self.ld(self.base, off + 8);
        self.b.ins().jump(merge, &[t2.into(), p2.into()]);
        self.b.switch_to_block(merge);
        SV::D(rt, rp)
    }

    fn slow_neg(&mut self, a: SV, r: usize) {
        self.spill();
        let off = self.soff(r);
        self.write(a, self.base, off);
        self.set_ip();
        let sp = self.abs(r + 1);
        let p = self.slot_ptr(r);
        let st = self.call_rt(rt_neg as *const () as usize, &[self.vm, sp, p, p]);
        self.check(st);
    }

    /// `GetIter` on `range(...)`: the loop state is three ints, checked here.
    fn range_iter(&mut self, s: u32, d: usize) {
        self.flush();
        let off = self.soff(d - 1);
        let t = self.ld(self.base, off);
        let p = self.ld(self.base, off + 8);
        let deopt = self.b.create_block();
        self.b.set_cold_block(deopt);
        let kb = self.b.create_block();
        let ok = self.b.create_block();
        let is_obj = self.tag_is(t, tag::OBJ);
        self.b.ins().brif(is_obj, kb, &[], deopt, &[]);
        self.b.switch_to_block(kb);
        let k = self.b.ins().uload8(I32, flags(), p, self.o.l.kind);
        let is_range = self.b.ins().icmp_imm_s(IntCC::Equal, k, kind::RANGE as i64);
        self.b.ins().brif(is_range, ok, &[], deopt, &[]);
        self.b.switch_to_block(deopt);
        self.deopt(self.i, d);
        self.b.switch_to_block(ok);
        let start = self.ld(p, self.o.l.range_start);
        let stop = self.ld(p, self.o.l.range_stop);
        let step = self.ld(p, self.o.l.range_step);
        self.stack.pop();
        self.set_local(s, SV::I(start));
        self.set_local(s + 1, SV::I(stop));
        self.set_local(s + 2, SV::I(step));
    }

    /// `ForIter` / `ForLoop`: advance the loop in locals[s..s+3]; on an item, store
    /// it in locals[v] and go to `more`, else to `done`.
    fn for_step(&mut self, s: u32, v: u32, pre: &St, more: usize, done: usize, back_edge: bool) {
        let back_edge = back_edge && !self.an.pure_loop[self.i];
        self.flush();
        let depth = self.stack.len();
        let (more_b, done_b) = (self.block(more), self.block(done));
        // a fresh block per successor so the back-edge can run the GC check
        let take = self.b.create_block();
        if pre.locals[s as usize] == Ty::Int {
            let (cur, stop, step) = match (
                self.get_local(s, Ty::Int),
                self.get_local(s + 1, Ty::Int),
                self.get_local(s + 2, Ty::Int),
            ) {
                (SV::I(a), SV::I(b), SV::I(c)) => (a, b, c),
                _ => unreachable!(),
            };
            let c = self.range_more(cur, stop, step);
            self.b.ins().brif(c, take, &[], done_b, &[]);
            self.b.switch_to_block(take);
            self.set_local(v, SV::I(cur));
            let next = self.b.ins().iadd(cur, step);
            self.set_local(s, SV::I(next));
            if back_edge {
                self.gc_check(depth);
            }
            self.b.ins().jump(more_b, &[]);
            self.live = false;
            return;
        }
        let so = self.loff(s);
        let t0 = self.ld(self.base, so);
        let rangeb = self.b.create_block();
        let objc = self.b.create_block();
        let kindb = self.b.create_block();
        let tupc = self.b.create_block();
        let listb = self.b.create_block();
        let tupb = self.b.create_block();
        let elem = self.b.create_block();
        let len = self.b.append_block_param(elem, I64);
        let items = self.b.append_block_param(elem, I64);
        let take_e = self.b.create_block();
        let is_int = self.tag_is(t0, tag::INT);
        self.b.ins().brif(is_int, rangeb, &[], objc, &[]);

        // range state: [cur, stop, step]
        self.b.switch_to_block(rangeb);
        let cur = self.ld(self.base, so + 8);
        let stop = self.ld(self.base, so + 24);
        let step = self.ld(self.base, so + 40);
        let c = self.range_more(cur, stop, step);
        let tr = self.b.create_block();
        self.b.ins().brif(c, tr, &[], done_b, &[]);
        self.b.switch_to_block(tr);
        self.set_local(v, SV::I(cur));
        let next = self.b.ins().iadd(cur, step);
        self.sto(next, self.base, so + 8);
        self.b.ins().jump(take, &[]);

        // sequence state: [list or tuple, index, -]
        self.b.switch_to_block(objc);
        let is_obj = self.tag_is(t0, tag::OBJ);
        self.b.ins().brif(is_obj, kindb, &[], done_b, &[]);
        self.b.switch_to_block(kindb);
        let g = self.ld(self.base, so + 8);
        let k = self.b.ins().uload8(I32, flags(), g, self.o.l.kind);
        let is_list = self.b.ins().icmp_imm_s(IntCC::Equal, k, kind::LIST as i64);
        self.b.ins().brif(is_list, listb, &[], tupc, &[]);
        self.b.switch_to_block(tupc);
        let is_tup = self.b.ins().icmp_imm_s(IntCC::Equal, k, kind::TUPLE as i64);
        self.b.ins().brif(is_tup, tupb, &[], done_b, &[]);
        self.b.switch_to_block(listb);
        let n = self.ld(g, self.o.l.list_len);
        let ptr = self.ld(g, self.o.l.list_ptr);
        self.b.ins().jump(elem, &[n.into(), ptr.into()]);
        self.b.switch_to_block(tupb);
        let n = self.ld(g, self.o.l.tuple_len);
        let ptr = self.ld(g, self.o.l.tuple_ptr);
        self.b.ins().jump(elem, &[n.into(), ptr.into()]);
        self.b.switch_to_block(elem);
        let idx = self.ld(self.base, so + 24);
        let inb = self.b.ins().icmp(IntCC::UnsignedLessThan, idx, len);
        self.b.ins().brif(inb, take_e, &[], done_b, &[]);
        self.b.switch_to_block(take_e);
        let eoff = self.b.ins().ishl_imm_s(idx, 4);
        let e = self.b.ins().iadd(items, eoff);
        let et = self.ld(e, 0);
        let ep = self.ld(e, 8);
        self.set_local(v, SV::D(et, ep));
        let idx1 = self.b.ins().iadd_imm_s(idx, 1);
        self.sto(idx1, self.base, so + 24);
        self.b.ins().jump(take, &[]);

        self.b.switch_to_block(take);
        if back_edge {
            self.gc_check(depth);
        }
        self.b.ins().jump(more_b, &[]);
        self.live = false;
    }

    /// Does a range with this state have another item?
    fn range_more(&mut self, cur: V, stop: V, step: V) -> V {
        let pos = self.b.ins().icmp_imm_s(IntCC::SignedGreaterThan, step, 0);
        let lt = self.b.ins().icmp(IntCC::SignedLessThan, cur, stop);
        let gt = self.b.ins().icmp(IntCC::SignedGreaterThan, cur, stop);
        let neg = self.b.ins().icmp_imm_s(IntCC::SignedLessThan, step, 0);
        let gt = self.b.ins().band(gt, neg);
        self.b.ins().select(pos, lt, gt)
    }
}

// ---------------------------------------------------------------------------- inline caches & calls

/// A fresh instance of class `cls` into `*out` (the constructor's frame slot).
extern "C" fn rt_new_instance(vm: &mut Vm, cls: u64, out: *mut Value) -> u64 {
    let g: Gc = unsafe { std::mem::transmute::<u64, Gc>(cls) };
    let n = g.as_class().slot_count as usize;
    let inst = vm.heap.alloc(Obj::Instance(Instance::new(g, vec![Value::Undef; n])));
    unsafe { *out = Value::Obj(inst) };
    OK
}

/// `list.append(v)` from the `Invoke` fast path.
extern "C" fn rt_list_push(_vm: &mut Vm, list: u64, v: *const Value) -> u64 {
    let g: Gc = unsafe { std::mem::transmute::<u64, Gc>(list) };
    if let Obj::List(l) = g.get_mut() {
        l.push(unsafe { *v });
    }
    OK
}

impl Cg<'_> {
    /// Branches to `yes` if (t, p) is an instance whose class shape matches this
    /// instruction's cache cell; passes (class ptr) along. Otherwise to `no`.
    fn shape_check(&mut self, t: V, p: V, no: Block) -> V {
        let b1 = self.b.create_block();
        let b2 = self.b.create_block();
        let b3 = self.b.create_block();
        let is_obj = self.tag_is(t, tag::OBJ);
        self.b.ins().brif(is_obj, b1, &[], no, &[]);
        self.b.switch_to_block(b1);
        let k = self.b.ins().uload8(I32, flags(), p, self.o.l.kind);
        let is_inst = self.b.ins().icmp_imm_s(IntCC::Equal, k, kind::INSTANCE as i64);
        self.b.ins().brif(is_inst, b2, &[], no, &[]);
        self.b.switch_to_block(b2);
        let cls = self.ld(p, self.o.l.inst_cls);
        let id = self.b.ins().uload32(flags(), cls, self.o.l.class_id);
        let cell = self.cell();
        let cid = self.b.ins().uload32(flags(), cell, 0);
        let hit = self.b.ins().icmp(IntCC::Equal, id, cid);
        self.b.ins().brif(hit, b3, &[], no, &[]);
        self.b.switch_to_block(b3);
        cls
    }

    /// Address of field `cell.slot` of instance `p`, or a jump to `no` if the
    /// instance doesn't have that many fields yet.
    fn field_addr(&mut self, p: V, no: Block) -> V {
        let cell = self.cell();
        let slot = self.b.ins().uload32(flags(), cell, 4);
        let len = self.ld(p, self.o.l.inst_fields_len);
        let inb = self.b.ins().icmp(IntCC::UnsignedLessThan, slot, len);
        let ok = self.b.create_block();
        self.b.ins().brif(inb, ok, &[], no, &[]);
        self.b.switch_to_block(ok);
        let fp = self.ld(p, self.o.l.inst_fields_ptr);
        let off = self.b.ins().ishl_imm_s(slot, 4);
        self.b.ins().iadd(fp, off)
    }

    /// `obj.name` with an inline cache; `r` = scratch slot (also the result slot
    /// on the slow path).
    fn get_attr(&mut self, obj: SV, n: u32, r: usize) -> SV {
        let (t, p) = self.dynv(obj);
        let merge = self.b.create_block();
        let rt = self.b.append_block_param(merge, I64);
        let rp = self.b.append_block_param(merge, I64);
        let slow = self.b.create_block();
        self.b.set_cold_block(slow);
        self.shape_check(t, p, slow);
        let e = self.field_addr(p, slow);
        let vt = self.ld(e, 0);
        let vp = self.ld(e, 8);
        let set = self.b.ins().icmp_imm_s(IntCC::NotEqual, vt, tag::UNDEF);
        self.b.ins().brif(set, merge, &[vt.into(), vp.into()], slow, &[]);

        self.b.switch_to_block(slow);
        self.spill();
        let off = self.soff(r);
        self.write(SV::D(t, p), self.base, off);
        self.set_ip();
        let sp = self.abs(r + 1);
        let ptr = self.slot_ptr(r);
        let nv = self.ic(n as i64);
        let cell = self.cell();
        let st = self.call_rt(rt_getattr as *const () as usize, &[self.vm, sp, ptr, nv, ptr, cell]);
        self.check(st);
        let t2 = self.ld(self.base, off);
        let p2 = self.ld(self.base, off + 8);
        self.b.ins().jump(merge, &[t2.into(), p2.into()]);
        self.b.switch_to_block(merge);
        SV::D(rt, rp)
    }

    /// `obj.name = v` with an inline cache; `r`, `r+1` = scratch slots.
    fn set_attr(&mut self, obj: SV, v: SV, n: u32, r: usize) {
        let (t, p) = self.dynv(obj);
        let done = self.b.create_block();
        let slow = self.b.create_block();
        self.b.set_cold_block(slow);
        self.shape_check(t, p, slow);
        let e = self.field_addr(p, slow);
        self.write(v, e, 0);
        self.b.ins().jump(done, &[]);

        self.b.switch_to_block(slow);
        self.spill();
        let (o0, o1) = (self.soff(r), self.soff(r + 1));
        self.write(SV::D(t, p), self.base, o0);
        self.write(v, self.base, o1);
        self.set_ip();
        let sp = self.abs(r + 2);
        let (po, pv) = (self.slot_ptr(r), self.slot_ptr(r + 1));
        let nv = self.ic(n as i64);
        let cell = self.cell();
        let st = self.call_rt(rt_setattr as *const () as usize, &[self.vm, sp, po, nv, pv, cell]);
        self.check(st);
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(done);
    }

    /// (length, items pointer) of a list or tuple `p` (tag already checked), or a
    /// jump to `no`.
    fn seq_parts(&mut self, p: V, lists_only: bool, no: Block) -> (V, V) {
        let k = self.b.ins().uload8(I32, flags(), p, self.o.l.kind);
        let got = self.b.create_block();
        let len = self.b.append_block_param(got, I64);
        let items = self.b.append_block_param(got, I64);
        let listb = self.b.create_block();
        let not_list = self.b.create_block();
        let is_list = self.b.ins().icmp_imm_s(IntCC::Equal, k, kind::LIST as i64);
        self.b.ins().brif(is_list, listb, &[], not_list, &[]);
        self.b.switch_to_block(listb);
        let n = self.ld(p, self.o.l.list_len);
        let ptr = self.ld(p, self.o.l.list_ptr);
        self.b.ins().jump(got, &[n.into(), ptr.into()]);
        self.b.switch_to_block(not_list);
        if lists_only {
            self.b.ins().jump(no, &[]);
        } else {
            let tupb = self.b.create_block();
            let is_tup = self.b.ins().icmp_imm_s(IntCC::Equal, k, kind::TUPLE as i64);
            self.b.ins().brif(is_tup, tupb, &[], no, &[]);
            self.b.switch_to_block(tupb);
            let n = self.ld(p, self.o.l.tuple_len);
            let ptr = self.ld(p, self.o.l.tuple_ptr);
            self.b.ins().jump(got, &[n.into(), ptr.into()]);
        }
        self.b.switch_to_block(got);
        (len, items)
    }

    /// Address of `seq[idx]` for a list/tuple and an in-range int, else jump to `no`.
    fn elem_addr(&mut self, obj: SV, idx: SV, lists_only: bool, no: Block) -> V {
        let (to, po) = self.dynv(obj);
        let (ti, pi) = self.dynv(idx);
        let b1 = self.b.create_block();
        let is_obj = self.tag_is(to, tag::OBJ);
        let is_int = self.tag_is(ti, tag::INT);
        let both = self.b.ins().band(is_obj, is_int);
        self.b.ins().brif(both, b1, &[], no, &[]);
        self.b.switch_to_block(b1);
        let (len, items) = self.seq_parts(po, lists_only, no);
        let inb = self.b.ins().icmp(IntCC::UnsignedLessThan, pi, len);
        let ok = self.b.create_block();
        self.b.ins().brif(inb, ok, &[], no, &[]);
        self.b.switch_to_block(ok);
        let off = self.b.ins().ishl_imm_s(pi, 4);
        self.b.ins().iadd(items, off)
    }

    /// `obj[idx]`; `r`, `r+1` = the operands' slots.
    fn get_index(&mut self, obj: SV, idx: SV, r: usize) -> SV {
        let merge = self.b.create_block();
        let rt = self.b.append_block_param(merge, I64);
        let rp = self.b.append_block_param(merge, I64);
        let slow = self.b.create_block();
        self.b.set_cold_block(slow);
        let (obj, idx) = (self.dyn_sv(obj), self.dyn_sv(idx));
        let e = self.elem_addr(obj, idx, false, slow);
        let vt = self.ld(e, 0);
        let vp = self.ld(e, 8);
        self.b.ins().jump(merge, &[vt.into(), vp.into()]);

        self.b.switch_to_block(slow);
        self.spill();
        let (o0, o1) = (self.soff(r), self.soff(r + 1));
        self.write(obj, self.base, o0);
        self.write(idx, self.base, o1);
        self.set_ip();
        let sp = self.abs(r + 2);
        let (po, pi) = (self.slot_ptr(r), self.slot_ptr(r + 1));
        let st = self.call_rt(rt_getindex as *const () as usize, &[self.vm, sp, po, pi, po]);
        self.check(st);
        let t2 = self.ld(self.base, o0);
        let p2 = self.ld(self.base, o0 + 8);
        self.b.ins().jump(merge, &[t2.into(), p2.into()]);
        self.b.switch_to_block(merge);
        SV::D(rt, rp)
    }

    /// `obj[idx] = v` with [obj, idx, v] on top of the stack.
    fn set_index(&mut self, pre: &St) {
        let d = self.stack.len();
        let v = self.pop();
        let idx = self.pop();
        let obj = self.pop();
        let (obj, idx) = (self.dyn_sv(obj), self.dyn_sv(idx));
        let done = self.b.create_block();
        let slow = self.b.create_block();
        self.b.set_cold_block(slow);
        let e = self.elem_addr(obj, idx, true, slow);
        self.write(v, e, 0);
        self.b.ins().jump(done, &[]);

        self.b.switch_to_block(slow);
        // put the operands back where the interpreter's SetIndex expects them
        self.push(obj);
        self.push(idx);
        self.push(v);
        self.spill();
        self.set_ip();
        let sp = self.abs(d);
        let opp = self.ic(unsafe { self.ops_ptr.add(self.i) } as i64);
        let st = self.call_rt(rt_cold as *const () as usize, &[self.vm, sp, opp]);
        self.check(st);
        self.b.ins().jump(done, &[]);
        self.stack.truncate(d - 3);
        self.b.switch_to_block(done);
        let _ = pre;
    }

    /// Appends `v` to list `g`, in place when it has room (else through the
    /// runtime, with `v` at depth `r`); then jumps to `done`.
    fn list_push(&mut self, g: V, v: SV, r: usize, done: Block) {
        let len = self.ld(g, self.o.l.list_len);
        let cap = self.ld(g, self.o.l.list_cap);
        let room = self.b.ins().icmp(IntCC::UnsignedLessThan, len, cap);
        let fast = self.b.create_block();
        let slow = self.b.create_block();
        self.b.set_cold_block(slow);
        self.b.ins().brif(room, fast, &[], slow, &[]);
        self.b.switch_to_block(fast);
        let ptr = self.ld(g, self.o.l.list_ptr);
        let off = self.b.ins().ishl_imm_s(len, 4);
        let e = self.b.ins().iadd(ptr, off);
        self.write(v, e, 0);
        let len1 = self.b.ins().iadd_imm_s(len, 1);
        self.sto(len1, g, self.o.l.list_len);
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(slow);
        self.spill();
        let so = self.soff(r);
        self.write(v, self.base, so);
        let vp = self.slot_ptr(r);
        self.call_rt(rt_list_push as *const () as usize, &[self.vm, g, vp]);
        self.b.ins().jump(done, &[]);
    }

    /// A value as (tag, payload) registers (keeps the SV form for typed values).
    fn dyn_sv(&mut self, v: SV) -> SV {
        let (t, p) = self.dynv(v);
        SV::D(t, p)
    }

    /// Enters compiled function `fgc` (a Func box) directly when it takes exactly
    /// `nargs` arguments, which are already in memory starting at depth `first`;
    /// the result goes to depth `ret`. Falls back to `slow`; continues at `done`.
    #[allow(clippy::too_many_arguments)]
    fn direct_call(&mut self, owner: V, fgc: V, upvals: V, first: usize, ret: usize, nargs: usize, slow: Block, done: Block) {
        let fbox = self.ld(fgc, self.o.l.func_box);
        let info = self.ld(fbox, self.o.l.fn_jit);
        let b1 = self.b.create_block();
        self.b.ins().brif(info, b1, &[], slow, &[]);
        self.b.switch_to_block(b1);
        let fa = self.ld(info, self.o.info_fast_arity);
        let nfr = self.ld(self.vm, self.o.vm_frames_len);
        let need = self.ld(info, self.o.info_frame_need);
        let base_abs = self.abs(first);
        let top = self.b.ins().iadd(base_abs, need);
        let ok_arity = self.b.ins().icmp_imm_s(IntCC::Equal, fa, nargs as i64);
        let ok_depth = self.b.ins().icmp_imm_s(IntCC::UnsignedLessThan, nfr, MAX_FRAMES as i64);
        let ok_stack = self.b.ins().icmp_imm_s(IntCC::UnsignedLessThan, top, STACK_SLOTS as i64);
        let ok = self.b.ins().band(ok_arity, ok_depth);
        let ok = self.b.ins().band(ok, ok_stack);
        let b2 = self.b.create_block();
        self.b.ins().brif(ok, b2, &[], slow, &[]);
        self.b.switch_to_block(b2);
        // calls are GC safe points
        let depth = first + nargs;
        self.gc_check(depth);
        let frames = self.ld(self.vm, self.o.vm_frames_ptr);
        let nfr = self.ld(self.vm, self.o.vm_frames_len);
        let off = self.b.ins().imul_imm_s(nfr, self.o.frame_size);
        let fr = self.b.ins().iadd(frames, off);
        let code = self.ld(info, self.o.info_code);
        let consts = self.ld(info, self.o.info_consts);
        let globals = self.ld(info, self.o.info_globals);
        let ret_abs = self.abs(ret);
        self.sto(owner, fr, self.o.fr_owner);
        self.sto(code, fr, self.o.fr_code);
        self.sto(consts, fr, self.o.fr_consts);
        self.sto(code, fr, self.o.fr_ip);
        self.sto(base_abs, fr, self.o.fr_base);
        self.sto(ret_abs, fr, self.o.fr_ret);
        self.sto(upvals, fr, self.o.fr_upvals);
        self.sto(globals, fr, self.o.fr_globals);
        let n1 = self.b.ins().iadd_imm_s(nfr, 1);
        self.sto(n1, self.vm, self.o.vm_frames_len);
        let entry = self.ld(info, self.o.info_entry);
        let sig = self.entry_sig();
        let inst = self.b.ins().call_indirect(sig, entry, &[self.vm, fr]);
        let st = self.b.inst_results(inst)[0];
        self.b.ins().brif(st, self.catch, &[], done, &[]);
    }

    fn entry_sig(&mut self) -> SigRef {
        // same shape as a two-argument runtime helper
        match self.sigs.get(&2) {
            Some(&s) => s,
            None => {
                let mut s = Signature::new(self.cc);
                s.params.push(AbiParam::new(I64));
                s.params.push(AbiParam::new(I64));
                s.returns.push(AbiParam::new(I64));
                let r = self.b.import_signature(s);
                self.sigs.insert(2, r);
                r
            }
        }
    }

    /// Dispatches a callable (t, p) to `direct_call` for functions and closures.
    fn call_callable(&mut self, t: V, p: V, first: usize, ret: usize, nargs: usize, slow: Block, done: Block) {
        let b1 = self.b.create_block();
        let fb = self.b.create_block();
        let not_f = self.b.create_block();
        let cb = self.b.create_block();
        let is_obj = self.tag_is(t, tag::OBJ);
        self.b.ins().brif(is_obj, b1, &[], slow, &[]);
        self.b.switch_to_block(b1);
        let k = self.b.ins().uload8(I32, flags(), p, self.o.l.kind);
        let is_f = self.b.ins().icmp_imm_s(IntCC::Equal, k, kind::FUNC as i64);
        self.b.ins().brif(is_f, fb, &[], not_f, &[]);
        self.b.switch_to_block(fb);
        let zero = self.ic(0);
        self.direct_call(p, p, zero, first, ret, nargs, slow, done);
        self.b.switch_to_block(not_f);
        let not_c = self.b.create_block();
        let is_c = self.b.ins().icmp_imm_s(IntCC::Equal, k, kind::CLOSURE as i64);
        self.b.ins().brif(is_c, cb, &[], not_c, &[]);
        self.b.switch_to_block(cb);
        let fgc = self.ld(p, self.o.l.closure_func);
        let upv = self.ld(p, self.o.l.closure_upvals_ptr);
        self.direct_call(p, fgc, upv, first, ret, nargs, slow, done);
        self.b.switch_to_block(not_c);
        if first == ret + 1 {
            // Class(args): a new instance in the callee's slot, then its init(self, args)
            let clsb = self.b.create_block();
            let initb = self.b.create_block();
            let is_cls = self.b.ins().icmp_imm_s(IntCC::Equal, k, kind::CLASS as i64);
            self.b.ins().brif(is_cls, clsb, &[], slow, &[]);
            self.b.switch_to_block(clsb);
            let cbox = self.ld(p, self.o.l.class_box);
            let init = self.ld(cbox, self.o.l.class_init);
            self.b.ins().brif(init, initb, &[], slow, &[]);
            self.b.switch_to_block(initb);
            let slot = self.slot_ptr(ret);
            self.call_rt(rt_new_instance as *const () as usize, &[self.vm, p, slot]);
            let ik = self.b.ins().uload8(I32, flags(), init, self.o.l.kind);
            let fb2 = self.b.create_block();
            let is_f = self.b.ins().icmp_imm_s(IntCC::Equal, ik, kind::FUNC as i64);
            // (a closure init, or anything unusual: the runtime, with the class still in place)
            let undo = self.b.create_block();
            self.b.ins().brif(is_f, fb2, &[], undo, &[]);
            self.b.switch_to_block(fb2);
            let zero = self.ic(0);
            self.direct_call(init, init, zero, ret, ret, nargs + 1, undo, done);
            self.b.switch_to_block(undo);
            let off = self.soff(ret);
            self.write(SV::D(t, p), self.base, off);
            self.b.ins().jump(slow, &[]);
        } else {
            self.b.ins().jump(slow, &[]);
        }
    }

    /// `Call(n)`: callee at depth d-n-1, arguments above it.
    fn call(&mut self, n: usize, d: usize) {
        self.flush();
        self.set_ip();
        let c = d - n - 1;
        let off = self.soff(c);
        let t = self.ld(self.base, off);
        let p = self.ld(self.base, off + 8);
        let slow = self.b.create_block();
        let done = self.b.create_block();
        self.b.set_cold_block(slow);
        self.call_callable(t, p, c + 1, c, n, slow, done);

        self.b.switch_to_block(slow);
        let callee = self.abs(c);
        let nv = self.ic(n as i64);
        let st = self.call_rt(rt_call as *const () as usize, &[self.vm, callee, nv]);
        self.b.ins().brif(st, self.catch, &[], done, &[]);

        self.b.switch_to_block(done);
        self.stack.truncate(c + 1);
        self.stack[c] = SV::Mem(Ty::Any);
    }

    /// `Invoke(name, n)`: receiver at depth d-n-1, arguments above it.
    fn invoke(&mut self, name: u32, n: usize, d: usize) {
        self.flush();
        self.set_ip();
        let r = d - n - 1;
        let off = self.soff(r);
        let t = self.ld(self.base, off);
        let p = self.ld(self.base, off + 8);
        let slow = self.b.create_block();
        let done = self.b.create_block();
        self.b.set_cold_block(slow);
        if name == sym::APPEND && n == 1 {
            // list.append(v)
            let b1 = self.b.create_block();
            let lb = self.b.create_block();
            let no = self.b.create_block();
            let is_obj = self.tag_is(t, tag::OBJ);
            self.b.ins().brif(is_obj, b1, &[], no, &[]);
            self.b.switch_to_block(b1);
            let k = self.b.ins().uload8(I32, flags(), p, self.o.l.kind);
            let is_l = self.b.ins().icmp_imm_s(IntCC::Equal, k, kind::LIST as i64);
            self.b.ins().brif(is_l, lb, &[], no, &[]);
            self.b.switch_to_block(lb);
            let none = self.ic(tag::NONE);
            self.sto(none, self.base, off);
            let v = self.load_val(Ty::Any, self.base, off + 16);
            self.list_push(p, v, r + 1, done);
            self.b.switch_to_block(no);
        }
        let cls = self.shape_check(t, p, slow);
        let mptr = self.ld(cls, self.o.l.class_methods_ptr);
        let cell = self.cell();
        let midx = self.b.ins().uload32(flags(), cell, 4);
        let moff = self.b.ins().ishl_imm_s(midx, 4);
        let m = self.b.ins().iadd(mptr, moff);
        let mt = self.ld(m, 0);
        let mp = self.ld(m, 8);
        self.call_callable(mt, mp, r, r, n + 1, slow, done);

        self.b.switch_to_block(slow);
        let recv = self.abs(r);
        let nm = self.ic(name as i64);
        let nv = self.ic(n as i64);
        let cell = self.cell();
        let st = self.call_rt(rt_invoke as *const () as usize, &[self.vm, recv, nm, nv, cell]);
        self.b.ins().brif(st, self.catch, &[], done, &[]);

        self.b.switch_to_block(done);
        self.stack.truncate(r + 1);
        self.stack[r] = SV::Mem(Ty::Any);
    }
}
