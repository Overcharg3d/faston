//! Bytecode instruction set. Instructions are a 16-byte `Copy` enum with
//! pre-decoded operands, so dispatch is a single `match`.
//!
//! Some instructions are "superinstructions" produced by the compiler's
//! peephole pass (e.g. `GetLocal2`, `JumpIfNotLt`, `AddLocal`) to cut down
//! the number of dispatches in hot loops.

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op {
    // constants
    Const(u32),
    None,
    True,
    False,

    // stack
    Pop,
    Dup,
    Dup2,
    /// [a, b] -> [b, a]
    Swap,
    /// [a, b, c] -> [c, a, b]
    Rot3,

    // variables
    GetLocal(u32),
    GetLocal2(u32, u32),
    SetLocal(u32),
    /// locals[slot] += pop()
    AddLocal(u32),
    /// locals[dst] += locals[src]
    AddLocalL(u32, u32),
    /// locals[dst] += consts[k]
    AddLocalC(u32, u32),
    GetGlobal(u32),
    SetGlobal(u32),
    /// Captured variables of the running closure.
    GetUpval(u32),
    SetUpval(u32),
    /// Close (move to the heap) captured locals in slots >= operand.
    CloseUpvals(u32),
    /// Build a closure from function constant k, capturing per its `captures`.
    Closure(u32),
    /// Like `Closure`, also popping n default-argument values: (k, n).
    ClosureDefaults(u32, u32),

    // attributes (operand = interned symbol) / indexing
    GetAttr(u32),
    SetAttr(u32),
    /// push locals[a].name : (a, name)
    GetLocalAttr(u32, u32),
    // Inline-cached forms, rewritten in place at runtime after a successful lookup:
    // (name, class_shape_id, field_slot)
    GetAttrIC(u32, u32, u32),
    SetAttrIC(u32, u32, u32),
    /// (name, class_shape_id, local | field_slot << 16)
    GetLocalAttrIC(u32, u32, u32),
    /// locals[a].name = pop() : (a, name)
    SetLocalAttr(u32, u32),
    /// (name, class_shape_id, local | field_slot << 16)
    SetLocalAttrIC(u32, u32, u32),
    GetIndex,
    SetIndex,

    // arithmetic
    Add,
    Sub,
    Mul,
    Div,
    FloorDiv,
    Mod,
    Pow,
    Neg,
    Not,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Invert,
    // arithmetic with a constant right operand: top = top OP consts[k]
    AddC(u32),
    SubC(u32),
    MulC(u32),
    ModC(u32),
    // push locals[a] OP consts[k]
    AddLC(u32, u32),
    SubLC(u32, u32),
    MulLC(u32, u32),
    ModLC(u32, u32),
    // top = top OP locals[a]
    AddL(u32),
    SubL(u32),
    MulL(u32),
    // push locals[a] OP locals[b]
    AddLL(u32, u32),
    SubLL(u32, u32),
    MulLL(u32, u32),

    // comparison
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    In,
    // compare with a constant right operand
    EqC(u32),
    NeC(u32),
    LtC(u32),
    LeC(u32),
    GtC(u32),
    GeC(u32),

    // control flow (absolute targets)
    Jump(u32),
    JumpIfFalse(u32),
    /// Pop; jump if truthy (produced by jump threading of `or` in conditions).
    JumpIfTrue(u32),
    JumpIfFalseOrPop(u32),
    JumpIfTrueOrPop(u32),
    // fused compare + JumpIfFalse
    JumpIfNotEq(u32),
    JumpIfNotNe(u32),
    JumpIfNotLt(u32),
    JumpIfNotLe(u32),
    JumpIfNotGt(u32),
    JumpIfNotGe(u32),
    // compare top with consts[k], jump to t if false: (k, t)
    JumpIfNotEqC(u32, u32),
    JumpIfNotNeC(u32, u32),
    JumpIfNotLtC(u32, u32),
    JumpIfNotLeC(u32, u32),
    JumpIfNotGtC(u32, u32),
    JumpIfNotGeC(u32, u32),
    // compare top with consts[k], jump to t if TRUE: (k, t)
    JumpIfEqC(u32, u32),
    JumpIfNeC(u32, u32),
    JumpIfLtC(u32, u32),
    JumpIfLeC(u32, u32),
    JumpIfGtC(u32, u32),
    JumpIfGeC(u32, u32),
    // compare locals[a] with consts[k]: (a, k, t)
    JumpIfNotEqLC(u32, u32, u32),
    JumpIfNotNeLC(u32, u32, u32),
    JumpIfNotLtLC(u32, u32, u32),
    JumpIfNotLeLC(u32, u32, u32),
    JumpIfNotGtLC(u32, u32, u32),
    JumpIfNotGeLC(u32, u32, u32),
    // compare locals[a] with locals[b]: (a, b, t)
    JumpIfNotEqLL(u32, u32, u32),
    JumpIfNotNeLL(u32, u32, u32),
    JumpIfNotLtLL(u32, u32, u32),
    JumpIfNotLeLL(u32, u32, u32),
    JumpIfNotGtLL(u32, u32, u32),
    JumpIfNotGeLL(u32, u32, u32),

    /// Pops an iterable and sets up loop state in locals[slot..slot+3].
    GetIter(u32),
    /// ForIter(state_slot, loop_var_slot, exit_target)
    ForIter(u32, u32, u32),
    /// Bottom of an inverted for loop: if there is a next item, store it and jump
    /// back to the body. ForLoop(state_slot, loop_var_slot, body_target)
    ForLoop(u32, u32, u32),

    // calls
    Call(u32),
    /// CallKw(argc, k): the last len(consts[k]) of the argc arguments are keyword
    /// arguments named by the symbols in tuple constant k.
    CallKw(u32, u32),
    /// [callee, list, dict] -> result
    CallEx,
    /// InvokeKw(name, argc, k): obj.name(..., key=value)
    InvokeKw(u32, u32, u32),
    /// Argument building for CallEx: [list, v] -> [list], [list, xs] -> [list],
    /// [dict, k, v] -> [dict], [dict, d] -> [dict]
    ArgPush,
    ArgExtend,
    ArgKw,
    ArgMerge,
    /// Invoke(method_symbol, argc): obj.method(args) without a bound-method allocation
    Invoke(u32, u32),
    /// (name, class_shape_id, argc | method_index << 8)
    InvokeIC(u32, u32, u32),
    /// `x.append(v)` specialised after seeing a list receiver (name kept for fallback).
    InvokeAppend(u32),
    Return,

    // collections
    BuildList(u32),
    BuildDict(u32),
    BuildTuple(u32),
    BuildSet(u32),
    ListAppend(u32),
    /// comprehension helpers: locals[slot].add(pop()) / locals[slot][k] = v
    SetAdd(u32),
    DictSet(u32),
    /// Pops a sequence and pushes its n items (errors on length mismatch).
    Unpack(u32),
    /// obj[start:stop:step] (missing parts are None)
    GetSlice,
    /// Pushes the built-in module named by the symbol.
    Import(u32),

    // exceptions
    /// Push a handler: on an exception, unwind to here and jump to the target.
    SetupTry(u32),
    PopTry,
    /// Raise the exception (or exception class) on top of the stack.
    Raise,
    /// [exc, T1..Tn] -> [bool]: is exc an instance of any Ti?
    ExcMatch(u32),

    // classes (operands = symbols)
    Class(u32),
    Inherit,
    Method(u32),
}

impl Op {
    /// Net effect on the operand stack depth (used to size each frame's stack).
    pub fn stack_effect(self) -> i32 {
        match self {
            Op::Const(_) | Op::None | Op::True | Op::False | Op::Dup | Op::GetLocal(_) => 1,
            Op::GetGlobal(_) | Op::Class(_) | Op::GetUpval(_) | Op::Closure(_) => 1,
            Op::SetUpval(_) => -1,
            Op::CloseUpvals(_) => 0,
            Op::ClosureDefaults(_, n) => 1 - n as i32,
            Op::GetLocal2(..) | Op::Dup2 => 2,
            Op::Pop | Op::SetLocal(_) | Op::SetGlobal(_) | Op::AddLocal(_) => -1,
            Op::GetAttr(_) | Op::Neg | Op::Not | Op::Jump(_) | Op::ForIter(..) => 0,
            Op::SetAttr(_) => -2,
            Op::Swap | Op::Rot3 | Op::Invert => 0,
            Op::BitAnd | Op::BitOr | Op::BitXor | Op::Shl | Op::Shr => -1,
            Op::GetIndex => -1,
            Op::SetIndex => -3,
            Op::Add | Op::Sub | Op::Mul | Op::Div | Op::FloorDiv | Op::Mod | Op::Pow => -1,
            Op::Eq | Op::Ne | Op::Lt | Op::Le | Op::Gt | Op::Ge | Op::In => -1,
            Op::JumpIfFalse(_) | Op::JumpIfFalseOrPop(_) | Op::JumpIfTrueOrPop(_) => -1,
            Op::JumpIfNotEq(_) | Op::JumpIfNotNe(_) | Op::JumpIfNotLt(_) => -2,
            Op::JumpIfNotLe(_) | Op::JumpIfNotGt(_) | Op::JumpIfNotGe(_) => -2,
            Op::GetIter(_) => -1,
            Op::Call(n) | Op::Invoke(_, n) | Op::CallKw(n, _) | Op::InvokeKw(_, n, _) => -(n as i32),
            Op::CallEx => -2,
            Op::ArgPush | Op::ArgExtend | Op::ArgMerge => -1,
            Op::ArgKw => -2,
            Op::Return => -1,
            Op::BuildList(n) | Op::BuildTuple(n) | Op::BuildSet(n) => 1 - n as i32,
            Op::BuildDict(n) => 1 - 2 * n as i32,
            Op::ListAppend(_) | Op::Inherit | Op::Method(_) | Op::SetAdd(_) => -1,
            Op::DictSet(_) => -2,
            Op::Unpack(n) => n as i32 - 1,
            Op::GetSlice => -3,
            Op::Import(_) => 1,
            Op::SetupTry(_) | Op::PopTry => 0,
            Op::Raise => -1,
            Op::ExcMatch(n) => -(n as i32),
            Op::AddLocalL(..) | Op::AddLocalC(..) => 0,
            Op::GetAttrIC(..) => 0,
            Op::SetAttrIC(..) => -2,
            Op::GetLocalAttr(..) | Op::GetLocalAttrIC(..) => 1,
            Op::SetLocalAttr(..) | Op::SetLocalAttrIC(..) => -1,
            Op::InvokeIC(_, _, packed) => -((packed & 0xff) as i32),
            Op::InvokeAppend(_) => -1,
            Op::AddC(_) | Op::SubC(_) | Op::MulC(_) | Op::ModC(_) => 0,
            Op::AddLC(..) | Op::SubLC(..) | Op::MulLC(..) | Op::ModLC(..) => 1,
            Op::AddL(_) | Op::SubL(_) | Op::MulL(_) => 0,
            Op::AddLL(..) | Op::SubLL(..) | Op::MulLL(..) => 1,
            Op::JumpIfTrue(_) => -1,
            Op::JumpIfEqC(..) | Op::JumpIfNeC(..) | Op::JumpIfLtC(..) => -1,
            Op::JumpIfLeC(..) | Op::JumpIfGtC(..) | Op::JumpIfGeC(..) => -1,
            Op::ForLoop(..) => 0,
            Op::EqC(_) | Op::NeC(_) | Op::LtC(_) | Op::LeC(_) | Op::GtC(_) | Op::GeC(_) => 0,
            Op::JumpIfNotEqC(..) | Op::JumpIfNotNeC(..) | Op::JumpIfNotLtC(..) => -1,
            Op::JumpIfNotLeC(..) | Op::JumpIfNotGtC(..) | Op::JumpIfNotGeC(..) => -1,
            Op::JumpIfNotEqLC(..) | Op::JumpIfNotNeLC(..) | Op::JumpIfNotLtLC(..) => 0,
            Op::JumpIfNotLeLC(..) | Op::JumpIfNotGtLC(..) | Op::JumpIfNotGeLC(..) => 0,
            Op::JumpIfNotEqLL(..) | Op::JumpIfNotNeLL(..) | Op::JumpIfNotLtLL(..) => 0,
            Op::JumpIfNotLeLL(..) | Op::JumpIfNotGtLL(..) | Op::JumpIfNotGeLL(..) => 0,
        }
    }

    /// Peephole: fuse two adjacent instructions into one superinstruction.
    pub fn fuse(prev: Op, next: Op) -> Option<Op> {
        use Op::*;
        Some(match (prev, next) {
            (GetLocal(a), GetLocal(b)) => GetLocal2(a, b),
            (GetLocal(a), GetAttr(n)) => GetLocalAttr(a, n),
            (GetLocal(a), AddLocal(b)) => AddLocalL(b, a),
            (Const(k), AddLocal(b)) => AddLocalC(b, k),

            (Const(k), Add) => AddC(k),
            (Const(k), Sub) => SubC(k),
            (Const(k), Mul) => MulC(k),
            (Const(k), Mod) => ModC(k),
            (GetLocal(a), AddC(k)) => AddLC(a, k),
            (GetLocal(a), SubC(k)) => SubLC(a, k),
            (GetLocal(a), MulC(k)) => MulLC(a, k),
            (GetLocal(a), Add) => AddL(a),
            (GetLocal(a), Sub) => SubL(a),
            (GetLocal(a), Mul) => MulL(a),
            (GetLocal2(a, b), Add) => AddLL(a, b),
            (GetLocal2(a, b), Sub) => SubLL(a, b),
            (GetLocal2(a, b), Mul) => MulLL(a, b),
            (GetLocal(a), ModC(k)) => ModLC(a, k),

            (Const(k), Eq) => EqC(k),
            (Const(k), Ne) => NeC(k),
            (Const(k), Lt) => LtC(k),
            (Const(k), Le) => LeC(k),
            (Const(k), Gt) => GtC(k),
            (Const(k), Ge) => GeC(k),

            (Eq, JumpIfFalse(t)) => JumpIfNotEq(t),
            (Ne, JumpIfFalse(t)) => JumpIfNotNe(t),
            (Lt, JumpIfFalse(t)) => JumpIfNotLt(t),
            (Le, JumpIfFalse(t)) => JumpIfNotLe(t),
            (Gt, JumpIfFalse(t)) => JumpIfNotGt(t),
            (Ge, JumpIfFalse(t)) => JumpIfNotGe(t),

            (EqC(k), JumpIfTrue(t)) => JumpIfEqC(k, t),
            (NeC(k), JumpIfTrue(t)) => JumpIfNeC(k, t),
            (LtC(k), JumpIfTrue(t)) => JumpIfLtC(k, t),
            (LeC(k), JumpIfTrue(t)) => JumpIfLeC(k, t),
            (GtC(k), JumpIfTrue(t)) => JumpIfGtC(k, t),
            (GeC(k), JumpIfTrue(t)) => JumpIfGeC(k, t),
            (EqC(k), JumpIfFalse(t)) => JumpIfNotEqC(k, t),
            (NeC(k), JumpIfFalse(t)) => JumpIfNotNeC(k, t),
            (LtC(k), JumpIfFalse(t)) => JumpIfNotLtC(k, t),
            (LeC(k), JumpIfFalse(t)) => JumpIfNotLeC(k, t),
            (GtC(k), JumpIfFalse(t)) => JumpIfNotGtC(k, t),
            (GeC(k), JumpIfFalse(t)) => JumpIfNotGeC(k, t),

            (GetLocal(a), JumpIfNotEqC(k, t)) => JumpIfNotEqLC(a, k, t),
            (GetLocal(a), JumpIfNotNeC(k, t)) => JumpIfNotNeLC(a, k, t),
            (GetLocal(a), JumpIfNotLtC(k, t)) => JumpIfNotLtLC(a, k, t),
            (GetLocal(a), JumpIfNotLeC(k, t)) => JumpIfNotLeLC(a, k, t),
            (GetLocal(a), JumpIfNotGtC(k, t)) => JumpIfNotGtLC(a, k, t),
            (GetLocal(a), JumpIfNotGeC(k, t)) => JumpIfNotGeLC(a, k, t),

            (GetLocal2(a, b), JumpIfNotEq(t)) => JumpIfNotEqLL(a, b, t),
            (GetLocal2(a, b), JumpIfNotNe(t)) => JumpIfNotNeLL(a, b, t),
            (GetLocal2(a, b), JumpIfNotLt(t)) => JumpIfNotLtLL(a, b, t),
            (GetLocal2(a, b), JumpIfNotLe(t)) => JumpIfNotLeLL(a, b, t),
            (GetLocal2(a, b), JumpIfNotGt(t)) => JumpIfNotGtLL(a, b, t),
            (GetLocal2(a, b), JumpIfNotGe(t)) => JumpIfNotGeLL(a, b, t),
            _ => return Option::None,
        })
    }

    /// The code index this instruction can jump to, if any.
    pub fn jump_target(self) -> Option<u32> {
        use Op::*;
        Some(match self {
            Jump(t) | JumpIfFalse(t) | JumpIfTrue(t) | JumpIfFalseOrPop(t) | JumpIfTrueOrPop(t) => t,
            JumpIfNotEq(t) | JumpIfNotNe(t) | JumpIfNotLt(t) | JumpIfNotLe(t) | JumpIfNotGt(t)
            | JumpIfNotGe(t) => t,
            JumpIfNotEqC(_, t) | JumpIfNotNeC(_, t) | JumpIfNotLtC(_, t) | JumpIfNotLeC(_, t)
            | JumpIfNotGtC(_, t) | JumpIfNotGeC(_, t) => t,
            JumpIfEqC(_, t) | JumpIfNeC(_, t) | JumpIfLtC(_, t) | JumpIfLeC(_, t) | JumpIfGtC(_, t)
            | JumpIfGeC(_, t) => t,
            JumpIfNotEqLC(_, _, t) | JumpIfNotNeLC(_, _, t) | JumpIfNotLtLC(_, _, t)
            | JumpIfNotLeLC(_, _, t) | JumpIfNotGtLC(_, _, t) | JumpIfNotGeLC(_, _, t) => t,
            JumpIfNotEqLL(_, _, t) | JumpIfNotNeLL(_, _, t) | JumpIfNotLtLL(_, _, t)
            | JumpIfNotLeLL(_, _, t) | JumpIfNotGtLL(_, _, t) | JumpIfNotGeLL(_, _, t) => t,
            ForIter(_, _, t) | ForLoop(_, _, t) | SetupTry(t) => t,
            _ => return Option::None,
        })
    }

    /// The same instruction with its jump target replaced (non-jumps unchanged).
    pub fn with_target(self, t: u32) -> Op {
        use Op::*;
        match self {
            Jump(_) => Jump(t),
            JumpIfFalse(_) => JumpIfFalse(t),
            JumpIfTrue(_) => JumpIfTrue(t),
            JumpIfFalseOrPop(_) => JumpIfFalseOrPop(t),
            JumpIfTrueOrPop(_) => JumpIfTrueOrPop(t),
            JumpIfNotEq(_) => JumpIfNotEq(t),
            JumpIfNotNe(_) => JumpIfNotNe(t),
            JumpIfNotLt(_) => JumpIfNotLt(t),
            JumpIfNotLe(_) => JumpIfNotLe(t),
            JumpIfNotGt(_) => JumpIfNotGt(t),
            JumpIfNotGe(_) => JumpIfNotGe(t),
            JumpIfEqC(k, _) => JumpIfEqC(k, t),
            JumpIfNeC(k, _) => JumpIfNeC(k, t),
            JumpIfLtC(k, _) => JumpIfLtC(k, t),
            JumpIfLeC(k, _) => JumpIfLeC(k, t),
            JumpIfGtC(k, _) => JumpIfGtC(k, t),
            JumpIfGeC(k, _) => JumpIfGeC(k, t),
            JumpIfNotEqC(k, _) => JumpIfNotEqC(k, t),
            JumpIfNotNeC(k, _) => JumpIfNotNeC(k, t),
            JumpIfNotLtC(k, _) => JumpIfNotLtC(k, t),
            JumpIfNotLeC(k, _) => JumpIfNotLeC(k, t),
            JumpIfNotGtC(k, _) => JumpIfNotGtC(k, t),
            JumpIfNotGeC(k, _) => JumpIfNotGeC(k, t),
            JumpIfNotEqLC(a, k, _) => JumpIfNotEqLC(a, k, t),
            JumpIfNotNeLC(a, k, _) => JumpIfNotNeLC(a, k, t),
            JumpIfNotLtLC(a, k, _) => JumpIfNotLtLC(a, k, t),
            JumpIfNotLeLC(a, k, _) => JumpIfNotLeLC(a, k, t),
            JumpIfNotGtLC(a, k, _) => JumpIfNotGtLC(a, k, t),
            JumpIfNotGeLC(a, k, _) => JumpIfNotGeLC(a, k, t),
            JumpIfNotEqLL(a, b, _) => JumpIfNotEqLL(a, b, t),
            JumpIfNotNeLL(a, b, _) => JumpIfNotNeLL(a, b, t),
            JumpIfNotLtLL(a, b, _) => JumpIfNotLtLL(a, b, t),
            JumpIfNotLeLL(a, b, _) => JumpIfNotLeLL(a, b, t),
            JumpIfNotGtLL(a, b, _) => JumpIfNotGtLL(a, b, t),
            JumpIfNotGeLL(a, b, _) => JumpIfNotGeLL(a, b, t),
            ForIter(a, b, _) => ForIter(a, b, t),
            ForLoop(a, b, _) => ForLoop(a, b, t),
            SetupTry(_) => SetupTry(t),
            other => other,
        }
    }
}
