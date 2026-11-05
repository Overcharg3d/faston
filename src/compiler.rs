//! Single-pass compiler: tokens -> bytecode, no AST.
//!
//! Scoping rules:
//! * Top level of a script: `let`/`const`/assignment/`def`/`class` create globals.
//! * Blocks (`if`, `while`, `for`, ...) are block-scoped: `let` inside them is local.
//! * Inside a function, plain assignment to an unknown name creates a local
//!   (like Python). Use `global name;` / `nonlocal name;` to assign to a global or
//!   to a variable of an enclosing function instead.
//! * Locals live in fixed per-frame slots, so they are as fast as an array index.
//! * Nested functions capture enclosing locals as upvalues (Lua-style): a captured
//!   variable stays on the stack while its scope is alive and is moved to the heap
//!   ("closed") when the scope ends, so closures share variables, like Python.

use std::collections::HashSet;

use crate::bytecode::Op;
use crate::lexer::{Tok, Token};
use crate::value::{sym, Function, Gc, Globals, Heap, Interner, Obj, Value};

struct Local {
    name: String,
    depth: u32,
    is_const: bool,
    /// Some nested function captures this variable.
    captured: bool,
}

struct UpvalDesc {
    from_local: bool,
    index: u32,
}

struct LoopCtx {
    continue_target: usize,
    breaks: Vec<usize>,
    continues: Vec<usize>,
    /// First local slot declared inside the loop body.
    boundary: usize,
    /// A body local was captured, so `break`/`continue` must close upvalues.
    captured: bool,
    /// Number of active exception handlers when the loop started.
    active_depth: usize,
}

/// What leaving an active `SetupTry` region early (return/break/continue) must do.
#[derive(Clone, Copy)]
enum Cleanup {
    /// An `except` handler: just pop it.
    Handler,
    /// A `finally` guard: pop it and run the block at this token range.
    Finally((usize, usize)),
    /// A `with` guard: pop it and call `__exit__(None, None, None)` on the manager in this local.
    With(u32),
}

struct FnState {
    name: String,
    code: Vec<Op>,
    lines: Vec<u32>,
    consts: Vec<Value>,
    locals: Vec<Local>,
    depth: u32,
    max_slots: usize,
    cur_stack: i32,
    max_stack: i32,
    /// Code index that some jump targets; the peephole pass never fuses across it.
    last_label: usize,
    is_script: bool,
    is_init: bool,
    loops: Vec<LoopCtx>,
    global_decls: HashSet<String>,
    nonlocal_decls: HashSet<String>,
    upvalues: Vec<UpvalDesc>,
    /// Compile-time mirror of the VM's handler stack inside this function: each entry
    /// is a `SetupTry` that is active here, with the token range of its `finally` block
    /// (if it is a finally guard). `return`/`break`/`continue` pop them and run finallys.
    active: Vec<Cleanup>,
    /// Hidden locals holding the exception being handled (for bare `raise;`).
    exc_slots: Vec<u32>,
    /// Parameter names (symbols) and `*args` / `**kwargs` flags, set by `params`.
    param_syms: Vec<u32>,
    varargs: bool,
    varkw: bool,
    /// The body contains `yield`: calling the function makes a generator.
    is_generator: bool,
    /// A method of a class (so `super()` works); the token range of the class's
    /// base-class expression, if it has one.
    is_method: bool,
    class_base: Option<(usize, usize)>,
}

impl FnState {
    fn new(name: String, is_script: bool, is_init: bool) -> Self {
        FnState {
            name,
            code: Vec::new(),
            lines: Vec::new(),
            consts: Vec::new(),
            locals: Vec::new(),
            depth: 0,
            max_slots: 0,
            cur_stack: 0,
            max_stack: 0,
            last_label: usize::MAX,
            is_script,
            is_init,
            loops: Vec::new(),
            global_decls: HashSet::new(),
            nonlocal_decls: HashSet::new(),
            upvalues: Vec::new(),
            active: Vec::new(),
            exc_slots: Vec::new(),
            param_syms: Vec::new(),
            varargs: false,
            varkw: false,
            is_generator: false,
            is_method: false,
            class_base: None,
        }
    }

    /// Jump threading for `and` / `or` used as conditions: a short-circuit jump that
    /// lands on a conditional jump testing the same value can go straight to where
    /// that test would send it.
    fn thread_jumps(&mut self) {
        for _ in 0..4 {
            let mut changed = false;
            for i in 0..self.code.len() {
                let new = match self.code[i] {
                    Op::JumpIfTrueOrPop(t) => match self.code.get(t as usize) {
                        // value is truthy there, so the JumpIfFalse falls through (popping)
                        Some(Op::JumpIfFalse(_)) => Some(Op::JumpIfTrue(t + 1)),
                        Some(&Op::JumpIfTrueOrPop(t2)) => Some(Op::JumpIfTrueOrPop(t2)),
                        _ => None,
                    },
                    Op::JumpIfFalseOrPop(t) => match self.code.get(t as usize) {
                        // value is falsy there, so the JumpIfFalse jumps (popping)
                        Some(&Op::JumpIfFalse(t2)) => Some(Op::JumpIfFalse(t2)),
                        Some(&Op::JumpIfFalseOrPop(t2)) => Some(Op::JumpIfFalseOrPop(t2)),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(op) = new {
                    self.code[i] = op;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    /// After threading, some instructions are no longer jump targets, so pairs that
    /// couldn't be fused during emission can be now. Fuses them and compacts the code,
    /// remapping every jump.
    fn refuse(&mut self) {
        let n = self.code.len();
        let mut is_target = vec![false; n + 1];
        for op in &self.code {
            if let Some(t) = op.jump_target() {
                is_target[t as usize] = true;
            }
        }
        let mut keep = vec![true; n];
        let mut i = 0;
        while i + 1 < n {
            if !is_target[i + 1] {
                if let Some(f) = Op::fuse(self.code[i], self.code[i + 1]) {
                    self.code[i] = f;
                    keep[i + 1] = false;
                    i += 2;
                    continue;
                }
            }
            i += 1;
        }
        if keep.iter().all(|&k| k) {
            return;
        }
        // old index -> new index (a deleted instruction maps to its fused predecessor's
        // successor, which is never a target, so any mapping is fine for it)
        let mut map = vec![0u32; n + 1];
        let mut j = 0u32;
        for i in 0..n {
            map[i] = j;
            if keep[i] {
                j += 1;
            }
        }
        map[n] = j;
        let mut code = Vec::with_capacity(j as usize);
        let mut lines = Vec::with_capacity(j as usize);
        for i in 0..n {
            if keep[i] {
                let op = self.code[i];
                code.push(match op.jump_target() {
                    Some(t) => op.with_target(map[t as usize]),
                    None => op,
                });
                lines.push(self.lines[i]);
            }
        }
        self.code = code;
        self.lines = lines;
    }

    fn finish(mut self, arity: u32, defaults: Vec<Value>, unit: u32) -> Function {
        self.thread_jumps();
        self.refuse();
        Function {
            name: self.name,
            arity,
            defaults,
            code: self.code,
            lines: self.lines,
            consts: self.consts,
            num_locals: self.max_slots as u32,
            max_stack: self.max_stack.max(0) as u32 + 4,
            captures: self.upvalues.iter().map(|u| (u.from_local, u.index)).collect(),
            unit,
            dynamic_defaults: false,
            fast_arity: if self.varargs || self.varkw { u32::MAX } else { arity },
            params: self.param_syms,
            varargs: self.varargs,
            varkw: self.varkw,
            jit: std::ptr::null(),
            native_state: 0,
        }
    }
}

type Res<T> = Result<T, String>;

#[derive(Clone, Copy)]
enum Comp {
    List,
    Set,
    Dict,
}

enum CallArgs {
    /// n positional arguments
    Plain(u32),
    /// n arguments in total, the last ones named by the tuple constant k
    Keywords(u32, u32),
    /// list + dict built for `CallEx`
    Splat,
}

/// `=` -> Some(None); `+=` etc -> Some(Some(op)); anything else -> None
fn assign_kind(t: &Tok) -> Option<Option<Op>> {
    match t {
        Tok::Assign => Some(None),
        Tok::PlusEq => Some(Some(Op::Add)),
        Tok::MinusEq => Some(Some(Op::Sub)),
        Tok::StarEq => Some(Some(Op::Mul)),
        Tok::SlashEq => Some(Some(Op::Div)),
        Tok::PercentEq => Some(Some(Op::Mod)),
        Tok::AmpEq => Some(Some(Op::BitAnd)),
        Tok::PipeEq => Some(Some(Op::BitOr)),
        Tok::CaretEq => Some(Some(Op::BitXor)),
        _ => None,
    }
}

pub struct Compiler<'g> {
    toks: Vec<Token>,
    pos: usize,
    fns: Vec<FnState>,
    globals: &'g mut Globals,
    heap: &'g mut Heap,
    syms: &'g mut Interner,
    unit: u32,
    /// Token ranges of default-argument expressions of the function just compiled,
    /// evaluated in the enclosing scope when the `def` runs (see `emit_function`).
    pending_defaults: Vec<(usize, usize)>,
    /// Set while compiling a class body: the next `function` is a method of a class
    /// with this base-class token range.
    pending_method: Option<Option<(usize, usize)>>,
    /// `del` target being parsed: the last accessor deletes instead of reading.
    del_mode: bool,
}

impl<'g> Compiler<'g> {
    pub fn new(
        toks: Vec<Token>,
        globals: &'g mut Globals,
        heap: &'g mut Heap,
        syms: &'g mut Interner,
        unit: u32,
    ) -> Self {
        Compiler {
            toks,
            pos: 0,
            fns: vec![FnState::new("<module>".into(), true, false)],
            globals,
            heap,
            syms,
            unit,
            pending_defaults: Vec::new(),
            pending_method: None,
            del_mode: false,
        }
    }

    /// Compiles the whole token stream into a script function (allocated on `heap`).
    pub fn compile(mut self) -> Res<Gc> {
        while !self.check(&Tok::Eof) {
            self.statement()?;
        }
        self.emit(Op::None);
        self.emit(Op::Return);
        let st = self.fns.pop().unwrap();
        let unit = self.unit;
        Ok(self.heap.alloc(Obj::Func(Box::new(st.finish(0, Vec::new(), unit)))))
    }

    // ------------------------------------------------------------ tokens

    fn peek(&self) -> &Tok {
        &self.toks[self.pos].tok
    }

    fn advance(&mut self) -> Tok {
        let t = self.toks[self.pos].tok.clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }

    fn check(&self, t: &Tok) -> bool {
        self.peek() == t
    }

    fn matches(&mut self, t: &Tok) -> bool {
        if self.check(t) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, t: Tok, msg: &str) -> Res<()> {
        if self.check(&t) {
            self.advance();
            return Ok(());
        }
        let found = &self.toks[self.pos];
        if t == Tok::Semi && found.line > self.prev_line() {
            // The ';' belongs at the end of the previous line, so point there.
            return Err(format!(
                "[line {}] SyntaxError: {} (missing ';' at end of line?)",
                self.prev_line(),
                msg
            ));
        }
        if t == Tok::LBrace && found.tok == Tok::Colon {
            return Err(self.error(&format!(
                "{} (Faston uses {{ }} blocks instead of ':')",
                msg
            )));
        }
        Err(self.error(msg))
    }

    fn expect_ident(&mut self, msg: &str) -> Res<String> {
        if let Tok::Ident(s) = self.peek().clone() {
            self.advance();
            Ok(s)
        } else {
            Err(self.error(msg))
        }
    }

    fn error(&self, msg: &str) -> String {
        let t = &self.toks[self.pos];
        let at = match &t.tok {
            Tok::Eof => "end of file".to_string(),
            tok => format!("'{}'", tok),
        };
        format!("[line {}] SyntaxError at {}: {}", t.line, at, msg)
    }

    fn error_prev(&self, msg: &str) -> String {
        format!("[line {}] SyntaxError: {}", self.prev_line(), msg)
    }

    fn prev_line(&self) -> u32 {
        self.toks[self.pos.saturating_sub(1)].line
    }

    // ------------------------------------------------------------ emitting

    fn f(&mut self) -> &mut FnState {
        self.fns.last_mut().unwrap()
    }

    fn cur(&self) -> &FnState {
        self.fns.last().unwrap()
    }

    fn emit(&mut self, op: Op) -> usize {
        let line = self.prev_line();
        let f = self.f();
        f.cur_stack += op.stack_effect();
        f.max_stack = f.max_stack.max(f.cur_stack);
        f.code.push(op);
        f.lines.push(line);
        // `x = <list just built in x's own slot>` leaves GetLocal(a); SetLocal(a): a no-op.
        let len = f.code.len();
        if len >= 2 && f.last_label != len - 1 {
            if let (Op::GetLocal(a), Op::SetLocal(b)) = (f.code[len - 2], f.code[len - 1]) {
                if a == b {
                    f.code.truncate(len - 2);
                    f.lines.truncate(len - 2);
                    return f.code.len().saturating_sub(1);
                }
            }
        }
        // Peephole: keep fusing the last two instructions while possible. Never fuse
        // an instruction that is a jump target (`last_label` is the highest target).
        loop {
            let len = f.code.len();
            if len < 2 || f.last_label == len - 1 {
                break;
            }
            match Op::fuse(f.code[len - 2], f.code[len - 1]) {
                Some(fused) => {
                    f.code.pop();
                    f.lines.pop();
                    f.code[len - 2] = fused;
                    f.lines[len - 2] = line;
                }
                None => break,
            }
        }
        f.code.len() - 1
    }

    fn here(&self) -> usize {
        self.cur().code.len()
    }

    /// Marks the current position as a jump target and returns it.
    fn label(&mut self) -> usize {
        let h = self.here();
        self.f().last_label = h;
        h
    }

    /// Point the jump at `at` to the current end of the code.
    fn patch(&mut self, at: usize) {
        let target = self.label() as u32;
        let op = &mut self.f().code[at];
        *op = op.with_target(target);
    }

    fn add_const(&mut self, v: Value) -> u32 {
        let c = &mut self.f().consts;
        c.push(v);
        (c.len() - 1) as u32
    }

    fn name_const(&mut self, name: &str) -> u32 {
        self.syms.intern(name)
    }

    // ------------------------------------------------------------ scopes

    fn begin_scope(&mut self) {
        self.f().depth += 1;
    }

    fn end_scope(&mut self) {
        let f = self.f();
        f.depth -= 1;
        let d = f.depth;
        let mut close_from = None;
        while let Some(l) = f.locals.last() {
            if l.depth > d {
                if l.captured {
                    close_from = Some(f.locals.len() - 1);
                }
                f.locals.pop();
            } else {
                break;
            }
        }
        // Captured variables of this block move to the heap before their slots are reused.
        if let Some(slot) = close_from {
            self.emit(Op::CloseUpvals(slot as u32));
        }
    }

    /// Finds `name` in enclosing functions, adding upvalues along the way.
    /// Returns (upvalue index in function `fi`, is_const).
    fn resolve_upvalue(&mut self, fi: usize, name: &str) -> Option<(u32, bool)> {
        if fi == 0 {
            return None;
        }
        let parent = fi - 1;
        if self.fns[parent].global_decls.contains(name) {
            return None;
        }
        if let Some(slot) = self.fns[parent].locals.iter().rposition(|l| l.name == name) {
            let p = &mut self.fns[parent];
            p.locals[slot].captured = true;
            let is_const = p.locals[slot].is_const;
            for lp in p.loops.iter_mut() {
                if slot >= lp.boundary {
                    lp.captured = true;
                }
            }
            return Some((self.add_upvalue(fi, true, slot as u32, is_const), is_const));
        }
        let (up, is_const) = self.resolve_upvalue(parent, name)?;
        Some((self.add_upvalue(fi, false, up, is_const), is_const))
    }

    fn add_upvalue(&mut self, fi: usize, from_local: bool, index: u32, is_const: bool) -> u32 {
        let f = &mut self.fns[fi];
        if let Some(i) = f
            .upvalues
            .iter()
            .position(|u| u.from_local == from_local && u.index == index)
        {
            return i as u32;
        }
        let _ = is_const; // const-ness is checked against the original local
        f.upvalues.push(UpvalDesc { from_local, index });
        (f.upvalues.len() - 1) as u32
    }

    /// Upvalue for a name declared `nonlocal` in the current function.
    fn nonlocal_upvalue(&mut self, name: &str) -> Res<Option<(u32, bool)>> {
        if !self.cur().nonlocal_decls.contains(name) {
            return Ok(None);
        }
        let fi = self.fns.len() - 1;
        match self.resolve_upvalue(fi, name) {
            Some(u) => Ok(Some(u)),
            None => Err(self.error_prev(&format!(
                "no enclosing function variable '{}' for nonlocal",
                name
            ))),
        }
    }

    /// Pushes function `func`: a constant, or a closure if it captures variables or
    /// has default values that must be evaluated now (like Python's `def`).
    fn emit_function(&mut self, func: Gc) -> Res<()> {
        let k = self.add_const(Value::Obj(func));
        let ranges = std::mem::take(&mut self.pending_defaults);
        if func.as_func().dynamic_defaults {
            let resume = self.pos;
            for &(start, end) in &ranges {
                self.pos = start;
                self.expression()?;
                if self.pos != end {
                    return Err(self.error("unexpected token in default value"));
                }
            }
            self.pos = resume;
            self.emit(Op::ClosureDefaults(k, ranges.len() as u32));
        } else if func.as_func().captures.is_empty() {
            self.emit(Op::Const(k));
        } else {
            self.emit(Op::Closure(k));
        }
        Ok(())
    }

    fn resolve_local(&self, name: &str) -> Option<(u32, bool)> {
        let f = self.cur();
        for i in (0..f.locals.len()).rev() {
            if f.locals[i].name == name {
                return Some((i as u32, f.locals[i].is_const));
            }
        }
        None
    }

    fn declare_local(&mut self, name: &str, is_const: bool) -> Res<u32> {
        let line = self.prev_line();
        let f = self.f();
        let d = f.depth;
        for i in (0..f.locals.len()).rev() {
            if f.locals[i].depth < d {
                break;
            }
            if f.locals[i].name == name {
                if f.locals[i].is_const {
                    return Err(format!(
                        "[line {}] SyntaxError: cannot redeclare constant '{}'",
                        line, name
                    ));
                }
                f.locals[i].is_const = is_const;
                return Ok(i as u32);
            }
        }
        f.locals.push(Local {
            name: name.to_string(),
            depth: d,
            is_const,
            captured: false,
        });
        f.max_slots = f.max_slots.max(f.locals.len());
        Ok((f.locals.len() - 1) as u32)
    }

    /// `x` or `k, v` before `in`.
    fn loop_targets(&mut self) -> Res<Vec<String>> {
        let mut names = vec![self.expect_ident("expected loop variable name")?];
        while self.matches(&Tok::Comma) {
            names.push(self.expect_ident("expected loop variable name")?);
        }
        Ok(names)
    }

    /// Slot the loop writes each item to, plus the slots to unpack it into (if several).
    fn declare_loop_targets(&mut self, names: &[String]) -> Res<(u32, Vec<u32>)> {
        if names.len() == 1 {
            return Ok((self.declare_local(&names[0], false)?, Vec::new()));
        }
        let item = self.declare_local("(item)", false)?;
        let mut slots = Vec::new();
        for n in names {
            slots.push(self.declare_local(n, false)?);
        }
        Ok((item, slots))
    }

    fn emit_unpack_targets(&mut self, item: u32, slots: &[u32]) {
        if slots.is_empty() {
            return;
        }
        self.emit(Op::GetLocal(item));
        self.emit(Op::Unpack(slots.len() as u32));
        for s in slots.iter().rev() {
            self.emit(Op::SetLocal(*s));
        }
    }

    /// Three consecutive hidden slots holding a loop's iteration state.
    fn declare_iter_slots(&mut self) -> Res<u32> {
        let a = self.declare_local("(iter)", false)?;
        self.declare_local("(iter.1)", false)?;
        self.declare_local("(iter.2)", false)?;
        Ok(a)
    }

    fn const_err(&self, name: &str) -> String {
        self.error_prev(&format!("cannot assign to constant '{}'", name))
    }

    /// Store the value on top of the stack into global `name`.
    fn store_global(&mut self, name: &str, make_const: bool) -> Res<()> {
        let g = self.globals.slot(name);
        if self.globals.is_const[g as usize] {
            return Err(self.const_err(name));
        }
        if make_const {
            self.globals.is_const[g as usize] = true;
        }
        self.emit(Op::SetGlobal(g));
        Ok(())
    }

    /// `let` / `const`: global at script top level, otherwise block-local.
    fn define_let(&mut self, name: &str, is_const: bool) -> Res<()> {
        let (script, depth) = (self.cur().is_script, self.cur().depth);
        if script && depth == 0 {
            self.store_global(name, is_const)
        } else {
            let s = self.declare_local(name, is_const)?;
            self.emit(Op::SetLocal(s));
            Ok(())
        }
    }

    /// `def` / `class`: an existing local, else a global in scripts, else a new local.
    fn define_named(&mut self, name: &str) -> Res<()> {
        if let Some((u, is_const)) = self.nonlocal_upvalue(name)? {
            if is_const {
                return Err(self.const_err(name));
            }
            self.emit(Op::SetUpval(u));
            return Ok(());
        }
        let global_decl = self.cur().global_decls.contains(name);
        if !global_decl {
            if let Some((slot, is_const)) = self.resolve_local(name) {
                if is_const {
                    return Err(self.const_err(name));
                }
                self.emit(Op::SetLocal(slot));
                return Ok(());
            }
        }
        if self.cur().is_script || global_decl {
            self.store_global(name, false)
        } else {
            let s = self.declare_local(name, false)?;
            self.emit(Op::SetLocal(s));
            Ok(())
        }
    }

    // ------------------------------------------------------------ statements

    fn statement(&mut self) -> Res<()> {
        match self.peek() {
            Tok::Let => self.let_decl(false),
            Tok::Const => self.let_decl(true),
            Tok::Global => self.global_decl(),
            Tok::Def => self.def_decl(),
            Tok::Class => self.class_decl(),
            Tok::If => self.if_stmt(),
            Tok::While => self.while_stmt(),
            Tok::For => self.for_stmt(),
            Tok::Return => self.return_stmt(),
            Tok::Break => self.break_stmt(),
            Tok::Continue => self.continue_stmt(),
            Tok::Import => self.import_stmt(),
            Tok::From => self.from_import_stmt(),
            Tok::Nonlocal => self.nonlocal_decl(),
            Tok::Try => self.try_stmt(),
            Tok::Raise => self.raise_stmt(),
            Tok::Assert => self.assert_stmt(),
            Tok::Del => self.del_stmt(),
            Tok::With => self.with_stmt(),
            Tok::At => self.decorated(),
            Tok::Yield => {
                self.yield_expr()?;
                self.expect(Tok::Semi, "expected ';' after yield")?;
                self.emit(Op::Pop);
                Ok(())
            }
            Tok::Ident(s) if s == "match" && self.is_match_start() => self.match_stmt(),
            Tok::Ident(s)
                if s == "enum"
                    && matches!(self.toks[self.pos + 1].tok, Tok::Ident(_))
                    && self.toks[self.pos + 2].tok == Tok::LBrace =>
            {
                self.advance();
                let name = self.class_value(true)?;
                self.define_named(&name)
            }
            Tok::Ident(_) if self.toks[self.pos + 1].tok == Tok::Comma => self.multi_assign(),
            Tok::Pass => {
                self.advance();
                self.expect(Tok::Semi, "expected ';' after 'pass'")
            }
            Tok::Semi => {
                self.advance();
                Ok(())
            }
            _ => self.expr_stmt(),
        }
    }

    fn let_decl(&mut self, is_const: bool) -> Res<()> {
        self.advance();
        let kw = if is_const { "const" } else { "let" };
        let name = self.expect_ident(&format!("expected variable name after '{}'", kw))?;
        if self.check(&Tok::Comma) {
            // let a, b = 1, 2;
            let mut names = vec![name];
            while self.matches(&Tok::Comma) {
                names.push(self.expect_ident("expected variable name")?);
            }
            if self.matches(&Tok::Assign) {
                self.expr_list()?;
                self.emit(Op::Unpack(names.len() as u32));
            } else if is_const {
                return Err(self.error("constants need values: const A, B = 1, 2;"));
            } else {
                for _ in &names {
                    self.emit(Op::None);
                }
            }
            self.expect(Tok::Semi, "expected ';' after variable declaration")?;
            for n in names.iter().rev() {
                self.define_let(n, is_const)?;
            }
            return Ok(());
        }
        if self.matches(&Tok::Assign) {
            self.expression()?;
        } else if is_const {
            return Err(self.error("a constant needs a value: const NAME = value;"));
        } else {
            self.emit(Op::None);
        }
        self.expect(Tok::Semi, "expected ';' after variable declaration")?;
        self.define_let(&name, is_const)
    }

    /// `a, b = b, a;`  /  `x, y = point;`
    fn multi_assign(&mut self) -> Res<()> {
        let mut names = Vec::new();
        loop {
            names.push(self.expect_ident("expected a variable name")?);
            if !self.matches(&Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::Assign, "expected '=' after assignment targets")?;
        self.expr_list()?;
        self.expect(Tok::Semi, "expected ';' after assignment")?;
        self.emit(Op::Unpack(names.len() as u32));
        for n in names.iter().rev() {
            self.define_named(n)?;
        }
        Ok(())
    }

    /// One expression, or several separated by commas packed into a list.
    fn expr_list(&mut self) -> Res<()> {
        self.expression()?;
        let mut n = 1u32;
        while self.matches(&Tok::Comma) {
            self.expression()?;
            n += 1;
        }
        if n > 1 {
            self.emit(Op::BuildTuple(n));
        }
        Ok(())
    }

    /// `game.enemies` (a module name, possibly dotted)
    fn dotted_name(&mut self, msg: &str) -> Res<String> {
        let mut name = self.expect_ident(msg)?;
        while self.check(&Tok::Dot) {
            self.advance();
            name.push('.');
            name.push_str(&self.expect_ident("expected a name after '.'")?);
        }
        Ok(name)
    }

    /// `import math, game.enemies, collections as c;`
    fn import_stmt(&mut self) -> Res<()> {
        self.advance();
        loop {
            let name = self.dotted_name("expected a module name after 'import'")?;
            let sym = self.name_const(&name);
            self.emit(Op::Import(sym));
            if self.matches(&Tok::As) {
                let alias = self.expect_ident("expected a name after 'as'")?;
                self.define_named(&alias)?;
            } else {
                // `import a.b` binds `a` (with `b` loaded as its attribute)
                let first = name.split('.').next().unwrap().to_string();
                if first != name {
                    self.emit(Op::Pop);
                    let fsym = self.name_const(&first);
                    self.emit(Op::Import(fsym));
                }
                self.define_named(&first)?;
            }
            if !self.matches(&Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::Semi, "expected ';' after import")
    }

    /// `from game.enemies import Slime, spawn as make;`
    fn from_import_stmt(&mut self) -> Res<()> {
        self.advance();
        let module = self.dotted_name("expected a module name after 'from'")?;
        self.expect(Tok::Import, "expected 'import' after the module name")?;
        let msym = self.name_const(&module);
        self.emit(Op::Import(msym));
        loop {
            let name = self.expect_ident("expected a name to import")?;
            let n = self.name_const(&name);
            self.emit(Op::Dup);
            self.emit(Op::GetAttr(n));
            let bind = if self.matches(&Tok::As) {
                self.expect_ident("expected a name after 'as'")?
            } else {
                name
            };
            self.define_named(&bind)?;
            if !self.matches(&Tok::Comma) {
                break;
            }
        }
        self.emit(Op::Pop);
        self.expect(Tok::Semi, "expected ';' after import")
    }

    /// `nonlocal count;` — assign to a variable of an enclosing function.
    fn nonlocal_decl(&mut self) -> Res<()> {
        self.advance();
        if self.fns.len() < 2 {
            return Err(self.error_prev("'nonlocal' is only allowed inside a nested function"));
        }
        loop {
            let name = self.expect_ident("expected a name after 'nonlocal'")?;
            self.f().nonlocal_decls.insert(name.clone());
            self.nonlocal_upvalue(&name)?; // report unknown names right away
            if !self.matches(&Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::Semi, "expected ';' after nonlocal declaration")
    }

    /// `assert cond;`  /  `assert cond, "message";`
    fn assert_stmt(&mut self) -> Res<()> {
        self.advance();
        let g = self.globals.slot("$assert");
        self.emit(Op::GetGlobal(g));
        self.expression()?;
        if self.matches(&Tok::Comma) {
            self.expression()?;
        } else {
            self.emit(Op::None);
        }
        self.expect(Tok::Semi, "expected ';' after assert")?;
        self.emit(Op::Call(2));
        self.emit(Op::Pop);
        Ok(())
    }

    /// A literal usable as a default parameter value.
    fn literal_value(&mut self) -> Res<Value> {
        let neg = self.matches(&Tok::Minus);
        let v = match self.advance() {
            Tok::Int(i) => Value::Int(if neg { -i } else { i }),
            Tok::Float(f) => Value::Float(if neg { -f } else { f }),
            Tok::Str(s) if !neg => self.heap.string(s),
            Tok::Bytes(b) if !neg => self.heap.bytes(b),
            Tok::True if !neg => Value::Bool(true),
            Tok::False if !neg => Value::Bool(false),
            Tok::None if !neg => Value::None,
            _ => {
                self.pos -= 1;
                return Err(self.error(
                    "default values must be literals (numbers, strings, True, False, None)",
                ));
            }
        };
        Ok(v)
    }

    fn global_decl(&mut self) -> Res<()> {
        self.advance();
        if self.cur().is_script {
            return Err(self.error_prev("'global' is only allowed inside a function"));
        }
        loop {
            let name = self.expect_ident("expected a name after 'global'")?;
            self.f().global_decls.insert(name);
            if !self.matches(&Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::Semi, "expected ';' after global declaration")
    }

    fn def_decl(&mut self) -> Res<()> {
        self.advance();
        let name = self.expect_ident("expected function name after 'def'")?;
        // Inside a function, declare the local first so the nested function can
        // refer to itself (recursion) through its captured variable.
        let f = self.cur();
        if !f.is_script
            && !f.global_decls.contains(&name)
            && !f.nonlocal_decls.contains(&name)
            && self.resolve_local(&name).is_none()
        {
            self.declare_local(&name, false)?;
        }
        let func = self.function(name.clone(), false)?;
        self.emit_function(func)?;
        self.define_named(&name)
    }

    /// Parses `(params) { body }` and returns the compiled function.
    fn function(&mut self, name: String, is_init: bool) -> Res<Gc> {
        self.fns.push(FnState::new(name, false, is_init));
        if let Some(base) = self.pending_method.take() {
            self.f().is_method = true;
            self.f().class_base = base;
        }
        self.expect(Tok::LParen, "expected '(' after function name")?;
        let (arity, defaults, dynamic) = self.params()?;
        self.hoist_locals()?;
        if self.cur().is_generator {
            if is_init {
                return Err(self.error("init() can't be a generator (it contains 'yield')"));
            }
            self.emit(Op::Gen);
        }
        self.block()?;
        if is_init {
            self.emit(Op::GetLocal(0));
        } else {
            self.emit(Op::None);
        }
        self.emit(Op::Return);
        Ok(self.finish_function(arity, defaults, dynamic))
    }

    fn finish_function(&mut self, arity: u32, defaults: Vec<Value>, dynamic: Vec<(usize, usize)>) -> Gc {
        let st = self.fns.pop().unwrap();
        let unit = self.unit;
        let mut f = st.finish(arity, defaults, unit);
        f.dynamic_defaults = !dynamic.is_empty();
        self.pending_defaults = dynamic;
        self.heap.alloc(Obj::Func(Box::new(f)))
    }

    /// `lambda (a, b) a + b`  or  `lambda (a) { statements }`
    fn lambda(&mut self) -> Res<()> {
        self.fns.push(FnState::new("<lambda>".into(), false, false));
        self.expect(Tok::LParen, "expected '(' after 'lambda'")?;
        let (arity, defaults, dynamic) = self.params()?;
        if self.check(&Tok::LBrace) {
            self.hoist_locals()?;
            if self.cur().is_generator {
                return Err(self.error("a lambda can't contain 'yield' (use def)"));
            }
            self.block()?;
            self.emit(Op::None);
        } else {
            self.expression()?;
        }
        self.emit(Op::Return);
        let func = self.finish_function(arity, defaults, dynamic);
        self.emit_function(func)?;
        Ok(())
    }

    /// Python scoping: a name plainly assigned anywhere in a function body (`x = ...`,
    /// `x += ...`, `a, b = ...`), even inside a nested block, is a local of the whole
    /// function. Pre-scans the body (cursor on its `{`, nested functions and classes
    /// skipped) and declares those names up front. `let` names stay block-scoped.
    fn hoist_locals(&mut self) -> Res<()> {
        let end = self.skip_braces(self.pos)?;
        let mut names: Vec<String> = Vec::new();
        let mut excluded: HashSet<String> = HashSet::new();
        let mut i = self.pos + 1;
        let mut stmt_start = true;
        let mut saw_yield = false;
        while i < end - 1 {
            match &self.toks[i].tok {
                Tok::Yield => {
                    saw_yield = true;
                    stmt_start = false;
                }
                // decorators: skip `@expr` so its names aren't taken for assignments
                Tok::At => {
                    stmt_start = false;
                }
                Tok::Def => {
                    i += 2;
                    i = self.skip_group(i, Tok::LParen, Tok::RParen, "expected '('")?;
                    i = self.skip_braces(i)?;
                    stmt_start = true;
                    continue;
                }
                Tok::Class => {
                    i += 2;
                    if self.toks[i].tok == Tok::LParen {
                        i = self.skip_group(i, Tok::LParen, Tok::RParen, "expected '('")?;
                    }
                    i = self.skip_braces(i)?;
                    stmt_start = true;
                    continue;
                }
                Tok::Lambda => {
                    i += 1;
                    i = self.skip_group(i, Tok::LParen, Tok::RParen, "expected '(' after 'lambda'")?;
                    if self.toks[i].tok == Tok::LBrace {
                        i = self.skip_braces(i)?;
                    }
                    stmt_start = false;
                    continue;
                }
                Tok::Global | Tok::Nonlocal => {
                    i += 1;
                    while let Tok::Ident(n) = &self.toks[i].tok {
                        excluded.insert(n.clone());
                        i += 1;
                        if self.toks[i].tok != Tok::Comma {
                            break;
                        }
                        i += 1;
                    }
                    continue;
                }
                // enum bodies hold members, not assignments
                Tok::Ident(e)
                    if stmt_start
                        && e == "enum"
                        && matches!(self.toks[i + 1].tok, Tok::Ident(_))
                        && self.toks[i + 2].tok == Tok::LBrace =>
                {
                    i += 2;
                    i = self.skip_braces(i)?;
                    stmt_start = true;
                    continue;
                }
                Tok::Ident(first) if stmt_start => {
                    let mut group = vec![first.clone()];
                    let mut j = i + 1;
                    while self.toks[j].tok == Tok::Comma {
                        match &self.toks[j + 1].tok {
                            Tok::Ident(n) => {
                                group.push(n.clone());
                                j += 2;
                            }
                            _ => break,
                        }
                    }
                    // Only plain `=` makes a new local; `x += 1` on an unknown name is
                    // reported at compile time (see `variable`), as before.
                    if self.toks[j].tok == Tok::Assign {
                        names.extend(group);
                    }
                    stmt_start = false;
                    i = j;
                    continue;
                }
                Tok::Semi | Tok::LBrace | Tok::RBrace => stmt_start = true,
                _ => stmt_start = false,
            }
            i += 1;
        }
        for n in names {
            if !excluded.contains(&n) && self.resolve_local(&n).is_none() {
                self.declare_local(&n, false)?;
            }
        }
        if saw_yield {
            self.f().is_generator = true;
        }
        Ok(())
    }

    /// Parameter list after '(' up to and including ')'. Declares the parameters.
    /// Returns (arity, literal defaults, token ranges of defaults). If every default is a
    /// literal the ranges are empty; otherwise all defaults are evaluated at `def` time.
    fn params(&mut self) -> Res<(u32, Vec<Value>, Vec<(usize, usize)>)> {
        let mut arity = 0u32;
        let mut defaults = Vec::new();
        let mut ranges = Vec::new();
        let mut all_literal = true;
        let mut star: Option<String> = None;
        let mut starstar: Option<String> = None;
        if !self.check(&Tok::RParen) {
            loop {
                if starstar.is_some() {
                    return Err(self.error("'**kwargs' must be the last parameter"));
                }
                if self.matches(&Tok::StarStar) {
                    starstar = Some(self.expect_ident("expected a name after '**'")?);
                    if !self.matches(&Tok::Comma) || self.check(&Tok::RParen) {
                        break;
                    }
                    continue;
                }
                if self.matches(&Tok::Star) {
                    if star.is_some() {
                        return Err(self.error_prev("only one '*args' parameter is allowed"));
                    }
                    star = Some(self.expect_ident("expected a name after '*'")?);
                    if !self.matches(&Tok::Comma) || self.check(&Tok::RParen) {
                        break;
                    }
                    continue;
                }
                if star.is_some() {
                    return Err(self.error(
                        "parameters after '*args' aren't supported yet; put them before it",
                    ));
                }
                let p = self.expect_ident("expected parameter name")?;
                if self.cur().locals.iter().any(|l| l.name == p) {
                    return Err(self.error_prev(&format!("duplicate parameter '{}'", p)));
                }
                self.declare_local(&p, false)?;
                let psym = self.name_const(&p);
                self.f().param_syms.push(psym);
                arity += 1;
                if self.matches(&Tok::Assign) {
                    let start = self.pos;
                    let end = self.skip_default_expr()?;
                    ranges.push((start, end));
                    self.pos = start;
                    match self.literal_value() {
                        Ok(v) if self.pos == end => defaults.push(v),
                        _ => {
                            all_literal = false;
                            defaults.push(Value::None); // placeholder; real value comes at def time
                        }
                    }
                    self.pos = end;
                } else if !defaults.is_empty() {
                    return Err(self.error_prev(&format!(
                        "parameter '{}' needs a default value because an earlier one has one",
                        p
                    )));
                }
                if !self.matches(&Tok::Comma) || self.check(&Tok::RParen) {
                    break;
                }
            }
        }
        self.expect(Tok::RParen, "expected ')' after parameters")?;
        // `*args` / `**kwargs` slots come right after the regular parameters.
        for (name, is_star) in [(star, true), (starstar, false)] {
            if let Some(n) = name {
                if self.cur().locals.iter().any(|l| l.name == n) {
                    return Err(self.error_prev(&format!("duplicate parameter '{}'", n)));
                }
                self.declare_local(&n, false)?;
                if is_star {
                    self.f().varargs = true;
                } else {
                    self.f().varkw = true;
                }
            }
        }
        if all_literal {
            ranges.clear();
        }
        Ok((arity, defaults, ranges))
    }

    /// Finds the end of a default-value expression (the next top-level ',' or ')').
    fn skip_default_expr(&mut self) -> Res<usize> {
        let mut depth = 0i32;
        let mut i = self.pos;
        loop {
            match &self.toks[i].tok {
                Tok::LParen | Tok::LBracket | Tok::LBrace => depth += 1,
                Tok::RParen | Tok::RBracket | Tok::RBrace if depth > 0 => depth -= 1,
                Tok::Comma | Tok::RParen if depth == 0 => break,
                Tok::Eof | Tok::Semi | Tok::RBracket | Tok::RBrace => {
                    self.pos = i;
                    return Err(self.error("unterminated default value"));
                }
                _ => {}
            }
            i += 1;
        }
        if i == self.pos {
            return Err(self.error("expected a default value after '='"));
        }
        Ok(i)
    }

    fn class_decl(&mut self) -> Res<()> {
        self.advance();
        let name = self.class_value(false)?;
        self.define_named(&name)
    }

    /// `class Name(Base) { ... }` after the `class` keyword (or, with `enum_syntax`,
    /// `enum Name { A, B = 2, ... }` after `enum`): leaves the class on the stack and
    /// returns its name. Classes based on `Enum` get their attributes turned into
    /// members.
    fn class_value(&mut self, enum_syntax: bool) -> Res<String> {
        let what = if enum_syntax { "enum" } else { "class" };
        let name = self.expect_ident(&format!("expected a name after '{}'", what))?;
        let n = self.name_const(&name);
        self.emit(Op::Class(n));
        let mut base = None;
        let mut is_enum = enum_syntax;
        if enum_syntax {
            let g = self.globals.slot("Enum");
            self.emit(Op::GetGlobal(g));
            self.emit(Op::Inherit);
        } else if self.matches(&Tok::LParen) {
            let start = self.pos;
            self.expression()?;
            base = Some((start, self.pos));
            let toks: Vec<&Tok> = self.toks[start..self.pos].iter().map(|t| &t.tok).collect();
            is_enum = matches!(toks.as_slice(), [Tok::Ident(e)] if e == "Enum")
                || matches!(toks.as_slice(), [Tok::Ident(m), Tok::Dot, Tok::Ident(e)] if m == "enum" && e == "Enum");
            self.expect(Tok::RParen, "expected ')' after superclass")?;
            self.emit(Op::Inherit);
        }
        self.expect(Tok::LBrace, &format!("expected '{{' before {} body", what))?;
        let mut members: HashSet<String> = HashSet::new();
        while !self.check(&Tok::RBrace) && !self.check(&Tok::Eof) {
            if enum_syntax && matches!(self.peek(), Tok::Ident(_)) {
                // enum member: NAME or NAME = value, separated by ',' or ';'
                let m = self.expect_ident("expected a member name")?;
                if self.matches(&Tok::Assign) {
                    self.expression()?;
                } else {
                    let g = self.globals.slot("$auto");
                    self.emit(Op::GetGlobal(g));
                }
                let mn = self.name_const(&m);
                self.emit(Op::Method(mn));
                members.insert(m);
                if !self.matches(&Tok::Comma) && !self.matches(&Tok::Semi) && !self.check(&Tok::RBrace) {
                    return Err(self.error("expected ',' between enum members"));
                }
                continue;
            }
            if self.check(&Tok::At) || self.check(&Tok::Def) {
                // [decorators] def name(self, ...) { ... }
                let mut ndec = 0;
                while self.matches(&Tok::At) {
                    // `@prop.setter`: `prop` is an earlier member of this class
                    let accessor = match (&self.toks[self.pos].tok, &self.toks[self.pos + 1].tok, &self.toks[self.pos + 2].tok) {
                        (Tok::Ident(p), Tok::Dot, Tok::Ident(a))
                            if members.contains(p)
                                && matches!(a.as_str(), "setter" | "getter" | "deleter")
                                && self.toks[self.pos + 3].tok == Tok::Def =>
                        {
                            Some((p.clone(), a.clone()))
                        }
                        _ => None,
                    };
                    match accessor {
                        Some((p, a)) => {
                            self.pos += 3;
                            self.emit(Op::Dup); // the class being built
                            let pn = self.name_const(&p);
                            self.emit(Op::GetAttr(pn));
                            let an = self.name_const(&a);
                            self.emit(Op::GetAttr(an));
                        }
                        None => {
                            self.parse_or(false)?;
                        }
                    }
                    ndec += 1;
                }
                self.expect(Tok::Def, "expected 'def' after a decorator in a class body")?;
                let mname = self.expect_ident("expected method name after 'def'")?;
                let is_init = mname == "init";
                self.pending_method = Some(base);
                let func = self.function(mname.clone(), is_init)?;
                if ndec == 0 && func.as_func().arity == 0 && !func.as_func().varargs {
                    return Err(self.error_prev(&format!(
                        "method '{}' needs a 'self' parameter (or use @staticmethod)",
                        mname
                    )));
                }
                self.emit_function(func)?;
                for _ in 0..ndec {
                    self.emit(Op::Call(1));
                }
                let mn = self.name_const(&mname);
                self.emit(Op::Method(mn));
                members.insert(mname);
            } else if self.matches(&Tok::Pass) {
                self.expect(Tok::Semi, "expected ';' after 'pass'")?;
            } else if let Some(attr) = match (self.peek(), &self.toks[self.pos + 1].tok) {
                (Tok::Ident(a), Tok::Assign) => Some(a.clone()),
                _ => None,
            } {
                // class attribute: NAME = value;
                self.pos += 2;
                self.expression()?;
                self.expect(Tok::Semi, "expected ';' after class attribute")?;
                let an = self.name_const(&attr);
                self.emit(Op::Method(an));
                members.insert(attr);
            } else {
                return Err(self.error(
                    "a class body can only contain methods (def), class attributes (name = value;) and pass",
                ));
            }
        }
        self.expect(Tok::RBrace, &format!("expected '}}' after {} body", what))?;
        if is_enum {
            // $enum(cls), keeping the class on the stack
            self.emit(Op::Dup);
            let g = self.globals.slot("$enum");
            self.emit(Op::GetGlobal(g));
            self.emit(Op::Swap);
            self.emit(Op::Call(1));
            self.emit(Op::Pop);
        }
        Ok(name)
    }

    // ------------------------------------------------------------ match

    /// `match (subject) {` or `match name {` (otherwise `match` is a plain name).
    fn is_match_start(&self) -> bool {
        match &self.toks[self.pos + 1].tok {
            Tok::LParen => match self.skip_group(self.pos + 1, Tok::LParen, Tok::RParen, "") {
                Ok(i) => self.toks.get(i).is_some_and(|t| t.tok == Tok::LBrace),
                Err(_) => false,
            },
            Tok::Ident(_) => self.toks[self.pos + 2].tok == Tok::LBrace,
            _ => false,
        }
    }

    /// match (subject) { case PATTERN [if guard] { ... } ... }
    fn match_stmt(&mut self) -> Res<()> {
        self.advance();
        if self.check(&Tok::LParen) {
            self.paren_condition("match")?;
        } else {
            self.expression()?;
        }
        self.begin_scope();
        let subject = self.declare_local("(match)", false)?;
        self.emit(Op::SetLocal(subject));
        self.expect(Tok::LBrace, "expected '{' after the match subject")?;
        let mut ends = Vec::new();
        while !self.check(&Tok::RBrace) && !self.check(&Tok::Eof) {
            match self.peek() {
                Tok::Ident(s) if s == "case" => {
                    self.advance();
                }
                _ => return Err(self.error("expected 'case' in a match block")),
            }
            self.begin_scope();
            let mut fails = Vec::new();
            self.pattern(subject, &mut fails)?;
            if self.matches(&Tok::If) {
                self.expression()?;
                fails.push(self.emit(Op::JumpIfFalse(0)));
            }
            self.block()?;
            self.end_scope();
            ends.push(self.emit(Op::Jump(0)));
            for f in fails {
                self.patch(f);
            }
        }
        self.expect(Tok::RBrace, "expected '}' to close the match block")?;
        for e in ends {
            self.patch(e);
        }
        self.end_scope();
        Ok(())
    }

    /// A fresh hidden local for a sub-value being matched.
    fn match_temp(&mut self) -> Res<u32> {
        let n = self.cur().locals.len();
        self.declare_local(&format!("(pat{})", n), false)
    }

    /// Binds the value in local `src` to `name` (a capture pattern).
    fn match_bind(&mut self, src: u32, name: &str) -> Res<()> {
        let s = self.declare_local(name, false)?;
        self.emit(Op::GetLocal(src));
        self.emit(Op::SetLocal(s));
        Ok(())
    }

    /// Matches the value in local `src` against a pattern; jumps that leave on a
    /// mismatch are added to `fails`.
    fn pattern(&mut self, src: u32, fails: &mut Vec<usize>) -> Res<()> {
        let mut oks = Vec::new();
        loop {
            let mut alt_fails = Vec::new();
            self.closed_pattern(src, &mut alt_fails)?;
            if self.matches(&Tok::Pipe) {
                oks.push(self.emit(Op::Jump(0)));
                for f in alt_fails {
                    self.patch(f);
                }
                continue;
            }
            fails.extend(alt_fails);
            break;
        }
        for o in oks {
            self.patch(o);
        }
        if self.matches(&Tok::As) {
            let name = self.expect_ident("expected a name after 'as'")?;
            self.match_bind(src, &name)?;
        }
        Ok(())
    }

    /// Pushes a literal pattern's value: a number (maybe negative), str or bytes.
    fn pattern_literal(&mut self) -> Res<()> {
        let neg = self.matches(&Tok::Minus);
        let v = match self.advance() {
            Tok::Int(i) => Value::Int(if neg { -i } else { i }),
            Tok::Float(x) => Value::Float(if neg { -x } else { x }),
            Tok::Str(s) if !neg => self.heap.string(s),
            Tok::Bytes(b) if !neg => self.heap.bytes(b),
            _ => return Err(self.error_prev("expected a literal in the pattern")),
        };
        let k = self.add_const(v);
        self.emit(Op::Const(k));
        Ok(())
    }

    /// Pushes `a.b.c` (the first name is already consumed).
    fn dotted_value(&mut self, first: String) -> Res<()> {
        self.variable(first, false)?;
        while self.matches(&Tok::Dot) {
            let attr = self.expect_ident("expected a name after '.'")?;
            let s = self.name_const(&attr);
            self.emit(Op::GetAttr(s));
        }
        Ok(())
    }

    fn closed_pattern(&mut self, src: u32, fails: &mut Vec<usize>) -> Res<()> {
        match self.peek().clone() {
            Tok::Int(_) | Tok::Float(_) | Tok::Str(_) | Tok::Bytes(_) | Tok::Minus => {
                self.emit(Op::GetLocal(src));
                self.pattern_literal()?;
                self.emit(Op::Eq);
                fails.push(self.emit(Op::JumpIfFalse(0)));
            }
            Tok::None | Tok::True | Tok::False => {
                self.emit(Op::GetLocal(src));
                let op = match self.advance() {
                    Tok::None => Op::None,
                    Tok::True => Op::True,
                    _ => Op::False,
                };
                self.emit(op);
                self.emit(Op::Is);
                fails.push(self.emit(Op::JumpIfFalse(0)));
            }
            Tok::LBracket => {
                self.advance();
                self.seq_pattern(src, fails, Tok::RBracket)?;
            }
            Tok::LParen => {
                self.advance();
                if self.seq_has_comma(Tok::RParen) || self.check(&Tok::RParen) {
                    self.seq_pattern(src, fails, Tok::RParen)?;
                } else {
                    self.pattern(src, fails)?;
                    self.expect(Tok::RParen, "expected ')' after the pattern")?;
                }
            }
            Tok::LBrace => {
                self.advance();
                self.map_pattern(src, fails)?;
            }
            Tok::Ident(name) => {
                self.advance();
                if self.check(&Tok::Dot) || self.check(&Tok::LParen) {
                    let start = self.pos - 1;
                    let mut end = self.pos;
                    while self.toks[end].tok == Tok::Dot {
                        end += 2;
                    }
                    if self.toks[end].tok == Tok::LParen {
                        self.class_pattern(src, fails, start, end)?;
                    } else {
                        // a value: Color.RED
                        self.emit(Op::GetLocal(src));
                        self.dotted_value(name)?;
                        self.emit(Op::Eq);
                        fails.push(self.emit(Op::JumpIfFalse(0)));
                    }
                } else if name != "_" {
                    self.match_bind(src, &name)?;
                }
            }
            _ => return Err(self.error("expected a pattern")),
        }
        Ok(())
    }

    /// Is there a top-level ',' before the closing bracket (a sequence, not a group)?
    fn seq_has_comma(&self, close: Tok) -> bool {
        let mut depth = 0;
        let mut i = self.pos;
        while i < self.toks.len() {
            match &self.toks[i].tok {
                Tok::LParen | Tok::LBracket | Tok::LBrace => depth += 1,
                Tok::RParen | Tok::RBracket | Tok::RBrace if depth > 0 => depth -= 1,
                t if *t == close && depth == 0 => return false,
                Tok::Comma if depth == 0 => return true,
                Tok::Eof => return false,
                _ => {}
            }
            i += 1;
        }
        false
    }

    /// [p, q, *rest, r] / (p, q): the opening bracket is consumed.
    fn seq_pattern(&mut self, src: u32, fails: &mut Vec<usize>, close: Tok) -> Res<()> {
        // count the items and find the star first
        let (mut items, mut star) = (0usize, None);
        let mut depth = 0;
        let mut i = self.pos;
        let mut item_start = true;
        loop {
            let t = &self.toks[i].tok;
            match t {
                Tok::Eof => return Err(self.error("unclosed pattern")),
                t if *t == close && depth == 0 => break,
                Tok::LParen | Tok::LBracket | Tok::LBrace => depth += 1,
                Tok::RParen | Tok::RBracket | Tok::RBrace => depth -= 1,
                _ => {}
            }
            if depth == 0 && *t == Tok::Comma {
                item_start = true;
            } else if item_start {
                if *t == Tok::Star && depth == 0 {
                    if star.is_some() {
                        return Err(self.error("only one '*' is allowed in a sequence pattern"));
                    }
                    star = Some(items);
                }
                items += 1;
                item_start = false;
            }
            i += 1;
        }
        let fixed = items - star.is_some() as usize;
        self.emit(Op::GetLocal(src));
        self.emit(Op::MatchSeq(fixed as u32, star.is_some() as u32));
        fails.push(self.emit(Op::JumpIfFalse(0)));
        for k in 0..items {
            if Some(k) == star {
                self.expect(Tok::Star, "expected '*'")?;
                let name = self.expect_ident("expected a name after '*'")?;
                if name != "_" {
                    let s = self.declare_local(&name, false)?;
                    self.emit(Op::GetLocal(src));
                    self.emit(Op::MatchRest(k as u32, (items - 1 - k) as u32));
                    self.emit(Op::SetLocal(s));
                }
            } else {
                let index = match star {
                    Some(s) if k > s => -((items - k) as i64),
                    _ => k as i64,
                };
                let tmp = self.match_temp()?;
                self.emit(Op::GetLocal(src));
                let c = self.add_const(Value::Int(index));
                self.emit(Op::Const(c));
                self.emit(Op::GetIndex);
                self.emit(Op::SetLocal(tmp));
                self.pattern(tmp, fails)?;
            }
            if k + 1 < items {
                self.expect(Tok::Comma, "expected ',' between patterns")?;
            } else {
                self.matches(&Tok::Comma);
            }
        }
        self.expect(close, "expected the end of the sequence pattern")
    }

    /// {"key": pattern, ...}: the '{' is consumed.
    fn map_pattern(&mut self, src: u32, fails: &mut Vec<usize>) -> Res<()> {
        self.emit(Op::GetLocal(src));
        self.emit(Op::MatchMap);
        fails.push(self.emit(Op::JumpIfFalse(0)));
        let mut keys = Vec::new();
        while !self.check(&Tok::RBrace) {
            if self.matches(&Tok::StarStar) {
                // **rest: the other items, as a new dict
                let name = self.expect_ident("expected a name after '**'")?;
                self.matches(&Tok::Comma);
                if !self.check(&Tok::RBrace) {
                    return Err(self.error("'**rest' must be the last item of a mapping pattern"));
                }
                let resume = self.pos;
                self.emit(Op::GetLocal(src));
                for &k in keys.iter() {
                    self.pos = k;
                    self.map_key()?;
                }
                self.pos = resume;
                self.emit(Op::MatchMapRest(keys.len() as u32));
                let s = self.declare_local(&name, false)?;
                self.emit(Op::SetLocal(s));
                break;
            }
            let tmp = self.match_temp()?;
            self.emit(Op::GetLocal(src));
            keys.push(self.pos);
            self.map_key()?;
            self.expect(Tok::Colon, "expected ':' after the key")?;
            self.emit(Op::MatchKey(tmp));
            fails.push(self.emit(Op::JumpIfFalse(0)));
            self.pattern(tmp, fails)?;
            if !self.matches(&Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RBrace, "expected '}' after the mapping pattern")
    }

    /// Pushes a mapping pattern's key: a literal or a dotted value.
    fn map_key(&mut self) -> Res<()> {
        match self.peek().clone() {
            Tok::Ident(n) => {
                self.advance();
                self.dotted_value(n)
            }
            Tok::None | Tok::True | Tok::False => {
                let op = match self.advance() {
                    Tok::None => Op::None,
                    Tok::True => Op::True,
                    _ => Op::False,
                };
                self.emit(op);
                Ok(())
            }
            _ => self.pattern_literal(),
        }
    }

    /// Point(x, y=0): the class name is the tokens [start, end); the cursor is
    /// just after the first name.
    fn class_pattern(&mut self, src: u32, fails: &mut Vec<usize>, start: usize, end: usize) -> Res<()> {
        let load_class = |c: &mut Self| -> Res<()> {
            let resume = c.pos;
            c.pos = start;
            let first = c.expect_ident("expected a class name")?;
            c.dotted_value(first)?;
            let _ = resume;
            Ok(())
        };
        self.emit(Op::GetLocal(src));
        load_class(self)?;
        self.expect(Tok::LParen, "expected '('")?;
        let after = self.pos;
        let _ = end;
        self.emit(Op::MatchClass);
        fails.push(self.emit(Op::JumpIfFalse(0)));
        let mut i = 0u32;
        let mut keywords = false;
        while !self.check(&Tok::RParen) {
            let tmp = self.match_temp()?;
            if matches!(self.peek(), Tok::Ident(_)) && self.toks[self.pos + 1].tok == Tok::Assign {
                let attr = self.expect_ident("expected an attribute name")?;
                self.advance();
                let s = self.name_const(&attr);
                self.emit(Op::GetLocal(src));
                self.emit(Op::MatchAttr(s, tmp));
                keywords = true;
            } else {
                if keywords {
                    return Err(self.error("positional patterns must come before keyword patterns"));
                }
                self.emit(Op::GetLocal(src));
                let here = self.pos;
                load_class(self)?;
                self.pos = here;
                self.emit(Op::MatchArg(i, tmp));
                i += 1;
            }
            fails.push(self.emit(Op::JumpIfFalse(0)));
            self.pattern(tmp, fails)?;
            if !self.matches(&Tok::Comma) {
                break;
            }
        }
        let _ = after;
        self.expect(Tok::RParen, "expected ')' after the class pattern")
    }

    /// `@decorator ... def f() {}` / `@decorator class C {}`
    fn decorated(&mut self) -> Res<()> {
        let mut n = 0;
        while self.matches(&Tok::At) {
            self.parse_or(false)?;
            n += 1;
        }
        match self.peek() {
            Tok::Def => {
                self.advance();
                let name = self.expect_ident("expected function name after 'def'")?;
                let f = self.cur();
                if !f.is_script
                    && !f.global_decls.contains(&name)
                    && !f.nonlocal_decls.contains(&name)
                    && self.resolve_local(&name).is_none()
                {
                    self.declare_local(&name, false)?;
                }
                let func = self.function(name.clone(), false)?;
                self.emit_function(func)?;
                for _ in 0..n {
                    self.emit(Op::Call(1));
                }
                self.define_named(&name)
            }
            Tok::Class => {
                self.advance();
                let name = self.class_value(false)?;
                for _ in 0..n {
                    self.emit(Op::Call(1));
                }
                self.define_named(&name)
            }
            _ => Err(self.error("expected 'def' or 'class' after a decorator")),
        }
    }

    /// `yield value`, `yield` (None) or `yield from iterable`; leaves the value sent
    /// back in (None for plain iteration).
    fn yield_expr(&mut self) -> Res<()> {
        self.advance(); // yield
        if self.cur().is_script {
            return Err(self.error_prev("'yield' outside of a function"));
        }
        if !self.cur().is_generator {
            return Err(self.error_prev("'yield' can only be used directly inside a def (not in a lambda)"));
        }
        if self.matches(&Tok::From) {
            // for (x in iterable) { yield x; }
            self.expression()?;
            self.begin_scope();
            let iter_slot = self.declare_iter_slots()?;
            let var = self.declare_local("(yield from)", false)?;
            self.emit(Op::GetIter(iter_slot));
            let top = self.label();
            let exit = self.emit(Op::ForIter(iter_slot, var, 0));
            self.emit(Op::GetLocal(var));
            self.emit(Op::Yield);
            self.emit(Op::Pop);
            self.emit(Op::Jump(top as u32));
            self.patch(exit);
            self.end_scope();
            self.emit(Op::None);
            return Ok(());
        }
        if matches!(self.peek(), Tok::Semi | Tok::RParen | Tok::RBracket | Tok::RBrace | Tok::Comma | Tok::Colon) {
            self.emit(Op::None);
        } else {
            self.expr_list()?;
        }
        self.emit(Op::Yield);
        Ok(())
    }

    /// `__exit__(None, None, None)` on the context manager in local `slot`.
    fn emit_with_exit(&mut self, slot: u32) {
        self.emit(Op::GetLocal(slot));
        self.emit(Op::None);
        self.emit(Op::None);
        self.emit(Op::None);
        self.emit(Op::Invoke(sym::D_EXIT, 3));
        self.emit(Op::Pop);
    }

    /// with (open(path) as f, lock) { ... }
    ///
    ///       mgr = EXPR; f = mgr.__enter__()
    ///       SetupTry HANDLER
    ///       body
    ///       PopTry; mgr.__exit__(None, None, None); Jump END
    ///   HANDLER: (exception pushed)
    ///       if (not mgr.__exit__(class, exc, None)) raise exc;
    ///   END:
    fn with_stmt(&mut self) -> Res<()> {
        self.advance(); // with
        self.expect(Tok::LParen, "expected '(' after 'with'")?;
        self.begin_scope();
        let mut guards = Vec::new();
        loop {
            self.expression()?;
            let hidden = format!("(with {})", self.cur().locals.len());
            let slot = self.declare_local(&hidden, false)?;
            self.emit(Op::SetLocal(slot));
            self.emit(Op::GetLocal(slot));
            self.emit(Op::Invoke(sym::D_ENTER, 0));
            if self.matches(&Tok::As) {
                let name = self.expect_ident("expected a name after 'as'")?;
                self.define_named(&name)?;
            } else {
                self.emit(Op::Pop);
            }
            let boundary = self.cur().locals.len();
            let base_stack = self.cur().cur_stack;
            let setup = self.emit(Op::SetupTry(0));
            self.f().active.push(Cleanup::With(slot));
            guards.push((slot, setup, base_stack, boundary));
            if !self.matches(&Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RParen, "expected ')' after with items")?;
        self.scoped_block()?;
        for (slot, setup, base_stack, boundary) in guards.into_iter().rev() {
            self.emit(Op::PopTry);
            self.f().active.pop();
            self.emit_with_exit(slot);
            let done = self.emit(Op::Jump(0));
            // exception path
            self.patch(setup);
            self.handler_entry(base_stack, boundary);
            let exc = self.declare_local("(with exc)", false)?;
            self.emit(Op::SetLocal(exc));
            self.emit(Op::GetLocal(slot));
            let classof = self.globals.slot("$classof");
            self.emit(Op::GetGlobal(classof));
            self.emit(Op::GetLocal(exc));
            self.emit(Op::Call(1));
            self.emit(Op::GetLocal(exc));
            self.emit(Op::None);
            self.emit(Op::Invoke(sym::D_EXIT, 3));
            let swallowed = self.emit(Op::JumpIfTrue(0));
            self.emit(Op::GetLocal(exc));
            self.emit(Op::Raise);
            self.patch(swallowed);
            self.patch(done);
        }
        self.end_scope();
        Ok(())
    }

    /// del x; del d[k]; del xs[1:3]; del obj.attr; (comma-separated)
    fn del_stmt(&mut self) -> Res<()> {
        self.advance(); // del
        loop {
            let plain = match (self.peek(), &self.toks[self.pos + 1].tok) {
                (Tok::Ident(n), Tok::Comma | Tok::Semi) => Some(n.clone()),
                _ => None,
            };
            if let Some(name) = plain {
                self.advance();
                self.del_name(&name)?;
            } else {
                self.del_mode = true;
                let done = self.parse_postfix(false);
                self.del_mode = false;
                if !done? {
                    return Err(self.error_prev(
                        "can only delete names, items (x[i], x[a:b]) and attributes (x.name)",
                    ));
                }
            }
            if !self.matches(&Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::Semi, "expected ';' after del")
    }

    fn del_name(&mut self, name: &str) -> Res<()> {
        let global_decl = self.cur().global_decls.contains(name);
        let fi = self.fns.len() - 1;
        if !global_decl && (self.resolve_local(name).is_some() || self.resolve_upvalue(fi, name).is_some()) {
            return Err(self.error_prev(&format!(
                "can't delete local variable '{}' (assign None to it instead)",
                name
            )));
        }
        let g = self.globals.slot(name);
        if self.globals.is_const[g as usize] {
            return Err(self.error_prev(&format!("cannot delete constant '{}'", name)));
        }
        self.emit(Op::DelGlobal(g));
        Ok(())
    }

    /// `super().name(args)` inside a method: `Base.name(self, args)`.
    fn super_call(&mut self) -> Res<bool> {
        let (is_method, base) = (self.cur().is_method, self.cur().class_base);
        let base = match (is_method, base) {
            (true, Some(b)) => b,
            (true, None) => {
                return Err(self.error_prev("super() needs a base class: class Child(Parent) { ... }"))
            }
            _ => return Err(self.error_prev("super() can only be used directly inside a method")),
        };
        self.expect(Tok::LParen, "expected '(' after super")?;
        self.expect(Tok::RParen, "super() takes no arguments in Faston")?;
        self.expect(Tok::Dot, "expected '.' after super(): super().method(...)")?;
        let mname = self.expect_ident("expected a method name after 'super().'")?;
        // the base-class expression, re-parsed here
        let resume = self.pos;
        self.pos = base.0;
        self.expression()?;
        if self.pos != base.1 {
            return Err(self.error("unexpected token in base class"));
        }
        self.pos = resume;
        let n = self.name_const(&mname);
        self.emit(Op::GetAttr(n));
        if !self.check(&Tok::LParen) {
            return Err(self.error("super() can only call a method: super().name(...)"));
        }
        if self.args_have_splat_after_paren() {
            return Err(self.error("super() calls with * or ** arguments aren't supported yet"));
        }
        self.advance(); // (
        self.emit(Op::GetLocal(0)); // self
        match self.arguments()? {
            CallArgs::Plain(argc) => self.emit(Op::Call(argc + 1)),
            CallArgs::Keywords(argc, k) => self.emit(Op::CallKw(argc + 1, k)),
            CallArgs::Splat => unreachable!(),
        };
        Ok(false)
    }

    fn block(&mut self) -> Res<()> {
        self.expect(Tok::LBrace, "expected '{'")?;
        while !self.check(&Tok::RBrace) && !self.check(&Tok::Eof) {
            self.statement()?;
        }
        self.expect(Tok::RBrace, "expected '}' to close block")
    }

    fn scoped_block(&mut self) -> Res<()> {
        self.begin_scope();
        self.block()?;
        self.end_scope();
        Ok(())
    }

    fn paren_condition(&mut self, kw: &str) -> Res<()> {
        self.expect(Tok::LParen, &format!("expected '(' after '{}'", kw))?;
        self.expression()?;
        self.expect(Tok::RParen, &format!("expected ')' after {} condition", kw))
    }

    fn if_stmt(&mut self) -> Res<()> {
        self.advance();
        self.paren_condition("if")?;
        let mut jf = Some(self.emit(Op::JumpIfFalse(0)));
        self.scoped_block()?;
        let mut end_jumps = Vec::new();
        loop {
            if self.matches(&Tok::Elif) {
                end_jumps.push(self.emit(Op::Jump(0)));
                self.patch(jf.take().unwrap());
                self.paren_condition("elif")?;
                jf = Some(self.emit(Op::JumpIfFalse(0)));
                self.scoped_block()?;
            } else if self.matches(&Tok::Else) {
                end_jumps.push(self.emit(Op::Jump(0)));
                self.patch(jf.take().unwrap());
                if self.check(&Tok::If) {
                    self.if_stmt()?; // `else if (...)`
                } else {
                    self.scoped_block()?;
                }
                break;
            } else {
                break;
            }
        }
        if let Some(j) = jf {
            self.patch(j);
        }
        for j in end_jumps {
            self.patch(j);
        }
        Ok(())
    }

    fn while_stmt(&mut self) -> Res<()> {
        self.advance();
        let start = self.label();
        self.paren_condition("while")?;
        let exit = self.emit(Op::JumpIfFalse(0));
        self.push_loop(start);
        self.scoped_block()?;
        self.emit(Op::Jump(start as u32));
        self.patch(exit);
        self.finish_loop(start);
        Ok(())
    }

    fn push_loop(&mut self, start: usize) {
        let boundary = self.cur().locals.len();
        let active_depth = self.cur().active.len();
        self.f().loops.push(LoopCtx {
            continue_target: start,
            breaks: Vec::new(),
            continues: Vec::new(),
            boundary,
            captured: false,
            active_depth,
        });
    }

    /// Patches `break`s to the current position. If the loop body has captured
    /// variables, `break`/`continue` go through small stubs that close them first.
    fn finish_loop(&mut self, start: usize) {
        let ctx = self.f().loops.pop().unwrap();
        if !ctx.captured || (ctx.breaks.is_empty() && ctx.continues.is_empty()) {
            for b in ctx.breaks {
                self.patch(b);
            }
            return;
        }
        let skip = self.emit(Op::Jump(0));
        let mut to_end = vec![skip];
        let retarget = |c: &mut Self, sites: &[usize], target: usize| {
            for &site in sites {
                let op = &mut c.f().code[site];
                *op = op.with_target(target as u32);
            }
        };
        if !ctx.breaks.is_empty() {
            let stub = self.label();
            retarget(self, &ctx.breaks, stub);
            self.emit(Op::CloseUpvals(ctx.boundary as u32));
            to_end.push(self.emit(Op::Jump(0)));
        }
        if !ctx.continues.is_empty() {
            let stub = self.label();
            retarget(self, &ctx.continues, stub);
            self.emit(Op::CloseUpvals(ctx.boundary as u32));
            self.emit(Op::Jump(start as u32));
        }
        for j in to_end {
            self.patch(j);
        }
    }

    fn for_stmt(&mut self) -> Res<()> {
        self.advance();
        self.expect(Tok::LParen, "expected '(' after 'for'")?;
        let names = self.loop_targets()?;
        self.expect(Tok::In, "expected 'in' after loop variable")?;
        self.begin_scope();
        self.expression()?;
        self.expect(Tok::RParen, "expected ')' after for clause")?;
        let iter_slot = self.declare_iter_slots()?;
        self.emit(Op::GetIter(iter_slot));
        let (var_slot, unpack) = self.declare_loop_targets(&names)?;

        // Inverted loop: test once on entry, then `ForLoop` at the bottom advances
        // and jumps back, so each iteration costs one dispatch instead of two.
        let exit = self.emit(Op::ForIter(iter_slot, var_slot, 0));
        let body = self.label();
        self.emit_unpack_targets(var_slot, &unpack);
        self.push_loop(usize::MAX); // `continue` target is the ForLoop, patched below
        self.scoped_block()?;
        let cont = self.label();
        let continues = self.cur().loops.last().unwrap().continues.clone();
        for c in continues {
            let op = &mut self.f().code[c];
            *op = op.with_target(cont as u32);
        }
        self.f().loops.last_mut().unwrap().continue_target = cont;
        self.emit(Op::ForLoop(iter_slot, var_slot, body as u32));
        self.patch(exit);
        self.finish_loop(cont);
        self.end_scope();
        Ok(())
    }

    fn return_stmt(&mut self) -> Res<()> {
        self.advance();
        if self.cur().is_script {
            return Err(self.error_prev("'return' outside of a function"));
        }
        if self.matches(&Tok::Semi) {
            if self.cur().is_init {
                self.emit(Op::GetLocal(0));
            } else {
                self.emit(Op::None);
            }
        } else {
            if self.cur().is_init {
                return Err(self.error("init() cannot return a value"));
            }
            self.expr_list()?;
            self.expect(Tok::Semi, "expected ';' after return value")?;
        }
        self.unwind_handlers(0)?;
        self.emit(Op::Return);
        Ok(())
    }

    /// Leaves active `try` blocks down to `depth` (for return/break/continue):
    /// pops their handlers and runs their `finally` blocks, innermost first.
    fn unwind_handlers(&mut self, depth: usize) -> Res<()> {
        let saved = self.cur().active.clone();
        while self.cur().active.len() > depth {
            let h = self.f().active.pop().unwrap();
            self.emit(Op::PopTry);
            match h {
                Cleanup::Handler => {}
                Cleanup::Finally(range) => self.block_at(range)?,
                Cleanup::With(slot) => self.emit_with_exit(slot),
            }
        }
        self.f().active = saved;
        Ok(())
    }

    /// Compiles the `{ ... }` block at token range `range` (re-parsing it), then
    /// returns to the current position.
    fn block_at(&mut self, range: (usize, usize)) -> Res<()> {
        let resume = self.pos;
        self.pos = range.0;
        self.scoped_block()?;
        if self.pos != range.1 {
            return Err(self.error("unexpected token in block"));
        }
        self.pos = resume;
        Ok(())
    }

    /// Token index just past the `{ ... }` block starting at `p`.
    fn skip_braces(&self, p: usize) -> Res<usize> {
        self.skip_group(p, Tok::LBrace, Tok::RBrace, "expected '{'")
    }

    fn skip_group(&self, p: usize, open: Tok, close: Tok, msg: &str) -> Res<usize> {
        if self.toks[p].tok != open {
            let at = self.toks[p].line;
            return Err(format!("[line {}] SyntaxError: {}", at, msg));
        }
        let mut depth = 0;
        let mut i = p;
        loop {
            let t = &self.toks[i].tok;
            if *t == open {
                depth += 1;
            } else if *t == close {
                depth -= 1;
                if depth == 0 {
                    return Ok(i + 1);
                }
            } else if *t == Tok::Eof {
                return Err(format!(
                    "[line {}] SyntaxError: unclosed '{}'",
                    self.toks[p].line, open
                ));
            }
            i += 1;
        }
    }

    /// try { A } except (T1, T2 as e) { B } ... else { C } finally { F }
    ///
    ///       [SetupTry FIN_EXC]          finally guard, if any
    ///       [SetupTry HANDLER]          if there are except clauses
    ///       A
    ///       [PopTry] C [PopTry F]       normal exit: else, then finally
    ///       Jump END
    ///   HANDLER: (exception pushed)     match clauses in order; no match: re-raise
    ///   FIN_EXC: (exception pushed)     F, then re-raise
    ///   END:
    fn try_stmt(&mut self) -> Res<()> {
        self.advance(); // try
        // Pre-scan the shape of the statement: the finally (and else) blocks are
        // compiled out of order, so their token ranges are needed up front.
        let body_start = self.pos;
        let mut p = self.skip_braces(body_start)?;
        let clauses_start = p;
        let mut has_except = false;
        while self.toks[p].tok == Tok::Except {
            has_except = true;
            p += 1;
            if self.toks[p].tok == Tok::LParen {
                p = self.skip_group(p, Tok::LParen, Tok::RParen, "expected '('")?;
            }
            p = self.skip_braces(p)?;
        }
        let else_range = if self.toks[p].tok == Tok::Else {
            let s = p + 1;
            p = self.skip_braces(s)?;
            Some((s, p))
        } else {
            None
        };
        let finally = if self.toks[p].tok == Tok::Finally {
            let s = p + 1;
            p = self.skip_braces(s)?;
            Some((s, p))
        } else {
            None
        };
        let stmt_end = p;
        if !has_except && finally.is_none() {
            return Err(self.error_prev("'try' needs at least one 'except' or a 'finally' block"));
        }
        if else_range.is_some() && !has_except {
            return Err(self.error_prev("'else' after 'try' needs an 'except' block"));
        }

        let boundary = self.cur().locals.len();
        let base_stack = self.cur().cur_stack;
        let fin_setup = match finally {
            Some(range) => {
                let j = self.emit(Op::SetupTry(0));
                self.f().active.push(Cleanup::Finally(range));
                Some(j)
            }
            None => None,
        };
        let exc_setup = if has_except {
            let j = self.emit(Op::SetupTry(0));
            self.f().active.push(Cleanup::Handler);
            Some(j)
        } else {
            None
        };

        self.scoped_block()?; // A

        if has_except {
            self.emit(Op::PopTry);
            self.f().active.pop();
        }
        if let Some(range) = else_range {
            self.block_at(range)?;
        }
        if let Some(range) = finally {
            self.emit(Op::PopTry);
            self.f().active.pop();
            self.block_at(range)?;
        }
        let mut end_jumps = vec![self.emit(Op::Jump(0))];

        // ---- except clauses (the VM already popped the HANDLER; the finally guard
        // is still active while they run)
        if let Some(j) = exc_setup {
            self.patch(j);
            self.handler_entry(base_stack, boundary);
            if let Some(range) = finally {
                self.f().active.push(Cleanup::Finally(range));
            }
            self.begin_scope();
            let exc_slot = self.declare_local("(exc)", false)?;
            self.emit(Op::SetLocal(exc_slot));
            self.f().exc_slots.push(exc_slot);
            self.pos = clauses_start;
            let mut saw_bare = false;
            while self.matches(&Tok::Except) {
                if saw_bare {
                    return Err(self.error_prev("a bare 'except' must be the last except clause"));
                }
                let mut bind = None;
                let mut next = None;
                if self.matches(&Tok::LParen) {
                    self.emit(Op::GetLocal(exc_slot));
                    let mut n = 0;
                    loop {
                        self.parse_or(false)?;
                        n += 1;
                        if !self.matches(&Tok::Comma) {
                            break;
                        }
                    }
                    if self.matches(&Tok::As) {
                        bind = Some(self.expect_ident("expected a name after 'as'")?);
                    }
                    self.expect(Tok::RParen, "expected ')' after exception types")?;
                    self.emit(Op::ExcMatch(n));
                    next = Some(self.emit(Op::JumpIfFalse(0)));
                } else {
                    saw_bare = true;
                }
                self.begin_scope();
                if let Some(name) = bind {
                    self.emit(Op::GetLocal(exc_slot));
                    self.define_named(&name)?;
                }
                self.block()?;
                self.end_scope();
                if let Some(range) = finally {
                    self.emit(Op::PopTry);
                    self.f().active.pop();
                    self.block_at(range)?;
                    self.f().active.push(Cleanup::Finally(range));
                }
                end_jumps.push(self.emit(Op::Jump(0)));
                if let Some(j) = next {
                    self.patch(j);
                }
            }
            // No clause matched: re-raise to outer handlers (or the finally guard).
            self.emit(Op::GetLocal(exc_slot));
            self.emit(Op::Raise);
            self.f().exc_slots.pop();
            self.end_scope();
            if finally.is_some() {
                self.f().active.pop();
            }
        }

        // ---- finally on the exception path
        if let (Some(j), Some(range)) = (fin_setup, finally) {
            self.patch(j);
            self.handler_entry(base_stack, boundary);
            self.block_at(range)?;
            self.emit(Op::Raise);
        }

        for j in end_jumps {
            self.patch(j);
        }
        self.f().cur_stack = base_stack;
        self.pos = stmt_end;
        Ok(())
    }

    /// At a handler label the VM has restored the stack and pushed the exception.
    fn handler_entry(&mut self, base_stack: i32, boundary: usize) {
        let f = self.f();
        f.cur_stack = base_stack + 1;
        f.max_stack = f.max_stack.max(f.cur_stack);
        // Variables captured inside the abandoned block must leave their slots.
        self.emit(Op::CloseUpvals(boundary as u32));
    }

    /// `raise ValueError("bad");` / `raise MyError;` / `raise;` (re-raise in except)
    fn raise_stmt(&mut self) -> Res<()> {
        self.advance();
        if self.matches(&Tok::Semi) {
            match self.cur().exc_slots.last() {
                Some(&slot) => {
                    self.emit(Op::GetLocal(slot));
                }
                None => return Err(self.error_prev("bare 'raise' outside of an except block")),
            }
        } else {
            self.expression()?;
            self.expect(Tok::Semi, "expected ';' after raise")?;
        }
        self.emit(Op::Raise);
        Ok(())
    }

    fn break_stmt(&mut self) -> Res<()> {
        self.advance();
        if self.cur().loops.is_empty() {
            return Err(self.error_prev("'break' outside of a loop"));
        }
        self.expect(Tok::Semi, "expected ';' after 'break'")?;
        let depth = self.cur().loops.last().unwrap().active_depth;
        self.unwind_handlers(depth)?;
        let j = self.emit(Op::Jump(0));
        self.f().loops.last_mut().unwrap().breaks.push(j);
        Ok(())
    }

    fn continue_stmt(&mut self) -> Res<()> {
        self.advance();
        let target = match self.cur().loops.last() {
            Some(l) => l.continue_target,
            None => return Err(self.error_prev("'continue' outside of a loop")),
        };
        self.expect(Tok::Semi, "expected ';' after 'continue'")?;
        let depth = self.cur().loops.last().unwrap().active_depth;
        self.unwind_handlers(depth)?;
        // usize::MAX: a `for` loop's continue target isn't emitted yet (patched later)
        let j = self.emit(Op::Jump(if target == usize::MAX { 0 } else { target as u32 }));
        self.f().loops.last_mut().unwrap().continues.push(j);
        Ok(())
    }

    fn expr_stmt(&mut self) -> Res<()> {
        let assigned = self.parse_or(true)?;
        self.expect(Tok::Semi, "expected ';' after statement")?;
        if !assigned {
            self.emit(Op::Pop);
        }
        Ok(())
    }

    // ------------------------------------------------------------ expressions
    // Each level returns `true` if it compiled an assignment (statement-level only).

    /// Full expression, including `a if cond else b`.
    fn expression(&mut self) -> Res<()> {
        if self.check(&Tok::Yield) {
            return self.yield_expr();
        }
        let start_pos = self.pos;
        let prev_label = self.cur().last_label;
        let code_len = self.label(); // lets us discard `a` if it turns out to be a ternary
        let stack_before = self.cur().cur_stack;
        self.parse_or(false)?;
        if !self.check(&Tok::If) {
            if self.cur().last_label == code_len {
                self.f().last_label = prev_label;
            }
            return Ok(());
        }
        // `a if c else b`: `a` was compiled first, but `c` must run first.
        let f = self.f();
        f.code.truncate(code_len);
        f.lines.truncate(code_len);
        f.cur_stack = stack_before;
        f.last_label = code_len;
        self.advance(); // if
        self.parse_or(false)?;
        let jf = self.emit(Op::JumpIfFalse(0));
        self.expect(Tok::Else, "expected 'else' in conditional expression")?;
        let else_pos = self.pos;
        self.pos = start_pos;
        self.parse_or(false)?;
        let jend = self.emit(Op::Jump(0));
        self.patch(jf);
        self.pos = else_pos;
        self.expression()?;
        self.patch(jend);
        self.f().cur_stack -= 1; // only one branch's value is on the stack
        Ok(())
    }

    fn parse_or(&mut self, can_assign: bool) -> Res<bool> {
        if self.parse_and(can_assign)? {
            return Ok(true);
        }
        while self.matches(&Tok::Or) {
            let j = self.emit(Op::JumpIfTrueOrPop(0));
            self.parse_and(false)?;
            self.patch(j);
        }
        Ok(false)
    }

    fn parse_and(&mut self, can_assign: bool) -> Res<bool> {
        if self.parse_not(can_assign)? {
            return Ok(true);
        }
        while self.matches(&Tok::And) {
            let j = self.emit(Op::JumpIfFalseOrPop(0));
            self.parse_not(false)?;
            self.patch(j);
        }
        Ok(false)
    }

    fn parse_not(&mut self, can_assign: bool) -> Res<bool> {
        if self.matches(&Tok::Not) {
            self.parse_not(false)?;
            self.emit(Op::Not);
            return Ok(false);
        }
        self.parse_comparison(can_assign)
    }

    fn comparison_op(&mut self) -> Option<(Op, bool)> {
        let r = match self.peek() {
            Tok::EqEq => (Op::Eq, false),
            Tok::NotEq => (Op::Ne, false),
            Tok::Lt => (Op::Lt, false),
            Tok::Le => (Op::Le, false),
            Tok::Gt => (Op::Gt, false),
            Tok::Ge => (Op::Ge, false),
            Tok::In => (Op::In, false),
            Tok::Not if self.toks[self.pos + 1].tok == Tok::In => {
                self.advance();
                (Op::In, true)
            }
            Tok::Is if self.toks[self.pos + 1].tok == Tok::Not => {
                self.advance();
                (Op::Is, true)
            }
            Tok::Is => (Op::Is, false),
            _ => return None,
        };
        self.advance();
        Some(r)
    }

    /// Comparisons chain like Python: `a < b < c` means `a < b and b < c`
    /// with `b` evaluated once.
    fn parse_comparison(&mut self, can_assign: bool) -> Res<bool> {
        if self.parse_bitor(can_assign)? {
            return Ok(true);
        }
        let Some((mut op, mut negate)) = self.comparison_op() else {
            return Ok(false);
        };
        let mut cleanups = Vec::new();
        loop {
            self.parse_bitor(false)?;
            let is_chain = matches!(
                self.peek(),
                Tok::EqEq | Tok::NotEq | Tok::Lt | Tok::Le | Tok::Gt | Tok::Ge | Tok::In | Tok::Is
            ) || (self.check(&Tok::Not) && self.toks[self.pos + 1].tok == Tok::In);
            if is_chain {
                // [a, b] -> [b, a, b] -> [b, a<b]; stop early (keeping b below) if false.
                self.emit(Op::Dup);
                self.emit(Op::Rot3);
            }
            self.emit(op);
            if negate {
                self.emit(Op::Not);
            }
            if !is_chain {
                break;
            }
            cleanups.push(self.emit(Op::JumpIfFalseOrPop(0)));
            let next = self.comparison_op().unwrap();
            op = next.0;
            negate = next.1;
        }
        if !cleanups.is_empty() {
            let end = self.emit(Op::Jump(0));
            for c in cleanups {
                self.patch(c);
            }
            // A link failed: stack is [b, False] -> [False].
            self.emit(Op::Swap);
            self.emit(Op::Pop);
            self.patch(end);
            self.f().cur_stack += 1; // the cleanup path's Pop balanced a value already counted
        }
        Ok(false)
    }

    /// One left-associative binary precedence level: `next (op next)*`.
    fn binary_level(
        &mut self,
        can_assign: bool,
        next: fn(&mut Self, bool) -> Res<bool>,
        op_for: fn(&Tok) -> Option<Op>,
    ) -> Res<bool> {
        if next(self, can_assign)? {
            return Ok(true);
        }
        while let Some(op) = op_for(self.peek()) {
            self.advance();
            next(self, false)?;
            self.emit(op);
        }
        Ok(false)
    }

    fn parse_bitor(&mut self, can_assign: bool) -> Res<bool> {
        self.binary_level(can_assign, Self::parse_bitxor, |t| (*t == Tok::Pipe).then_some(Op::BitOr))
    }

    fn parse_bitxor(&mut self, can_assign: bool) -> Res<bool> {
        self.binary_level(can_assign, Self::parse_bitand, |t| (*t == Tok::Caret).then_some(Op::BitXor))
    }

    fn parse_bitand(&mut self, can_assign: bool) -> Res<bool> {
        self.binary_level(can_assign, Self::parse_shift, |t| (*t == Tok::Amp).then_some(Op::BitAnd))
    }

    fn parse_shift(&mut self, can_assign: bool) -> Res<bool> {
        self.binary_level(can_assign, Self::parse_additive, |t| match t {
            Tok::Shl => Some(Op::Shl),
            Tok::Shr => Some(Op::Shr),
            _ => None,
        })
    }

    fn parse_additive(&mut self, can_assign: bool) -> Res<bool> {
        if self.parse_multiplicative(can_assign)? {
            return Ok(true);
        }
        loop {
            let op = match self.peek() {
                Tok::Plus => Op::Add,
                Tok::Minus => Op::Sub,
                _ => break,
            };
            self.advance();
            self.parse_multiplicative(false)?;
            self.emit(op);
        }
        Ok(false)
    }

    fn parse_multiplicative(&mut self, can_assign: bool) -> Res<bool> {
        if self.parse_unary(can_assign)? {
            return Ok(true);
        }
        loop {
            let op = match self.peek() {
                Tok::Star => Op::Mul,
                Tok::Slash => Op::Div,
                Tok::SlashSlash => Op::FloorDiv,
                Tok::Percent => Op::Mod,
                _ => break,
            };
            self.advance();
            self.parse_unary(false)?;
            self.emit(op);
        }
        Ok(false)
    }

    fn parse_unary(&mut self, can_assign: bool) -> Res<bool> {
        if self.matches(&Tok::Minus) {
            self.parse_unary(false)?;
            self.emit(Op::Neg);
            return Ok(false);
        }
        if self.matches(&Tok::Plus) {
            self.parse_unary(false)?;
            return Ok(false);
        }
        if self.matches(&Tok::Tilde) {
            self.parse_unary(false)?;
            self.emit(Op::Invert);
            return Ok(false);
        }
        self.parse_power(can_assign)
    }

    fn parse_power(&mut self, can_assign: bool) -> Res<bool> {
        if self.parse_postfix(can_assign)? {
            return Ok(true);
        }
        if self.matches(&Tok::StarStar) {
            self.parse_unary(false)?; // right-associative, binds tighter than unary on the left
            self.emit(Op::Pow);
        }
        Ok(false)
    }

    fn parse_postfix(&mut self, can_assign: bool) -> Res<bool> {
        // a `del` target: only this level's last accessor deletes (nested expressions read)
        let del = std::mem::replace(&mut self.del_mode, false);
        if self.primary(can_assign)? {
            return Ok(true);
        }
        let del_end = |c: &Self| del && matches!(c.peek(), Tok::Comma | Tok::Semi);
        loop {
            match self.peek() {
                Tok::LParen => {
                    self.advance();
                    match self.arguments()? {
                        CallArgs::Plain(n) => self.emit(Op::Call(n)),
                        CallArgs::Keywords(n, k) => self.emit(Op::CallKw(n, k)),
                        CallArgs::Splat => self.emit(Op::CallEx),
                    };
                }
                Tok::Dot => {
                    self.advance();
                    let name = self.expect_ident("expected attribute name after '.'")?;
                    let n = self.name_const(&name);
                    if self.check(&Tok::LParen) {
                        if self.args_have_splat_after_paren() {
                            // obj.m(*xs): look the method up, then a general call
                            self.emit(Op::GetAttr(n));
                            self.advance();
                            self.splat_arguments()?;
                            self.emit(Op::CallEx);
                        } else {
                            self.advance();
                            match self.arguments()? {
                                CallArgs::Plain(argc) => self.emit(Op::Invoke(n, argc)),
                                CallArgs::Keywords(argc, k) => self.emit(Op::InvokeKw(n, argc, k)),
                                CallArgs::Splat => unreachable!(),
                            };
                        }
                    } else if let (true, Some(kind)) = (can_assign, assign_kind(self.peek())) {
                        self.advance();
                        // `local.attr = v`: drop the GetLocal and store straight into the local's object.
                        let local_obj = match self.cur().code.last() {
                            Some(&Op::GetLocal(a)) if self.cur().last_label != self.here() - 1 => Some(a),
                            _ => None,
                        };
                        if let Some(a) = local_obj {
                            let f = self.f();
                            f.code.pop();
                            f.lines.pop();
                            f.cur_stack -= 1;
                            if let Some(op) = kind {
                                self.emit(Op::GetLocalAttr(a, n));
                                self.expression()?;
                                self.emit(op);
                            } else {
                                self.expression()?;
                            }
                            self.emit(Op::SetLocalAttr(a, n));
                            return Ok(true);
                        }
                        if let Some(op) = kind {
                            self.emit(Op::Dup);
                            self.emit(Op::GetAttr(n));
                            self.expression()?;
                            self.emit(op);
                        } else {
                            self.expression()?;
                        }
                        self.emit(Op::SetAttr(n));
                        return Ok(true);
                    } else if del_end(self) {
                        self.emit(Op::DelAttr(n));
                        return Ok(true);
                    } else {
                        self.emit(Op::GetAttr(n));
                    }
                }
                Tok::LBracket => {
                    self.advance();
                    if self.check(&Tok::Colon) {
                        self.emit(Op::None);
                    } else {
                        self.expression()?;
                    }
                    if self.matches(&Tok::Colon) {
                        // slice: obj[start:stop:step]
                        let part = |c: &mut Self| -> Res<()> {
                            if c.check(&Tok::Colon) || c.check(&Tok::RBracket) {
                                c.emit(Op::None);
                                Ok(())
                            } else {
                                c.expression()
                            }
                        };
                        part(self)?;
                        if self.matches(&Tok::Colon) {
                            part(self)?;
                        } else {
                            self.emit(Op::None);
                        }
                        self.expect(Tok::RBracket, "expected ']' after slice")?;
                        if del_end(self) {
                            self.emit(Op::DelSlice);
                            return Ok(true);
                        }
                        if can_assign && self.check(&Tok::Assign) {
                            self.advance();
                            self.expression()?;
                            self.emit(Op::SetSlice);
                            return Ok(true);
                        }
                        if can_assign && assign_kind(self.peek()).is_some() {
                            return Err(self.error("augmented assignment to a slice isn't supported"));
                        }
                        self.emit(Op::GetSlice);
                        continue;
                    }
                    self.expect(Tok::RBracket, "expected ']' after index")?;
                    if del_end(self) {
                        self.emit(Op::DelIndex);
                        return Ok(true);
                    }
                    if let (true, Some(kind)) = (can_assign, assign_kind(self.peek())) {
                        self.advance();
                        if let Some(op) = kind {
                            self.emit(Op::Dup2);
                            self.emit(Op::GetIndex);
                            self.expression()?;
                            self.emit(op);
                        } else {
                            self.expression()?;
                        }
                        self.emit(Op::SetIndex);
                        return Ok(true);
                    }
                    self.emit(Op::GetIndex);
                }
                _ => break,
            }
        }
        Ok(false)
    }

    /// Parses call arguments after '(' up to and including ')'. Plain positional
    /// calls push their arguments; keyword calls push positional values then keyword
    /// values; calls with `*xs` / `**d` build a list and a dict instead.
    fn arguments(&mut self) -> Res<CallArgs> {
        if self.args_have_splat() {
            return self.splat_arguments();
        }
        let mut n = 0u32;
        let mut names: Vec<u32> = Vec::new();
        // f(x for x in xs): a generator expression argument, evaluated eagerly as a list
        if !self.check(&Tok::RParen) && self.paren_has_for() {
            let elem_pos = self.pos;
            let code_len = self.label();
            let stack_before = self.cur().cur_stack;
            self.expression()?;
            if self.check(&Tok::For) {
                self.discard_since(code_len, stack_before);
                self.comprehension(elem_pos, Comp::List, Tok::RParen)?;
                return Ok(CallArgs::Plain(1));
            }
            self.discard_since(code_len, stack_before);
            self.pos = elem_pos;
        }
        if !self.check(&Tok::RParen) {
            loop {
                let is_kw = matches!(self.peek(), Tok::Ident(_)) && self.toks[self.pos + 1].tok == Tok::Assign;
                if is_kw {
                    let name = self.expect_ident("expected argument name")?;
                    let sym = self.name_const(&name);
                    if names.contains(&sym) {
                        return Err(self.error_prev(&format!("keyword argument repeated: {}", name)));
                    }
                    self.advance(); // =
                    self.expression()?;
                    names.push(sym);
                } else {
                    if !names.is_empty() {
                        return Err(self.error("positional argument follows keyword argument"));
                    }
                    self.expression()?;
                }
                n += 1;
                if !self.matches(&Tok::Comma) || self.check(&Tok::RParen) {
                    break;
                }
            }
        }
        self.expect(Tok::RParen, "expected ')' after arguments")?;
        if names.is_empty() {
            return Ok(CallArgs::Plain(n));
        }
        let t = self.heap.tuple(names.iter().map(|&s| Value::Int(s as i64)).collect());
        let k = self.add_const(t);
        Ok(CallArgs::Keywords(n, k))
    }

    /// Is there a `for` at the top level of this argument list (cursor after '(')?
    fn paren_has_for(&self) -> bool {
        let mut depth = 0;
        let mut i = self.pos;
        loop {
            match &self.toks[i].tok {
                Tok::LParen | Tok::LBracket | Tok::LBrace => depth += 1,
                Tok::RParen | Tok::RBracket | Tok::RBrace => {
                    if depth == 0 {
                        return false;
                    }
                    depth -= 1;
                }
                Tok::For if depth == 0 => return true,
                Tok::Eof | Tok::Semi => return false,
                _ => {}
            }
            i += 1;
        }
    }

    fn args_have_splat_after_paren(&mut self) -> bool {
        self.pos += 1;
        let r = self.args_have_splat();
        self.pos -= 1;
        r
    }

    /// Does the argument list (cursor after '(') contain `*x` or `**x`?
    fn args_have_splat(&self) -> bool {
        let mut depth = 0;
        let mut i = self.pos;
        let mut at_start = true;
        loop {
            match &self.toks[i].tok {
                Tok::LParen | Tok::LBracket | Tok::LBrace => {
                    depth += 1;
                    at_start = false;
                }
                Tok::RParen | Tok::RBracket | Tok::RBrace => {
                    if depth == 0 {
                        return false;
                    }
                    depth -= 1;
                }
                Tok::Comma if depth == 0 => {
                    at_start = true;
                    i += 1;
                    continue;
                }
                Tok::Star | Tok::StarStar if depth == 0 && at_start => return true,
                Tok::Eof | Tok::Semi => return false,
                _ => at_start = false,
            }
            i += 1;
        }
    }

    /// f(a, *xs, k=v, **d)  ->  [callee, list, dict] for `CallEx`
    fn splat_arguments(&mut self) -> Res<CallArgs> {
        self.emit(Op::BuildList(0));
        let mut dict_started = false;
        while !self.check(&Tok::RParen) {
            let is_kw = matches!(self.peek(), Tok::Ident(_)) && self.toks[self.pos + 1].tok == Tok::Assign;
            if self.matches(&Tok::StarStar) || is_kw {
                if !dict_started {
                    self.emit(Op::BuildDict(0));
                    dict_started = true;
                }
                if is_kw {
                    let name = self.expect_ident("expected argument name")?;
                    self.advance(); // =
                    let key = self.heap.string(name);
                    let k = self.add_const(key);
                    self.emit(Op::Const(k));
                    self.expression()?;
                    self.emit(Op::ArgKw);
                } else {
                    self.expression()?;
                    self.emit(Op::ArgMerge);
                }
            } else {
                if dict_started {
                    return Err(self.error("positional argument follows keyword argument"));
                }
                if self.matches(&Tok::Star) {
                    self.expression()?;
                    self.emit(Op::ArgExtend);
                } else {
                    self.expression()?;
                    self.emit(Op::ArgPush);
                }
            }
            if !self.matches(&Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RParen, "expected ')' after arguments")?;
        if !dict_started {
            self.emit(Op::BuildDict(0));
        }
        Ok(CallArgs::Splat)
    }

    fn primary(&mut self, can_assign: bool) -> Res<bool> {
        let tok = self.advance();
        match tok {
            Tok::Int(i) => {
                let c = self.add_const(Value::Int(i));
                self.emit(Op::Const(c));
            }
            Tok::Float(x) => {
                let c = self.add_const(Value::Float(x));
                self.emit(Op::Const(c));
            }
            Tok::Str(s) => {
                let v = self.heap.string(s);
                let c = self.add_const(v);
                self.emit(Op::Const(c));
            }
            Tok::Bytes(b) => {
                let v = self.heap.bytes(b);
                let c = self.add_const(v);
                self.emit(Op::Const(c));
            }
            Tok::True => {
                self.emit(Op::True);
            }
            Tok::False => {
                self.emit(Op::False);
            }
            Tok::None => {
                self.emit(Op::None);
            }
            Tok::Ident(name) => return self.variable(name, can_assign),
            Tok::LParen => {
                // (expr), () or a tuple: (a,) (a, b, ...)
                if self.matches(&Tok::RParen) {
                    self.emit(Op::BuildTuple(0));
                    return Ok(false);
                }
                self.expression()?;
                if self.matches(&Tok::Comma) {
                    let mut n = 1u32;
                    while !self.check(&Tok::RParen) {
                        self.expression()?;
                        n += 1;
                        if !self.matches(&Tok::Comma) {
                            break;
                        }
                    }
                    self.expect(Tok::RParen, "expected ')' after tuple items")?;
                    self.emit(Op::BuildTuple(n));
                } else {
                    self.expect(Tok::RParen, "expected ')' after expression")?;
                }
            }
            Tok::LBracket => self.list_literal()?,
            Tok::LBrace => self.dict_literal()?,
            Tok::Lambda => self.lambda()?,
            _ => {
                self.pos -= 1;
                return Err(self.error("expected an expression"));
            }
        }
        Ok(false)
    }

    fn variable(&mut self, name: String, can_assign: bool) -> Res<bool> {
        if name == "super" && self.check(&Tok::LParen) && self.resolve_local("super").is_none() {
            return self.super_call();
        }
        let global_decl = self.cur().global_decls.contains(&name);
        let local = if global_decl {
            None
        } else {
            self.resolve_local(&name)
        };

        if can_assign {
            if let Some(kind) = assign_kind(self.peek()) {
                self.advance();
                if local.is_none() {
                    if let Some((u, is_const)) = self.nonlocal_upvalue(&name)? {
                        if is_const {
                            return Err(self.const_err(&name));
                        }
                        if let Some(op) = kind {
                            self.emit(Op::GetUpval(u));
                            self.expression()?;
                            self.emit(op);
                        } else {
                            self.expression()?;
                        }
                        self.emit(Op::SetUpval(u));
                        return Ok(true);
                    }
                }
                match local {
                    Some((slot, is_const)) => {
                        if is_const {
                            return Err(self.const_err(&name));
                        }
                        match kind {
                            Some(Op::Add) => {
                                self.expression()?;
                                self.emit(Op::AddLocal(slot));
                            }
                            Some(op) => {
                                self.emit(Op::GetLocal(slot));
                                self.expression()?;
                                self.emit(op);
                                self.emit(Op::SetLocal(slot));
                            }
                            None => {
                                self.expression()?;
                                self.emit(Op::SetLocal(slot));
                            }
                        }
                    }
                    None if self.cur().is_script || global_decl => {
                        let g = self.globals.slot(&name);
                        if self.globals.is_const[g as usize] {
                            return Err(self.const_err(&name));
                        }
                        if let Some(op) = kind {
                            self.emit(Op::GetGlobal(g));
                            self.expression()?;
                            self.emit(op);
                        } else {
                            self.expression()?;
                        }
                        self.emit(Op::SetGlobal(g));
                    }
                    None => {
                        // Inside a function: plain assignment creates a local.
                        if kind.is_some() {
                            let fi = self.fns.len() - 1;
                            let hint = if self.resolve_upvalue(fi, &name).is_some() {
                                format!("add 'nonlocal {};' to modify the enclosing function's variable", name)
                            } else {
                                format!("add 'global {};' to modify the global", name)
                            };
                            return Err(self.error_prev(&format!(
                                "'{}' is not a local variable; {}",
                                name, hint
                            )));
                        }
                        self.expression()?;
                        let s = self.declare_local(&name, false)?;
                        self.emit(Op::SetLocal(s));
                    }
                }
                return Ok(true);
            }
        }

        if let Some((slot, _)) = local {
            self.emit(Op::GetLocal(slot));
            return Ok(false);
        }
        if !global_decl {
            let fi = self.fns.len() - 1;
            if let Some((u, _)) = self.resolve_upvalue(fi, &name) {
                self.emit(Op::GetUpval(u));
                return Ok(false);
            }
        }
        let g = self.globals.slot(&name);
        self.emit(Op::GetGlobal(g));
        Ok(false)
    }

    fn list_literal(&mut self) -> Res<()> {
        if self.matches(&Tok::RBracket) {
            self.emit(Op::BuildList(0));
            return Ok(());
        }
        let elem_pos = self.pos;
        let code_len = self.label(); // stops the trial compile fusing into earlier code
        let stack_before = self.cur().cur_stack;
        self.expression()?;
        if self.check(&Tok::For) {
            // Comprehension: the element must be emitted inside the loop, so throw
            // away the trial compile and re-parse it once the loop is set up.
            let f = self.f();
            f.code.truncate(code_len);
            f.lines.truncate(code_len);
            f.cur_stack = stack_before;
            f.last_label = code_len;
            return self.comprehension(elem_pos, Comp::List, Tok::RBracket);
        }
        let mut n = 1u32;
        while self.matches(&Tok::Comma) {
            if self.check(&Tok::RBracket) {
                break;
            }
            self.expression()?;
            n += 1;
        }
        self.expect(Tok::RBracket, "expected ']' after list items")?;
        self.emit(Op::BuildList(n));
        Ok(())
    }

    /// Throws away code emitted since `code_len` (a trial compile of a comprehension's
    /// element, which must instead be compiled inside the loop).
    fn discard_since(&mut self, code_len: usize, stack_before: i32) {
        let f = self.f();
        f.code.truncate(code_len);
        f.lines.truncate(code_len);
        f.cur_stack = stack_before;
        f.last_label = code_len;
    }

    /// `[elem for x in iterable if cond]` (also `{k: v for ...}`, `{x for ...}` and
    /// `f(x for ...)`), with the cursor on `for`. `close` ends it.
    fn comprehension(&mut self, elem_pos: usize, kind: Comp, close: Tok) -> Res<()> {
        self.advance(); // for
        let names = self.loop_targets()?;
        self.expect(Tok::In, "expected 'in' in comprehension")?;
        self.begin_scope();

        let list_slot = self.declare_local("(list)", false)?;
        match kind {
            Comp::List => self.emit(Op::BuildList(0)),
            Comp::Set => self.emit(Op::BuildSet(0)),
            Comp::Dict => self.emit(Op::BuildDict(0)),
        };
        self.emit(Op::SetLocal(list_slot));

        self.parse_or(false)?; // no conditional expression here: `if` starts the filter
        let iter_slot = self.declare_iter_slots()?;
        self.emit(Op::GetIter(iter_slot));
        let (var_slot, unpack) = self.declare_loop_targets(&names)?;

        let exit = self.emit(Op::ForIter(iter_slot, var_slot, 0));
        let body = self.label();
        self.emit_unpack_targets(var_slot, &unpack);
        let mut skip = None;
        if self.matches(&Tok::If) {
            self.parse_or(false)?;
            skip = Some(self.emit(Op::JumpIfFalse(0)));
        }
        self.expect(close, "expected the end of the comprehension")?;
        let end_pos = self.pos;

        self.pos = elem_pos;
        self.expression()?;
        match kind {
            Comp::List => self.emit(Op::ListAppend(list_slot)),
            Comp::Set => self.emit(Op::SetAdd(list_slot)),
            Comp::Dict => {
                self.expect(Tok::Colon, "expected ':' in dict comprehension")?;
                self.expression()?;
                self.emit(Op::DictSet(list_slot))
            }
        };
        match skip {
            Some(j) => self.patch(j),
            None => {
                self.label();
            }
        }
        self.emit(Op::ForLoop(iter_slot, var_slot, body as u32));
        self.patch(exit);
        self.emit(Op::GetLocal(list_slot));

        self.pos = end_pos;
        self.end_scope();
        Ok(())
    }

    /// `{k: v, ...}` (dict, including `{}`) or `{a, b, ...}` (set).
    fn dict_literal(&mut self) -> Res<()> {
        let mut n = 0u32;
        if !self.check(&Tok::RBrace) {
            let elem_pos = self.pos;
            let code_len = self.label();
            let stack_before = self.cur().cur_stack;
            self.expression()?;
            if self.check(&Tok::For) {
                self.discard_since(code_len, stack_before);
                return self.comprehension(elem_pos, Comp::Set, Tok::RBrace);
            }
            if !self.check(&Tok::Colon) {
                n = 1;
                while self.matches(&Tok::Comma) {
                    if self.check(&Tok::RBrace) {
                        break;
                    }
                    self.expression()?;
                    n += 1;
                }
                self.expect(Tok::RBrace, "expected '}' after set items")?;
                self.emit(Op::BuildSet(n));
                return Ok(());
            }
            self.expect(Tok::Colon, "expected ':' between dict key and value")?;
            self.expression()?;
            if self.check(&Tok::For) {
                self.discard_since(code_len, stack_before);
                return self.comprehension(elem_pos, Comp::Dict, Tok::RBrace);
            }
            n = 1;
            if !self.matches(&Tok::Comma) || self.check(&Tok::RBrace) {
                self.expect(Tok::RBrace, "expected '}' after dict items")?;
                self.emit(Op::BuildDict(n));
                return Ok(());
            }
            loop {
                self.expression()?;
                self.expect(Tok::Colon, "expected ':' between dict key and value")?;
                self.expression()?;
                n += 1;
                if !self.matches(&Tok::Comma) || self.check(&Tok::RBrace) {
                    break;
                }
            }
        }
        self.expect(Tok::RBrace, "expected '}' after dict items")?;
        self.emit(Op::BuildDict(n));
        Ok(())
    }
}
