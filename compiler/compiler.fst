# The Faston compiler, in Faston: tokens -> bytecode (.fbc), single pass, no
# AST. A port of the original Rust compiler (stage 0): for the same source it
# produces the same bytes.
#
# Scoping rules:
# * Top level of a script: let/const/assignment/def/class create globals.
# * Blocks (if, while, for, ...) are block-scoped: `let` inside them is local.
# * Inside a function, plain assignment to an unknown name creates a local
#   (like Python); `global name;` / `nonlocal name;` change that.
# * Nested functions capture enclosing locals as upvalues (Lua-style).

import bytecode;
from lexer import lex, token_text, CompileError;
from ops import fuse, jump_target, with_target;

class Local {
    def init(self, name, depth, is_const) {
        self.name = name;
        self.depth = depth;
        self.is_const = is_const;
        self.captured = False;
    }
}

class LoopCtx {
    def init(self, continue_target, boundary, active_depth) {
        self.continue_target = continue_target;
        self.breaks = [];
        self.continues = [];
        self.boundary = boundary;
        self.captured = False;
        self.active_depth = active_depth;
    }
}

# Keyword-argument names of a call (a constant).
class KwNames {
    def init(self, names) { self.names = names; }
}

# A compiled function.
class FnProto {
    def init(self, name, arity, defaults, code, lines, consts, num_locals, captures, params, varargs, varkw) {
        self.name = name;
        self.arity = arity;
        self.defaults = defaults;
        self.code = code;
        self.lines = lines;
        self.consts = consts;
        self.num_locals = num_locals;
        self.captures = captures;
        self.params = params;
        self.varargs = varargs;
        self.varkw = varkw;
        self.dynamic = False;
    }
}

class FnState {
    def init(self, name, is_script, is_init) {
        self.name = name;
        self.code = [];
        self.lines = [];
        self.consts = [];
        self.locals = [];
        self.depth = 0;
        self.max_slots = 0;
        self.last_label = -1;
        self.is_script = is_script;
        self.is_init = is_init;
        self.loops = [];
        self.global_decls = set();
        self.nonlocal_decls = set();
        self.upvalues = [];
        self.active = [];
        self.exc_slots = [];
        self.param_syms = [];
        self.varargs = False;
        self.varkw = False;
        self.is_generator = False;
        self.is_method = False;
        self.class_base = None;
    }

    # Jump threading for `and` / `or` used as conditions.
    def thread_jumps(self) {
        let code = self.code;
        for (round in range(4)) {
            let changed = False;
            for (i in range(len(code))) {
                let op = code[i];
                let new = None;
                if (op[0] == "JumpIfTrueOrPop") {
                    let t = op[1];
                    if (t < len(code)) {
                        let o = code[t];
                        if (o[0] == "JumpIfFalse") { new = ("JumpIfTrue", t + 1); }
                        elif (o[0] == "JumpIfTrueOrPop") { new = ("JumpIfTrueOrPop", o[1]); }
                    }
                } elif (op[0] == "JumpIfFalseOrPop") {
                    let t = op[1];
                    if (t < len(code)) {
                        let o = code[t];
                        if (o[0] == "JumpIfFalse") { new = ("JumpIfFalse", o[1]); }
                        elif (o[0] == "JumpIfFalseOrPop") { new = ("JumpIfFalseOrPop", o[1]); }
                    }
                }
                if (new != None) {
                    code[i] = new;
                    changed = True;
                }
            }
            if (not changed) { break; }
        }
    }

    # Fuses pairs that are no longer split by a jump target, compacting the code.
    def refuse(self) {
        let code = self.code;
        let n = len(code);
        let is_target = [False] * (n + 1);
        for (op in code) {
            let t = jump_target(op);
            if (t != None) { is_target[t] = True; }
        }
        let keep = [True] * n;
        let i = 0;
        let any = False;
        while (i + 1 < n) {
            if (not is_target[i + 1]) {
                let f = fuse(code[i], code[i + 1]);
                if (f != None) {
                    code[i] = f;
                    keep[i + 1] = False;
                    any = True;
                    i += 2;
                    continue;
                }
            }
            i += 1;
        }
        if (not any) { return; }
        let mapping = [0] * (n + 1);
        let j = 0;
        for (i in range(n)) {
            mapping[i] = j;
            if (keep[i]) { j += 1; }
        }
        mapping[n] = j;
        let out = [];
        let lines = [];
        for (i in range(n)) {
            if (keep[i]) {
                let op = code[i];
                let t = jump_target(op);
                out.append(op if t == None else with_target(op, mapping[t]));
                lines.append(self.lines[i]);
            }
        }
        self.code = out;
        self.lines = lines;
    }

    def finish(self, arity, defaults) {
        self.thread_jumps();
        self.refuse();
        return FnProto(self.name, arity, defaults, self.code, self.lines, self.consts, self.max_slots,
                       self.upvalues, self.param_syms, self.varargs, self.varkw);
    }
}

const ASSIGN = {"=": None, "+=": "Add", "-=": "Sub", "*=": "Mul", "/=": "Div", "%=": "Mod",
                "&=": "BitAnd", "|=": "BitOr", "^=": "BitXor"};
const COMPARE = {"==": "Eq", "!=": "Ne", "<": "Lt", "<=": "Le", ">": "Gt", ">=": "Ge", "in": "In"};

class Compiler {
    def init(self, toks, consts=None) {
        self.toks = toks;
        self.ntoks = len(toks);
        self.pos = 0;
        self.fns = [FnState("<module>", True, False)];
        # global name -> is it a constant (persists between REPL runs)
        self.gconst = consts if consts != None else {};
        self.pending_defaults = [];
        self.has_pending_method = False;
        self.pending_base = None;
        self.del_mode = False;
    }

    def compile(self) {
        while (not self.check("EOF")) { self.statement(); }
        self.emit(("None",));
        self.emit(("Return",));
        let st = self.fns.pop();
        return st.finish(0, []);
    }

    # ------------------------------------------------------------ tokens

    def peek(self) { return self.toks[self.pos][0]; }

    def kind_at(self, i) { return self.toks[i][0]; }

    def advance(self) {
        let t = self.toks[self.pos];
        if (self.pos < self.ntoks - 1) { self.pos += 1; }
        return t;
    }

    def check(self, k) { return self.toks[self.pos][0] == k; }

    def matches(self, k) {
        if (self.toks[self.pos][0] == k) {
            self.advance();
            return True;
        }
        return False;
    }

    def expect(self, k, msg) {
        if (self.check(k)) {
            self.advance();
            return;
        }
        let found = self.toks[self.pos];
        if (k == ";" and found[2] > self.prev_line()) {
            raise CompileError(f"[line {self.prev_line()}] SyntaxError: {msg} (missing ';' at end of line?)");
        }
        if (k == "{" and found[0] == ":") {
            raise self.error(f"{msg} (Faston uses {{ }} blocks instead of ':')");
        }
        raise self.error(msg);
    }

    def expect_ident(self, msg) {
        let t = self.toks[self.pos];
        if (t[0] == "Ident") {
            self.advance();
            return t[1];
        }
        raise self.error(msg);
    }

    def error(self, msg) {
        let t = self.toks[self.pos];
        let at = "end of file" if t[0] == "EOF" else "'" + token_text(t) + "'";
        return CompileError(f"[line {t[2]}] SyntaxError at {at}: {msg}");
    }

    def error_prev(self, msg) {
        return CompileError(f"[line {self.prev_line()}] SyntaxError: {msg}");
    }

    def prev_line(self) {
        return self.toks[self.pos - 1 if self.pos > 0 else 0][2];
    }

    # ------------------------------------------------------------ emitting

    def cur(self) { return self.fns[-1]; }

    def emit(self, op) {
        let line = self.prev_line();
        let f = self.fns[-1];
        let code = f.code;
        code.append(op);
        f.lines.append(line);
        let n = len(code);
        # `x = <list just built in x's slot>` leaves GetLocal(a); SetLocal(a): a no-op
        if (n >= 2 and f.last_label != n - 1) {
            let a = code[n - 2];
            if (a[0] == "GetLocal" and op[0] == "SetLocal" and a[1] == op[1]) {
                code.pop();
                code.pop();
                f.lines.pop();
                f.lines.pop();
                return max(len(code) - 1, 0);
            }
        }
        # peephole: keep fusing the last two instructions (never across a jump target)
        while (True) {
            n = len(code);
            if (n < 2 or f.last_label == n - 1) { break; }
            let fused = fuse(code[n - 2], code[n - 1]);
            if (fused == None) { break; }
            code.pop();
            f.lines.pop();
            code[n - 2] = fused;
            f.lines[n - 2] = line;
        }
        return len(code) - 1;
    }

    def here(self) { return len(self.fns[-1].code); }

    def label(self) {
        let h = len(self.fns[-1].code);
        self.fns[-1].last_label = h;
        return h;
    }

    def patch(self, at) {
        let target = self.label();
        let code = self.fns[-1].code;
        code[at] = with_target(code[at], target);
    }

    def add_const(self, v) {
        let c = self.fns[-1].consts;
        c.append(v);
        return len(c) - 1;
    }

    # ------------------------------------------------------------ scopes

    def begin_scope(self) { self.fns[-1].depth += 1; }

    def end_scope(self) {
        let f = self.fns[-1];
        f.depth -= 1;
        let d = f.depth;
        let close_from = None;
        while (len(f.locals) > 0 and f.locals[-1].depth > d) {
            if (f.locals[-1].captured) { close_from = len(f.locals) - 1; }
            f.locals.pop();
        }
        if (close_from != None) { self.emit(("CloseUpvals", close_from)); }
    }

    def resolve_upvalue(self, fi, name) {
        if (fi == 0) { return None; }
        let parent = self.fns[fi - 1];
        if (name in parent.global_decls) { return None; }
        let slot = None;
        for (i in range(len(parent.locals) - 1, -1, -1)) {
            if (parent.locals[i].name == name) { slot = i; break; }
        }
        if (slot != None) {
            parent.locals[slot].captured = True;
            let is_const = parent.locals[slot].is_const;
            for (lp in parent.loops) {
                if (slot >= lp.boundary) { lp.captured = True; }
            }
            return (self.add_upvalue(fi, True, slot), is_const);
        }
        let r = self.resolve_upvalue(fi - 1, name);
        if (r == None) { return None; }
        return (self.add_upvalue(fi, False, r[0]), r[1]);
    }

    def add_upvalue(self, fi, from_local, index) {
        let ups = self.fns[fi].upvalues;
        for (i in range(len(ups))) {
            if (ups[i][0] == from_local and ups[i][1] == index) { return i; }
        }
        ups.append((from_local, index));
        return len(ups) - 1;
    }

    def nonlocal_upvalue(self, name) {
        if (not (name in self.cur().nonlocal_decls)) { return None; }
        let r = self.resolve_upvalue(len(self.fns) - 1, name);
        if (r == None) { raise self.error_prev(f"no enclosing function variable '{name}' for nonlocal"); }
        return r;
    }

    # Pushes a function: a constant, or a closure (captures or def-time defaults).
    def emit_function(self, func) {
        let k = self.add_const(func);
        let ranges = self.pending_defaults;
        self.pending_defaults = [];
        if (func.dynamic) {
            let resume = self.pos;
            for (r in ranges) {
                self.pos = r[0];
                self.expression();
                if (self.pos != r[1]) { raise self.error("unexpected token in default value"); }
            }
            self.pos = resume;
            self.emit(("ClosureDefaults", k, len(ranges)));
        } elif (len(func.captures) == 0) {
            self.emit(("Const", k));
        } else {
            self.emit(("Closure", k));
        }
    }

    def resolve_local(self, name) {
        let locals = self.fns[-1].locals;
        for (i in range(len(locals) - 1, -1, -1)) {
            if (locals[i].name == name) { return (i, locals[i].is_const); }
        }
        return None;
    }

    def declare_local(self, name, is_const) {
        let line = self.prev_line();
        let f = self.fns[-1];
        let d = f.depth;
        for (i in range(len(f.locals) - 1, -1, -1)) {
            let l = f.locals[i];
            if (l.depth < d) { break; }
            if (l.name == name) {
                if (l.is_const) { raise CompileError(f"[line {line}] SyntaxError: cannot redeclare constant '{name}'"); }
                l.is_const = is_const;
                return i;
            }
        }
        f.locals.append(Local(name, d, is_const));
        if (len(f.locals) > f.max_slots) { f.max_slots = len(f.locals); }
        return len(f.locals) - 1;
    }

    def loop_targets(self) {
        let names = [self.expect_ident("expected loop variable name")];
        while (self.matches(",")) { names.append(self.expect_ident("expected loop variable name")); }
        return names;
    }

    def declare_loop_targets(self, names) {
        if (len(names) == 1) { return (self.declare_local(names[0], False), []); }
        let item = self.declare_local("(item)", False);
        let slots = [];
        for (n in names) { slots.append(self.declare_local(n, False)); }
        return (item, slots);
    }

    def emit_unpack_targets(self, item, slots) {
        if (len(slots) == 0) { return; }
        self.emit(("GetLocal", item));
        self.emit(("Unpack", len(slots)));
        for (i in range(len(slots) - 1, -1, -1)) { self.emit(("SetLocal", slots[i])); }
    }

    def declare_iter_slots(self) {
        let a = self.declare_local("(iter)", False);
        self.declare_local("(iter.1)", False);
        self.declare_local("(iter.2)", False);
        return a;
    }

    def const_err(self, name) { return self.error_prev(f"cannot assign to constant '{name}'"); }

    def store_global(self, name, make_const) {
        if (self.gconst.get(name, False)) { raise self.const_err(name); }
        if (make_const) { self.gconst[name] = True; }
        self.emit(("SetGlobal", name));
    }

    def define_let(self, name, is_const) {
        let f = self.cur();
        if (f.is_script and f.depth == 0) {
            self.store_global(name, is_const);
        } else {
            let s = self.declare_local(name, is_const);
            self.emit(("SetLocal", s));
        }
    }

    def define_named(self, name) {
        let up = self.nonlocal_upvalue(name);
        if (up != None) {
            if (up[1]) { raise self.const_err(name); }
            self.emit(("SetUpval", up[0]));
            return;
        }
        let global_decl = name in self.cur().global_decls;
        if (not global_decl) {
            let l = self.resolve_local(name);
            if (l != None) {
                if (l[1]) { raise self.const_err(name); }
                self.emit(("SetLocal", l[0]));
                return;
            }
        }
        if (self.cur().is_script or global_decl) {
            self.store_global(name, False);
        } else {
            let s = self.declare_local(name, False);
            self.emit(("SetLocal", s));
        }
    }

    # ------------------------------------------------------------ statements

    def statement(self) {
        let t = self.toks[self.pos];
        let k = t[0];
        if (k == "let") { return self.let_decl(False); }
        if (k == "const") { return self.let_decl(True); }
        if (k == "global") { return self.global_decl(); }
        if (k == "def") { return self.def_decl(); }
        if (k == "class") { return self.class_decl(); }
        if (k == "if") { return self.if_stmt(); }
        if (k == "while") { return self.while_stmt(); }
        if (k == "for") { return self.for_stmt(); }
        if (k == "return") { return self.return_stmt(); }
        if (k == "break") { return self.break_stmt(); }
        if (k == "continue") { return self.continue_stmt(); }
        if (k == "import") { return self.import_stmt(); }
        if (k == "from") { return self.from_import_stmt(); }
        if (k == "nonlocal") { return self.nonlocal_decl(); }
        if (k == "try") { return self.try_stmt(); }
        if (k == "raise") { return self.raise_stmt(); }
        if (k == "assert") { return self.assert_stmt(); }
        if (k == "del") { return self.del_stmt(); }
        if (k == "with") { return self.with_stmt(); }
        if (k == "@") { return self.decorated(); }
        if (k == "yield") {
            self.yield_expr();
            self.expect(";", "expected ';' after yield");
            self.emit(("Pop",));
            return;
        }
        if (k == "Ident") {
            if (t[1] == "match" and self.is_match_start()) { return self.match_stmt(); }
            if (t[1] == "enum" and self.kind_at(self.pos + 1) == "Ident" and self.kind_at(self.pos + 2) == "{") {
                self.advance();
                let name = self.class_value(True);
                return self.define_named(name);
            }
            if (self.kind_at(self.pos + 1) == ",") { return self.multi_assign(); }
        }
        if (k == "pass") {
            self.advance();
            return self.expect(";", "expected ';' after 'pass'");
        }
        if (k == ";") {
            self.advance();
            return;
        }
        self.expr_stmt();
    }

    def let_decl(self, is_const) {
        self.advance();
        let kw = "const" if is_const else "let";
        let name = self.expect_ident(f"expected variable name after '{kw}'");
        if (self.check(",")) {
            let names = [name];
            while (self.matches(",")) { names.append(self.expect_ident("expected variable name")); }
            if (self.matches("=")) {
                self.expr_list();
                self.emit(("Unpack", len(names)));
            } elif (is_const) {
                raise self.error("constants need values: const A, B = 1, 2;");
            } else {
                for (n in names) { self.emit(("None",)); }
            }
            self.expect(";", "expected ';' after variable declaration");
            for (i in range(len(names) - 1, -1, -1)) { self.define_let(names[i], is_const); }
            return;
        }
        if (self.matches("=")) {
            self.expression();
        } elif (is_const) {
            raise self.error("a constant needs a value: const NAME = value;");
        } else {
            self.emit(("None",));
        }
        self.expect(";", "expected ';' after variable declaration");
        self.define_let(name, is_const);
    }

    def multi_assign(self) {
        let names = [];
        while (True) {
            names.append(self.expect_ident("expected a variable name"));
            if (not self.matches(",")) { break; }
        }
        self.expect("=", "expected '=' after assignment targets");
        self.expr_list();
        self.expect(";", "expected ';' after assignment");
        self.emit(("Unpack", len(names)));
        for (i in range(len(names) - 1, -1, -1)) { self.define_named(names[i]); }
    }

    def expr_list(self) {
        self.expression();
        let n = 1;
        while (self.matches(",")) {
            self.expression();
            n += 1;
        }
        if (n > 1) { self.emit(("BuildTuple", n)); }
    }

    def dotted_name(self, msg) {
        let name = self.expect_ident(msg);
        while (self.check(".")) {
            self.advance();
            name = name + "." + self.expect_ident("expected a name after '.'");
        }
        return name;
    }

    def import_stmt(self) {
        self.advance();
        while (True) {
            let name = self.dotted_name("expected a module name after 'import'");
            self.emit(("Import", name));
            if (self.matches("as")) {
                let alias = self.expect_ident("expected a name after 'as'");
                self.define_named(alias);
            } else {
                let first = name.split(".")[0];
                if (first != name) {
                    self.emit(("Pop",));
                    self.emit(("Import", first));
                }
                self.define_named(first);
            }
            if (not self.matches(",")) { break; }
        }
        self.expect(";", "expected ';' after import");
    }

    def from_import_stmt(self) {
        self.advance();
        let module = self.dotted_name("expected a module name after 'from'");
        self.expect("import", "expected 'import' after the module name");
        self.emit(("Import", module));
        while (True) {
            let name = self.expect_ident("expected a name to import");
            self.emit(("Dup",));
            self.emit(("GetAttr", name));
            let bind = name;
            if (self.matches("as")) { bind = self.expect_ident("expected a name after 'as'"); }
            self.define_named(bind);
            if (not self.matches(",")) { break; }
        }
        self.emit(("Pop",));
        self.expect(";", "expected ';' after import");
    }

    def nonlocal_decl(self) {
        self.advance();
        if (len(self.fns) < 2) { raise self.error_prev("'nonlocal' is only allowed inside a nested function"); }
        while (True) {
            let name = self.expect_ident("expected a name after 'nonlocal'");
            self.cur().nonlocal_decls.add(name);
            self.nonlocal_upvalue(name);
            if (not self.matches(",")) { break; }
        }
        self.expect(";", "expected ';' after nonlocal declaration");
    }

    def assert_stmt(self) {
        self.advance();
        self.emit(("GetGlobal", "$assert"));
        self.expression();
        if (self.matches(",")) {
            self.expression();
        } else {
            self.emit(("None",));
        }
        self.expect(";", "expected ';' after assert");
        self.emit(("Call", 2));
        self.emit(("Pop",));
    }

    # A literal usable as a default parameter value; raises if it isn't one.
    def literal_value(self) {
        let neg = self.matches("-");
        let t = self.advance();
        let k = t[0];
        if (k == "Int") { return -t[1] if neg else t[1]; }
        if (k == "Float") { return -t[1] if neg else t[1]; }
        if (not neg) {
            if (k == "Str" or k == "Bytes") { return t[1]; }
            if (k == "True") { return True; }
            if (k == "False") { return False; }
            if (k == "None") { return None; }
        }
        self.pos -= 1;
        raise self.error("default values must be literals (numbers, strings, True, False, None)");
    }

    def global_decl(self) {
        self.advance();
        if (self.cur().is_script) { raise self.error_prev("'global' is only allowed inside a function"); }
        while (True) {
            let name = self.expect_ident("expected a name after 'global'");
            self.cur().global_decls.add(name);
            if (not self.matches(",")) { break; }
        }
        self.expect(";", "expected ';' after global declaration");
    }

    def def_decl(self) {
        self.advance();
        let name = self.expect_ident("expected function name after 'def'");
        let f = self.cur();
        if (not f.is_script and not (name in f.global_decls) and not (name in f.nonlocal_decls)
                and self.resolve_local(name) == None) {
            self.declare_local(name, False);
        }
        let func = self.function(name, False);
        self.emit_function(func);
        self.define_named(name);
    }

    def function(self, name, is_init) {
        self.fns.append(FnState(name, False, is_init));
        if (self.has_pending_method) {
            self.has_pending_method = False;
            self.cur().is_method = True;
            self.cur().class_base = self.pending_base;
        }
        self.expect("(", "expected '(' after function name");
        let p = self.params();
        self.hoist_locals();
        if (self.cur().is_generator) {
            if (is_init) { raise self.error("init() can't be a generator (it contains 'yield')"); }
            self.emit(("Gen",));
        }
        self.block();
        if (is_init) {
            self.emit(("GetLocal", 0));
        } else {
            self.emit(("None",));
        }
        self.emit(("Return",));
        return self.finish_function(p[0], p[1], p[2]);
    }

    def finish_function(self, arity, defaults, dynamic) {
        let st = self.fns.pop();
        let f = st.finish(arity, defaults);
        f.dynamic = len(dynamic) > 0;
        self.pending_defaults = dynamic;
        return f;
    }

    def lambda_expr(self) {
        self.fns.append(FnState("<lambda>", False, False));
        self.expect("(", "expected '(' after 'lambda'");
        let p = self.params();
        if (self.check("{")) {
            self.hoist_locals();
            if (self.cur().is_generator) { raise self.error("a lambda can't contain 'yield' (use def)"); }
            self.block();
            self.emit(("None",));
        } else {
            self.expression();
        }
        self.emit(("Return",));
        let func = self.finish_function(p[0], p[1], p[2]);
        self.emit_function(func);
    }

    # Python scoping: names plainly assigned anywhere in a function body are its
    # locals. Pre-scans the body (cursor on its '{') and declares them up front.
    def hoist_locals(self) {
        let toks = self.toks;
        let end = self.skip_braces(self.pos);
        let names = [];
        let excluded = set();
        let i = self.pos + 1;
        let stmt_start = True;
        let saw_yield = False;
        while (i < end - 1) {
            let k = toks[i][0];
            if (k == "yield") {
                saw_yield = True;
                stmt_start = False;
            } elif (k == "@") {
                stmt_start = False;
            } elif (k == "def") {
                i += 2;
                i = self.skip_group(i, "(", ")", "expected '('");
                i = self.skip_braces(i);
                stmt_start = True;
                continue;
            } elif (k == "class") {
                i += 2;
                if (toks[i][0] == "(") { i = self.skip_group(i, "(", ")", "expected '('"); }
                i = self.skip_braces(i);
                stmt_start = True;
                continue;
            } elif (k == "lambda") {
                i += 1;
                i = self.skip_group(i, "(", ")", "expected '(' after 'lambda'");
                if (toks[i][0] == "{") { i = self.skip_braces(i); }
                stmt_start = False;
                continue;
            } elif (k == "global" or k == "nonlocal") {
                i += 1;
                while (toks[i][0] == "Ident") {
                    excluded.add(toks[i][1]);
                    i += 1;
                    if (toks[i][0] != ",") { break; }
                    i += 1;
                }
                continue;
            } elif (k == "Ident" and stmt_start and toks[i][1] == "enum" and toks[i + 1][0] == "Ident" and toks[i + 2][0] == "{") {
                i += 2;
                i = self.skip_braces(i);
                stmt_start = True;
                continue;
            } elif (k == "Ident" and stmt_start) {
                let group = [toks[i][1]];
                let j = i + 1;
                while (toks[j][0] == ",") {
                    if (toks[j + 1][0] == "Ident") {
                        group.append(toks[j + 1][1]);
                        j += 2;
                    } else {
                        break;
                    }
                }
                if (toks[j][0] == "=") { names.extend(group); }
                stmt_start = False;
                i = j;
                continue;
            } elif (k == ";" or k == "{" or k == "}") {
                stmt_start = True;
            } else {
                stmt_start = False;
            }
            i += 1;
        }
        for (n in names) {
            if (not (n in excluded) and self.resolve_local(n) == None) { self.declare_local(n, False); }
        }
        if (saw_yield) { self.cur().is_generator = True; }
    }

    # Parameter list after '(' up to and including ')'. Returns (arity, literal
    # defaults, token ranges of defaults evaluated at def time).
    def params(self) {
        let arity = 0;
        let defaults = [];
        let ranges = [];
        let all_literal = True;
        let star = None;
        let starstar = None;
        if (not self.check(")")) {
            while (True) {
                if (starstar != None) { raise self.error("'**kwargs' must be the last parameter"); }
                if (self.matches("**")) {
                    starstar = self.expect_ident("expected a name after '**'");
                    if (not self.matches(",") or self.check(")")) { break; }
                    continue;
                }
                if (self.matches("*")) {
                    if (star != None) { raise self.error_prev("only one '*args' parameter is allowed"); }
                    star = self.expect_ident("expected a name after '*'");
                    if (not self.matches(",") or self.check(")")) { break; }
                    continue;
                }
                if (star != None) {
                    raise self.error("parameters after '*args' aren't supported yet; put them before it");
                }
                let p = self.expect_ident("expected parameter name");
                for (l in self.cur().locals) {
                    if (l.name == p) { raise self.error_prev(f"duplicate parameter '{p}'"); }
                }
                self.declare_local(p, False);
                self.cur().param_syms.append(p);
                arity += 1;
                if (self.matches("=")) {
                    let start = self.pos;
                    let end = self.skip_default_expr();
                    ranges.append((start, end));
                    self.pos = start;
                    let ok = False;
                    let v = None;
                    try {
                        v = self.literal_value();
                        ok = self.pos == end;
                    } except (CompileError) {
                        ok = False;
                    }
                    if (ok) {
                        defaults.append(v);
                    } else {
                        all_literal = False;
                        defaults.append(None);
                    }
                    self.pos = end;
                } elif (len(defaults) > 0) {
                    raise self.error_prev(f"parameter '{p}' needs a default value because an earlier one has one");
                }
                if (not self.matches(",") or self.check(")")) { break; }
            }
        }
        self.expect(")", "expected ')' after parameters");
        for (pair in [(star, True), (starstar, False)]) {
            let n = pair[0];
            if (n != None) {
                for (l in self.cur().locals) {
                    if (l.name == n) { raise self.error_prev(f"duplicate parameter '{n}'"); }
                }
                self.declare_local(n, False);
                if (pair[1]) { self.cur().varargs = True; } else { self.cur().varkw = True; }
            }
        }
        if (all_literal) { ranges = []; }
        return (arity, defaults, ranges);
    }

    def skip_default_expr(self) {
        let depth = 0;
        let i = self.pos;
        while (True) {
            let k = self.toks[i][0];
            if (k == "(" or k == "[" or k == "{") {
                depth += 1;
            } elif ((k == ")" or k == "]" or k == "}") and depth > 0) {
                depth -= 1;
            } elif ((k == "," or k == ")") and depth == 0) {
                break;
            } elif (k == "EOF" or k == ";" or k == "]" or k == "}") {
                self.pos = i;
                raise self.error("unterminated default value");
            }
            i += 1;
        }
        if (i == self.pos) { raise self.error("expected a default value after '='"); }
        return i;
    }

    def class_decl(self) {
        self.advance();
        let name = self.class_value(False);
        self.define_named(name);
    }

    # `class Name(Base) { ... }` after `class` (or `enum Name { A, B = 2 }` after
    # `enum`): leaves the class on the stack and returns its name.
    def class_value(self, enum_syntax) {
        let what = "enum" if enum_syntax else "class";
        let name = self.expect_ident(f"expected a name after '{what}'");
        self.emit(("Class", name));
        let base = None;
        let is_enum = enum_syntax;
        if (enum_syntax) {
            self.emit(("GetGlobal", "Enum"));
            self.emit(("Inherit",));
        } elif (self.matches("(")) {
            let start = self.pos;
            self.expression();
            base = (start, self.pos);
            let n = self.pos - start;
            let t = self.toks;
            is_enum = (n == 1 and t[start][0] == "Ident" and t[start][1] == "Enum")
                or (n == 3 and t[start][0] == "Ident" and t[start][1] == "enum" and t[start + 1][0] == "."
                    and t[start + 2][0] == "Ident" and t[start + 2][1] == "Enum");
            self.expect(")", "expected ')' after superclass");
            self.emit(("Inherit",));
        }
        self.expect("{", f"expected '{{' before {what} body");
        let members = set();
        while (not self.check("}") and not self.check("EOF")) {
            if (enum_syntax and self.check("Ident")) {
                let m = self.expect_ident("expected a member name");
                if (self.matches("=")) {
                    self.expression();
                } else {
                    self.emit(("GetGlobal", "$auto"));
                }
                self.emit(("Method", m));
                members.add(m);
                if (not self.matches(",") and not self.matches(";") and not self.check("}")) {
                    raise self.error("expected ',' between enum members");
                }
                continue;
            }
            if (self.check("@") or self.check("def")) {
                let ndec = 0;
                while (self.matches("@")) {
                    let t = self.toks;
                    let p = self.pos;
                    if (t[p][0] == "Ident" and t[p + 1][0] == "." and t[p + 2][0] == "Ident"
                            and t[p][1] in members and t[p + 2][1] in ("setter", "getter", "deleter")
                            and t[p + 3][0] == "def") {
                        self.pos += 3;
                        self.emit(("Dup",));
                        self.emit(("GetAttr", t[p][1]));
                        self.emit(("GetAttr", t[p + 2][1]));
                    } else {
                        self.parse_or(False);
                    }
                    ndec += 1;
                }
                self.expect("def", "expected 'def' after a decorator in a class body");
                let mname = self.expect_ident("expected method name after 'def'");
                let is_init = mname == "init";
                self.has_pending_method = True;
                self.pending_base = base;
                let func = self.function(mname, is_init);
                if (ndec == 0 and func.arity == 0 and not func.varargs) {
                    raise self.error_prev(f"method '{mname}' needs a 'self' parameter (or use @staticmethod)");
                }
                self.emit_function(func);
                for (d in range(ndec)) { self.emit(("Call", 1)); }
                self.emit(("Method", mname));
                members.add(mname);
            } elif (self.matches("pass")) {
                self.expect(";", "expected ';' after 'pass'");
            } elif (self.check("Ident") and self.kind_at(self.pos + 1) == "=") {
                let attr = self.toks[self.pos][1];
                self.pos += 2;
                self.expression();
                self.expect(";", "expected ';' after class attribute");
                self.emit(("Method", attr));
                members.add(attr);
            } else {
                raise self.error("a class body can only contain methods (def), class attributes (name = value;) and pass");
            }
        }
        self.expect("}", f"expected '}}' after {what} body");
        if (is_enum) {
            self.emit(("Dup",));
            self.emit(("GetGlobal", "$enum"));
            self.emit(("Swap",));
            self.emit(("Call", 1));
            self.emit(("Pop",));
        }
        return name;
    }

    # ------------------------------------------------------------ match

    def is_match_start(self) {
        let k = self.kind_at(self.pos + 1);
        if (k == "(") {
            try {
                let i = self.skip_group(self.pos + 1, "(", ")", "");
                return i < self.ntoks and self.toks[i][0] == "{";
            } except (CompileError) {
                return False;
            }
        }
        if (k == "Ident") { return self.kind_at(self.pos + 2) == "{"; }
        return False;
    }

    def match_stmt(self) {
        self.advance();
        if (self.check("(")) {
            self.paren_condition("match");
        } else {
            self.expression();
        }
        self.begin_scope();
        let subject = self.declare_local("(match)", False);
        self.emit(("SetLocal", subject));
        self.expect("{", "expected '{' after the match subject");
        let ends = [];
        while (not self.check("}") and not self.check("EOF")) {
            let t = self.toks[self.pos];
            if (t[0] == "Ident" and t[1] == "case") {
                self.advance();
            } else {
                raise self.error("expected 'case' in a match block");
            }
            self.begin_scope();
            let fails = [];
            self.pattern(subject, fails);
            if (self.matches("if")) {
                self.expression();
                fails.append(self.emit(("JumpIfFalse", 0)));
            }
            self.block();
            self.end_scope();
            ends.append(self.emit(("Jump", 0)));
            for (f in fails) { self.patch(f); }
        }
        self.expect("}", "expected '}' to close the match block");
        for (e in ends) { self.patch(e); }
        self.end_scope();
    }

    def match_temp(self) {
        let n = len(self.cur().locals);
        return self.declare_local(f"(pat{n})", False);
    }

    def match_bind(self, src, name) {
        let s = self.declare_local(name, False);
        self.emit(("GetLocal", src));
        self.emit(("SetLocal", s));
    }

    def pattern(self, src, fails) {
        let oks = [];
        while (True) {
            let alt_fails = [];
            self.closed_pattern(src, alt_fails);
            if (self.matches("|")) {
                oks.append(self.emit(("Jump", 0)));
                for (f in alt_fails) { self.patch(f); }
                continue;
            }
            fails.extend(alt_fails);
            break;
        }
        for (o in oks) { self.patch(o); }
        if (self.matches("as")) {
            let name = self.expect_ident("expected a name after 'as'");
            self.match_bind(src, name);
        }
    }

    def pattern_literal(self) {
        let neg = self.matches("-");
        let t = self.advance();
        let v = None;
        if (t[0] == "Int" or t[0] == "Float") {
            v = -t[1] if neg else t[1];
        } elif ((t[0] == "Str" or t[0] == "Bytes") and not neg) {
            v = t[1];
        } else {
            raise self.error_prev("expected a literal in the pattern");
        }
        let k = self.add_const(v);
        self.emit(("Const", k));
    }

    def dotted_value(self, first) {
        self.variable(first, False);
        while (self.matches(".")) {
            let attr = self.expect_ident("expected a name after '.'");
            self.emit(("GetAttr", attr));
        }
    }

    def closed_pattern(self, src, fails) {
        let t = self.toks[self.pos];
        let k = t[0];
        if (k == "Int" or k == "Float" or k == "Str" or k == "Bytes" or k == "-") {
            self.emit(("GetLocal", src));
            self.pattern_literal();
            self.emit(("Eq",));
            fails.append(self.emit(("JumpIfFalse", 0)));
        } elif (k == "None" or k == "True" or k == "False") {
            self.emit(("GetLocal", src));
            self.advance();
            self.emit((k,));
            self.emit(("Is",));
            fails.append(self.emit(("JumpIfFalse", 0)));
        } elif (k == "[") {
            self.advance();
            self.seq_pattern(src, fails, "]");
        } elif (k == "(") {
            self.advance();
            if (self.seq_has_comma(")") or self.check(")")) {
                self.seq_pattern(src, fails, ")");
            } else {
                self.pattern(src, fails);
                self.expect(")", "expected ')' after the pattern");
            }
        } elif (k == "{") {
            self.advance();
            self.map_pattern(src, fails);
        } elif (k == "Ident") {
            let name = t[1];
            self.advance();
            if (self.check(".") or self.check("(")) {
                let start = self.pos - 1;
                let end = self.pos;
                while (self.toks[end][0] == ".") { end += 2; }
                if (self.toks[end][0] == "(") {
                    self.class_pattern(src, fails, start, end);
                } else {
                    self.emit(("GetLocal", src));
                    self.dotted_value(name);
                    self.emit(("Eq",));
                    fails.append(self.emit(("JumpIfFalse", 0)));
                }
            } elif (name != "_") {
                self.match_bind(src, name);
            }
        } else {
            raise self.error("expected a pattern");
        }
    }

    def seq_has_comma(self, close) {
        let depth = 0;
        let i = self.pos;
        while (i < self.ntoks) {
            let k = self.toks[i][0];
            if (k == "(" or k == "[" or k == "{") {
                depth += 1;
            } elif ((k == ")" or k == "]" or k == "}") and depth > 0) {
                depth -= 1;
            } elif (k == close and depth == 0) {
                return False;
            } elif (k == "," and depth == 0) {
                return True;
            } elif (k == "EOF") {
                return False;
            }
            i += 1;
        }
        return False;
    }

    def seq_pattern(self, src, fails, close) {
        let items = 0;
        let star = None;
        let depth = 0;
        let i = self.pos;
        let item_start = True;
        while (True) {
            let k = self.toks[i][0];
            if (k == "EOF") { raise self.error("unclosed pattern"); }
            if (k == close and depth == 0) { break; }
            if (k == "(" or k == "[" or k == "{") {
                depth += 1;
            } elif (k == ")" or k == "]" or k == "}") {
                depth -= 1;
            }
            if (depth == 0 and k == ",") {
                item_start = True;
            } elif (item_start) {
                if (k == "*" and depth == 0) {
                    if (star != None) { raise self.error("only one '*' is allowed in a sequence pattern"); }
                    star = items;
                }
                items += 1;
                item_start = False;
            }
            i += 1;
        }
        let fixed = items - (1 if star != None else 0);
        self.emit(("GetLocal", src));
        self.emit(("MatchSeq", fixed, 1 if star != None else 0));
        fails.append(self.emit(("JumpIfFalse", 0)));
        for (kk in range(items)) {
            if (star != None and kk == star) {
                self.expect("*", "expected '*'");
                let name = self.expect_ident("expected a name after '*'");
                if (name != "_") {
                    let s = self.declare_local(name, False);
                    self.emit(("GetLocal", src));
                    self.emit(("MatchRest", kk, items - 1 - kk));
                    self.emit(("SetLocal", s));
                }
            } else {
                let index = kk;
                if (star != None and kk > star) { index = -(items - kk); }
                let tmp = self.match_temp();
                self.emit(("GetLocal", src));
                let c = self.add_const(index);
                self.emit(("Const", c));
                self.emit(("GetIndex",));
                self.emit(("SetLocal", tmp));
                self.pattern(tmp, fails);
            }
            if (kk + 1 < items) {
                self.expect(",", "expected ',' between patterns");
            } else {
                self.matches(",");
            }
        }
        self.expect(close, "expected the end of the sequence pattern");
    }

    def map_pattern(self, src, fails) {
        self.emit(("GetLocal", src));
        self.emit(("MatchMap",));
        fails.append(self.emit(("JumpIfFalse", 0)));
        let keys = [];
        while (not self.check("}")) {
            if (self.matches("**")) {
                let name = self.expect_ident("expected a name after '**'");
                self.matches(",");
                if (not self.check("}")) { raise self.error("'**rest' must be the last item of a mapping pattern"); }
                let resume = self.pos;
                self.emit(("GetLocal", src));
                for (kp in keys) {
                    self.pos = kp;
                    self.map_key();
                }
                self.pos = resume;
                self.emit(("MatchMapRest", len(keys)));
                let s = self.declare_local(name, False);
                self.emit(("SetLocal", s));
                break;
            }
            let tmp = self.match_temp();
            self.emit(("GetLocal", src));
            keys.append(self.pos);
            self.map_key();
            self.expect(":", "expected ':' after the key");
            self.emit(("MatchKey", tmp));
            fails.append(self.emit(("JumpIfFalse", 0)));
            self.pattern(tmp, fails);
            if (not self.matches(",")) { break; }
        }
        self.expect("}", "expected '}' after the mapping pattern");
    }

    def map_key(self) {
        let t = self.toks[self.pos];
        if (t[0] == "Ident") {
            self.advance();
            self.dotted_value(t[1]);
        } elif (t[0] == "None" or t[0] == "True" or t[0] == "False") {
            self.advance();
            self.emit((t[0],));
        } else {
            self.pattern_literal();
        }
    }

    def load_class_at(self, start) {
        self.pos = start;
        let first = self.expect_ident("expected a class name");
        self.dotted_value(first);
    }

    def class_pattern(self, src, fails, start, end) {
        self.emit(("GetLocal", src));
        self.load_class_at(start);
        self.expect("(", "expected '('");
        self.emit(("MatchClass",));
        fails.append(self.emit(("JumpIfFalse", 0)));
        let i = 0;
        let keywords = False;
        while (not self.check(")")) {
            let tmp = self.match_temp();
            if (self.check("Ident") and self.kind_at(self.pos + 1) == "=") {
                let attr = self.expect_ident("expected an attribute name");
                self.advance();
                self.emit(("GetLocal", src));
                self.emit(("MatchAttr", attr, tmp));
                keywords = True;
            } else {
                if (keywords) { raise self.error("positional patterns must come before keyword patterns"); }
                self.emit(("GetLocal", src));
                let here = self.pos;
                self.load_class_at(start);
                self.pos = here;
                self.emit(("MatchArg", i, tmp));
                i += 1;
            }
            fails.append(self.emit(("JumpIfFalse", 0)));
            self.pattern(tmp, fails);
            if (not self.matches(",")) { break; }
        }
        self.expect(")", "expected ')' after the class pattern");
    }

    # ------------------------------------------------------------ more statements

    def decorated(self) {
        let n = 0;
        while (self.matches("@")) {
            self.parse_or(False);
            n += 1;
        }
        let k = self.peek();
        if (k == "def") {
            self.advance();
            let name = self.expect_ident("expected function name after 'def'");
            let f = self.cur();
            if (not f.is_script and not (name in f.global_decls) and not (name in f.nonlocal_decls)
                    and self.resolve_local(name) == None) {
                self.declare_local(name, False);
            }
            let func = self.function(name, False);
            self.emit_function(func);
            for (d in range(n)) { self.emit(("Call", 1)); }
            return self.define_named(name);
        }
        if (k == "class") {
            self.advance();
            let name = self.class_value(False);
            for (d in range(n)) { self.emit(("Call", 1)); }
            return self.define_named(name);
        }
        raise self.error("expected 'def' or 'class' after a decorator");
    }

    def yield_expr(self) {
        self.advance();
        if (self.cur().is_script) { raise self.error_prev("'yield' outside of a function"); }
        if (not self.cur().is_generator) {
            raise self.error_prev("'yield' can only be used directly inside a def (not in a lambda)");
        }
        if (self.matches("from")) {
            self.expression();
            self.begin_scope();
            let iter_slot = self.declare_iter_slots();
            let var = self.declare_local("(yield from)", False);
            self.emit(("GetIter", iter_slot));
            let top = self.label();
            let exit = self.emit(("ForIter", iter_slot, var, 0));
            self.emit(("GetLocal", var));
            self.emit(("Yield",));
            self.emit(("Pop",));
            self.emit(("Jump", top));
            self.patch(exit);
            self.end_scope();
            self.emit(("None",));
            return;
        }
        let k = self.peek();
        if (k == ";" or k == ")" or k == "]" or k == "}" or k == "," or k == ":") {
            self.emit(("None",));
        } else {
            self.expr_list();
        }
        self.emit(("Yield",));
    }

    def emit_with_exit(self, slot) {
        self.emit(("GetLocal", slot));
        self.emit(("None",));
        self.emit(("None",));
        self.emit(("None",));
        self.emit(("Invoke", "__exit__", 3));
        self.emit(("Pop",));
    }

    def with_stmt(self) {
        self.advance();
        self.expect("(", "expected '(' after 'with'");
        self.begin_scope();
        let guards = [];
        while (True) {
            self.expression();
            let hidden = f"(with {len(self.cur().locals)})";
            let slot = self.declare_local(hidden, False);
            self.emit(("SetLocal", slot));
            self.emit(("GetLocal", slot));
            self.emit(("Invoke", "__enter__", 0));
            if (self.matches("as")) {
                let name = self.expect_ident("expected a name after 'as'");
                self.define_named(name);
            } else {
                self.emit(("Pop",));
            }
            let boundary = len(self.cur().locals);
            let setup = self.emit(("SetupTry", 0));
            self.cur().active.append(("W", slot));
            guards.append((slot, setup, boundary));
            if (not self.matches(",")) { break; }
        }
        self.expect(")", "expected ')' after with items");
        self.scoped_block();
        for (gi in range(len(guards) - 1, -1, -1)) {
            let g = guards[gi];
            let slot = g[0];
            self.emit(("PopTry",));
            self.cur().active.pop();
            self.emit_with_exit(slot);
            let done = self.emit(("Jump", 0));
            self.patch(g[1]);
            self.handler_entry(g[2]);
            let exc = self.declare_local("(with exc)", False);
            self.emit(("SetLocal", exc));
            self.emit(("GetLocal", slot));
            self.emit(("GetGlobal", "$classof"));
            self.emit(("GetLocal", exc));
            self.emit(("Call", 1));
            self.emit(("GetLocal", exc));
            self.emit(("None",));
            self.emit(("Invoke", "__exit__", 3));
            let swallowed = self.emit(("JumpIfTrue", 0));
            self.emit(("GetLocal", exc));
            self.emit(("Raise",));
            self.patch(swallowed);
            self.patch(done);
        }
        self.end_scope();
    }

    def del_stmt(self) {
        self.advance();
        while (True) {
            let t = self.toks[self.pos];
            let nk = self.kind_at(self.pos + 1);
            if (t[0] == "Ident" and (nk == "," or nk == ";")) {
                self.advance();
                self.del_name(t[1]);
            } else {
                self.del_mode = True;
                let done = False;
                try {
                    done = self.parse_postfix(False);
                } finally {
                    self.del_mode = False;
                }
                if (not done) {
                    raise self.error_prev("can only delete names, items (x[i], x[a:b]) and attributes (x.name)");
                }
            }
            if (not self.matches(",")) { break; }
        }
        self.expect(";", "expected ';' after del");
    }

    def del_name(self, name) {
        let global_decl = name in self.cur().global_decls;
        if (not global_decl and (self.resolve_local(name) != None
                or self.resolve_upvalue(len(self.fns) - 1, name) != None)) {
            raise self.error_prev(f"can't delete local variable '{name}' (assign None to it instead)");
        }
        if (self.gconst.get(name, False)) { raise self.error_prev(f"cannot delete constant '{name}'"); }
        self.emit(("DelGlobal", name));
    }

    def super_call(self) {
        let f = self.cur();
        let base = None;
        if (f.is_method and f.class_base != None) {
            base = f.class_base;
        } elif (f.is_method) {
            raise self.error_prev("super() needs a base class: class Child(Parent) { ... }");
        } else {
            raise self.error_prev("super() can only be used directly inside a method");
        }
        self.expect("(", "expected '(' after super");
        self.expect(")", "super() takes no arguments in Faston");
        self.expect(".", "expected '.' after super(): super().method(...)");
        let mname = self.expect_ident("expected a method name after 'super().'");
        let resume = self.pos;
        self.pos = base[0];
        self.expression();
        if (self.pos != base[1]) { raise self.error("unexpected token in base class"); }
        self.pos = resume;
        self.emit(("GetAttr", mname));
        if (not self.check("(")) { raise self.error("super() can only call a method: super().name(...)"); }
        if (self.args_have_splat_after_paren()) {
            raise self.error("super() calls with * or ** arguments aren't supported yet");
        }
        self.advance();
        self.emit(("GetLocal", 0));
        let a = self.arguments();
        if (a[0] == "Plain") {
            self.emit(("Call", a[1] + 1));
        } else {
            self.emit(("CallKw", a[1] + 1, a[2]));
        }
        return False;
    }

    def block(self) {
        self.expect("{", "expected '{'");
        while (not self.check("}") and not self.check("EOF")) { self.statement(); }
        self.expect("}", "expected '}' to close block");
    }

    def scoped_block(self) {
        self.begin_scope();
        self.block();
        self.end_scope();
    }

    def paren_condition(self, kw) {
        self.expect("(", f"expected '(' after '{kw}'");
        self.expression();
        self.expect(")", f"expected ')' after {kw} condition");
    }

    def if_stmt(self) {
        self.advance();
        self.paren_condition("if");
        let jf = self.emit(("JumpIfFalse", 0));
        self.scoped_block();
        let end_jumps = [];
        while (True) {
            if (self.matches("elif")) {
                end_jumps.append(self.emit(("Jump", 0)));
                self.patch(jf);
                jf = None;
                self.paren_condition("elif");
                jf = self.emit(("JumpIfFalse", 0));
                self.scoped_block();
            } elif (self.matches("else")) {
                end_jumps.append(self.emit(("Jump", 0)));
                self.patch(jf);
                jf = None;
                if (self.check("if")) {
                    self.if_stmt();
                } else {
                    self.scoped_block();
                }
                break;
            } else {
                break;
            }
        }
        if (jf != None) { self.patch(jf); }
        for (j in end_jumps) { self.patch(j); }
    }

    def while_stmt(self) {
        self.advance();
        let start = self.label();
        self.paren_condition("while");
        let exit = self.emit(("JumpIfFalse", 0));
        self.push_loop(start);
        self.scoped_block();
        self.emit(("Jump", start));
        self.patch(exit);
        self.finish_loop(start);
    }

    def push_loop(self, start) {
        let f = self.cur();
        f.loops.append(LoopCtx(start, len(f.locals), len(f.active)));
    }

    def finish_loop(self, start) {
        let ctx = self.cur().loops.pop();
        if (not ctx.captured or (len(ctx.breaks) == 0 and len(ctx.continues) == 0)) {
            for (b in ctx.breaks) { self.patch(b); }
            return;
        }
        let skip = self.emit(("Jump", 0));
        let to_end = [skip];
        let code = self.cur().code;
        if (len(ctx.breaks) > 0) {
            let stub = self.label();
            for (site in ctx.breaks) { code[site] = with_target(code[site], stub); }
            self.emit(("CloseUpvals", ctx.boundary));
            to_end.append(self.emit(("Jump", 0)));
        }
        if (len(ctx.continues) > 0) {
            let stub = self.label();
            for (site in ctx.continues) { code[site] = with_target(code[site], stub); }
            self.emit(("CloseUpvals", ctx.boundary));
            self.emit(("Jump", start));
        }
        for (j in to_end) { self.patch(j); }
    }

    def for_stmt(self) {
        self.advance();
        self.expect("(", "expected '(' after 'for'");
        let names = self.loop_targets();
        self.expect("in", "expected 'in' after loop variable");
        self.begin_scope();
        self.expression();
        self.expect(")", "expected ')' after for clause");
        let iter_slot = self.declare_iter_slots();
        self.emit(("GetIter", iter_slot));
        let tv = self.declare_loop_targets(names);
        let var_slot = tv[0];
        let exit = self.emit(("ForIter", iter_slot, var_slot, 0));
        let body = self.label();
        self.emit_unpack_targets(var_slot, tv[1]);
        self.push_loop(-1);
        self.scoped_block();
        let cont = self.label();
        let lp = self.cur().loops[-1];
        let code = self.cur().code;
        for (c in lp.continues) { code[c] = with_target(code[c], cont); }
        lp.continue_target = cont;
        self.emit(("ForLoop", iter_slot, var_slot, body));
        self.patch(exit);
        self.finish_loop(cont);
        self.end_scope();
    }

    def return_stmt(self) {
        self.advance();
        if (self.cur().is_script) { raise self.error_prev("'return' outside of a function"); }
        if (self.matches(";")) {
            if (self.cur().is_init) {
                self.emit(("GetLocal", 0));
            } else {
                self.emit(("None",));
            }
        } else {
            if (self.cur().is_init) { raise self.error("init() cannot return a value"); }
            self.expr_list();
            self.expect(";", "expected ';' after return value");
        }
        self.unwind_handlers(0);
        self.emit(("Return",));
    }

    def unwind_handlers(self, depth) {
        let saved = list(self.cur().active);
        while (len(self.cur().active) > depth) {
            let h = self.cur().active.pop();
            self.emit(("PopTry",));
            if (h[0] == "F") {
                self.block_at(h[1]);
            } elif (h[0] == "W") {
                self.emit_with_exit(h[1]);
            }
        }
        self.cur().active = saved;
    }

    def block_at(self, r) {
        let resume = self.pos;
        self.pos = r[0];
        self.scoped_block();
        if (self.pos != r[1]) { raise self.error("unexpected token in block"); }
        self.pos = resume;
    }

    def skip_braces(self, p) { return self.skip_group(p, "{", "}", "expected '{'"); }

    def skip_group(self, p, open, close, msg) {
        let toks = self.toks;
        if (toks[p][0] != open) { raise CompileError(f"[line {toks[p][2]}] SyntaxError: {msg}"); }
        let depth = 0;
        let i = p;
        while (True) {
            let k = toks[i][0];
            if (k == open) {
                depth += 1;
            } elif (k == close) {
                depth -= 1;
                if (depth == 0) { return i + 1; }
            } elif (k == "EOF") {
                raise CompileError(f"[line {toks[p][2]}] SyntaxError: unclosed '{open}'");
            }
            i += 1;
        }
    }

    def try_stmt(self) {
        self.advance();
        let body_start = self.pos;
        let p = self.skip_braces(body_start);
        let clauses_start = p;
        let has_except = False;
        while (self.toks[p][0] == "except") {
            has_except = True;
            p += 1;
            if (self.toks[p][0] == "(") { p = self.skip_group(p, "(", ")", "expected '('"); }
            p = self.skip_braces(p);
        }
        let else_range = None;
        if (self.toks[p][0] == "else") {
            let s = p + 1;
            p = self.skip_braces(s);
            else_range = (s, p);
        }
        let fin = None;
        if (self.toks[p][0] == "finally") {
            let s = p + 1;
            p = self.skip_braces(s);
            fin = (s, p);
        }
        let stmt_end = p;
        if (not has_except and fin == None) {
            raise self.error_prev("'try' needs at least one 'except' or a 'finally' block");
        }
        if (else_range != None and not has_except) {
            raise self.error_prev("'else' after 'try' needs an 'except' block");
        }
        let boundary = len(self.cur().locals);
        let fin_setup = None;
        if (fin != None) {
            fin_setup = self.emit(("SetupTry", 0));
            self.cur().active.append(("F", fin));
        }
        let exc_setup = None;
        if (has_except) {
            exc_setup = self.emit(("SetupTry", 0));
            self.cur().active.append(("H",));
        }
        self.scoped_block();
        if (has_except) {
            self.emit(("PopTry",));
            self.cur().active.pop();
        }
        if (else_range != None) { self.block_at(else_range); }
        if (fin != None) {
            self.emit(("PopTry",));
            self.cur().active.pop();
            self.block_at(fin);
        }
        let end_jumps = [self.emit(("Jump", 0))];
        if (exc_setup != None) {
            self.patch(exc_setup);
            self.handler_entry(boundary);
            if (fin != None) { self.cur().active.append(("F", fin)); }
            self.begin_scope();
            let exc_slot = self.declare_local("(exc)", False);
            self.emit(("SetLocal", exc_slot));
            self.cur().exc_slots.append(exc_slot);
            self.pos = clauses_start;
            let saw_bare = False;
            while (self.matches("except")) {
                if (saw_bare) { raise self.error_prev("a bare 'except' must be the last except clause"); }
                let bind = None;
                let nxt = None;
                if (self.matches("(")) {
                    self.emit(("GetLocal", exc_slot));
                    let n = 0;
                    while (True) {
                        self.parse_or(False);
                        n += 1;
                        if (not self.matches(",")) { break; }
                    }
                    if (self.matches("as")) { bind = self.expect_ident("expected a name after 'as'"); }
                    self.expect(")", "expected ')' after exception types");
                    self.emit(("ExcMatch", n));
                    nxt = self.emit(("JumpIfFalse", 0));
                } else {
                    saw_bare = True;
                }
                self.begin_scope();
                if (bind != None) {
                    self.emit(("GetLocal", exc_slot));
                    self.define_named(bind);
                }
                self.block();
                self.end_scope();
                if (fin != None) {
                    self.emit(("PopTry",));
                    self.cur().active.pop();
                    self.block_at(fin);
                    self.cur().active.append(("F", fin));
                }
                end_jumps.append(self.emit(("Jump", 0)));
                if (nxt != None) { self.patch(nxt); }
            }
            self.emit(("GetLocal", exc_slot));
            self.emit(("Raise",));
            self.cur().exc_slots.pop();
            self.end_scope();
            if (fin != None) { self.cur().active.pop(); }
        }
        if (fin_setup != None) {
            self.patch(fin_setup);
            self.handler_entry(boundary);
            self.block_at(fin);
            self.emit(("Raise",));
        }
        for (j in end_jumps) { self.patch(j); }
        self.pos = stmt_end;
    }

    def handler_entry(self, boundary) {
        self.emit(("CloseUpvals", boundary));
    }

    def raise_stmt(self) {
        self.advance();
        if (self.matches(";")) {
            let slots = self.cur().exc_slots;
            if (len(slots) == 0) { raise self.error_prev("bare 'raise' outside of an except block"); }
            self.emit(("GetLocal", slots[-1]));
        } else {
            self.expression();
            self.expect(";", "expected ';' after raise");
        }
        self.emit(("Raise",));
    }

    def break_stmt(self) {
        self.advance();
        if (len(self.cur().loops) == 0) { raise self.error_prev("'break' outside of a loop"); }
        self.expect(";", "expected ';' after 'break'");
        self.unwind_handlers(self.cur().loops[-1].active_depth);
        let j = self.emit(("Jump", 0));
        self.cur().loops[-1].breaks.append(j);
    }

    def continue_stmt(self) {
        self.advance();
        if (len(self.cur().loops) == 0) { raise self.error_prev("'continue' outside of a loop"); }
        let target = self.cur().loops[-1].continue_target;
        self.expect(";", "expected ';' after 'continue'");
        self.unwind_handlers(self.cur().loops[-1].active_depth);
        let j = self.emit(("Jump", 0 if target == -1 else target));
        self.cur().loops[-1].continues.append(j);
    }

    def expr_stmt(self) {
        let assigned = self.parse_or(True);
        self.expect(";", "expected ';' after statement");
        if (not assigned) { self.emit(("Pop",)); }
    }

    # ------------------------------------------------------------ expressions
    # Each level returns True if it compiled an assignment (statement level only).

    def expression(self) {
        if (self.check("yield")) { return self.yield_expr(); }
        let start_pos = self.pos;
        let f = self.cur();
        let prev_label = f.last_label;
        let code_len = self.label();
        self.parse_or(False);
        if (not self.check("if")) {
            if (f.last_label == code_len) { f.last_label = prev_label; }
            return;
        }
        # `a if c else b`: `a` was compiled first, but `c` must run first
        while (len(f.code) > code_len) {
            f.code.pop();
            f.lines.pop();
        }
        f.last_label = code_len;
        self.advance();
        self.parse_or(False);
        let jf = self.emit(("JumpIfFalse", 0));
        self.expect("else", "expected 'else' in conditional expression");
        let else_pos = self.pos;
        self.pos = start_pos;
        self.parse_or(False);
        let jend = self.emit(("Jump", 0));
        self.patch(jf);
        self.pos = else_pos;
        self.expression();
        self.patch(jend);
    }

    def parse_or(self, can_assign) {
        if (self.parse_and(can_assign)) { return True; }
        while (self.matches("or")) {
            let j = self.emit(("JumpIfTrueOrPop", 0));
            self.parse_and(False);
            self.patch(j);
        }
        return False;
    }

    def parse_and(self, can_assign) {
        if (self.parse_not(can_assign)) { return True; }
        while (self.matches("and")) {
            let j = self.emit(("JumpIfFalseOrPop", 0));
            self.parse_not(False);
            self.patch(j);
        }
        return False;
    }

    def parse_not(self, can_assign) {
        if (self.matches("not")) {
            self.parse_not(False);
            self.emit(("Not",));
            return False;
        }
        return self.parse_comparison(can_assign);
    }

    # (op name, negate) for a comparison operator at the cursor (consumed), or None
    def comparison_op(self) {
        let k = self.peek();
        let r = None;
        if (k in COMPARE) {
            r = (COMPARE[k], False);
        } elif (k == "not" and self.kind_at(self.pos + 1) == "in") {
            self.advance();
            r = ("In", True);
        } elif (k == "is" and self.kind_at(self.pos + 1) == "not") {
            self.advance();
            r = ("Is", True);
        } elif (k == "is") {
            r = ("Is", False);
        } else {
            return None;
        }
        self.advance();
        return r;
    }

    def is_comparison(self) {
        let k = self.peek();
        return k in COMPARE or k == "is" or (k == "not" and self.kind_at(self.pos + 1) == "in");
    }

    def parse_comparison(self, can_assign) {
        if (self.parse_bitor(can_assign)) { return True; }
        let c = self.comparison_op();
        if (c == None) { return False; }
        let op = c[0];
        let negate = c[1];
        let cleanups = [];
        while (True) {
            self.parse_bitor(False);
            let is_chain = self.is_comparison();
            if (is_chain) {
                self.emit(("Dup",));
                self.emit(("Rot3",));
            }
            self.emit((op,));
            if (negate) { self.emit(("Not",)); }
            if (not is_chain) { break; }
            cleanups.append(self.emit(("JumpIfFalseOrPop", 0)));
            let nxt = self.comparison_op();
            op = nxt[0];
            negate = nxt[1];
        }
        if (len(cleanups) > 0) {
            let end = self.emit(("Jump", 0));
            for (cl in cleanups) { self.patch(cl); }
            self.emit(("Swap",));
            self.emit(("Pop",));
            self.patch(end);
        }
        return False;
    }

    def parse_bitor(self, can_assign) {
        if (self.parse_bitxor(can_assign)) { return True; }
        while (self.check("|")) {
            self.advance();
            self.parse_bitxor(False);
            self.emit(("BitOr",));
        }
        return False;
    }

    def parse_bitxor(self, can_assign) {
        if (self.parse_bitand(can_assign)) { return True; }
        while (self.check("^")) {
            self.advance();
            self.parse_bitand(False);
            self.emit(("BitXor",));
        }
        return False;
    }

    def parse_bitand(self, can_assign) {
        if (self.parse_shift(can_assign)) { return True; }
        while (self.check("&")) {
            self.advance();
            self.parse_shift(False);
            self.emit(("BitAnd",));
        }
        return False;
    }

    def parse_shift(self, can_assign) {
        if (self.parse_additive(can_assign)) { return True; }
        while (True) {
            let k = self.peek();
            let op = None;
            if (k == "<<") { op = "Shl"; } elif (k == ">>") { op = "Shr"; } else { break; }
            self.advance();
            self.parse_additive(False);
            self.emit((op,));
        }
        return False;
    }

    def parse_additive(self, can_assign) {
        if (self.parse_multiplicative(can_assign)) { return True; }
        while (True) {
            let k = self.peek();
            let op = None;
            if (k == "+") { op = "Add"; } elif (k == "-") { op = "Sub"; } else { break; }
            self.advance();
            self.parse_multiplicative(False);
            self.emit((op,));
        }
        return False;
    }

    def parse_multiplicative(self, can_assign) {
        if (self.parse_unary(can_assign)) { return True; }
        while (True) {
            let k = self.peek();
            let op = None;
            if (k == "*") {
                op = "Mul";
            } elif (k == "/") {
                op = "Div";
            } elif (k == "//") {
                op = "FloorDiv";
            } elif (k == "%") {
                op = "Mod";
            } else {
                break;
            }
            self.advance();
            self.parse_unary(False);
            self.emit((op,));
        }
        return False;
    }

    def parse_unary(self, can_assign) {
        if (self.matches("-")) {
            self.parse_unary(False);
            self.emit(("Neg",));
            return False;
        }
        if (self.matches("+")) {
            self.parse_unary(False);
            return False;
        }
        if (self.matches("~")) {
            self.parse_unary(False);
            self.emit(("Invert",));
            return False;
        }
        return self.parse_power(can_assign);
    }

    def parse_power(self, can_assign) {
        if (self.parse_postfix(can_assign)) { return True; }
        if (self.matches("**")) {
            self.parse_unary(False);
            self.emit(("Pow",));
        }
        return False;
    }

    def del_end(self, d) {
        let k = self.peek();
        return d and (k == "," or k == ";");
    }

    def parse_postfix(self, can_assign) {
        let d = self.del_mode;
        self.del_mode = False;
        if (self.primary(can_assign)) { return True; }
        while (True) {
            let k = self.peek();
            if (k == "(") {
                self.advance();
                let a = self.arguments();
                if (a[0] == "Plain") {
                    self.emit(("Call", a[1]));
                } elif (a[0] == "Kw") {
                    self.emit(("CallKw", a[1], a[2]));
                } else {
                    self.emit(("CallEx",));
                }
            } elif (k == ".") {
                self.advance();
                let name = self.expect_ident("expected attribute name after '.'");
                if (self.check("(")) {
                    if (self.args_have_splat_after_paren()) {
                        self.emit(("GetAttr", name));
                        self.advance();
                        self.splat_arguments();
                        self.emit(("CallEx",));
                    } else {
                        self.advance();
                        let a = self.arguments();
                        if (a[0] == "Plain") {
                            self.emit(("Invoke", name, a[1]));
                        } else {
                            self.emit(("InvokeKw", name, a[1], a[2]));
                        }
                    }
                } elif (can_assign and self.peek() in ASSIGN) {
                    let kind = ASSIGN[self.peek()];
                    self.advance();
                    let f = self.cur();
                    let local_obj = None;
                    if (len(f.code) > 0 and f.code[-1][0] == "GetLocal" and f.last_label != len(f.code) - 1) {
                        local_obj = f.code[-1][1];
                    }
                    if (local_obj != None) {
                        f.code.pop();
                        f.lines.pop();
                        if (kind != None) {
                            self.emit(("GetLocalAttr", local_obj, name));
                            self.expression();
                            self.emit((kind,));
                        } else {
                            self.expression();
                        }
                        self.emit(("SetLocalAttr", local_obj, name));
                        return True;
                    }
                    if (kind != None) {
                        self.emit(("Dup",));
                        self.emit(("GetAttr", name));
                        self.expression();
                        self.emit((kind,));
                    } else {
                        self.expression();
                    }
                    self.emit(("SetAttr", name));
                    return True;
                } elif (self.del_end(d)) {
                    self.emit(("DelAttr", name));
                    return True;
                } else {
                    self.emit(("GetAttr", name));
                }
            } elif (k == "[") {
                self.advance();
                if (self.check(":")) {
                    self.emit(("None",));
                } else {
                    self.expression();
                }
                if (self.matches(":")) {
                    if (self.check(":") or self.check("]")) { self.emit(("None",)); } else { self.expression(); }
                    if (self.matches(":")) {
                        if (self.check(":") or self.check("]")) { self.emit(("None",)); } else { self.expression(); }
                    } else {
                        self.emit(("None",));
                    }
                    self.expect("]", "expected ']' after slice");
                    if (self.del_end(d)) {
                        self.emit(("DelSlice",));
                        return True;
                    }
                    if (can_assign and self.check("=")) {
                        self.advance();
                        self.expression();
                        self.emit(("SetSlice",));
                        return True;
                    }
                    if (can_assign and self.peek() in ASSIGN) {
                        raise self.error("augmented assignment to a slice isn't supported");
                    }
                    self.emit(("GetSlice",));
                    continue;
                }
                self.expect("]", "expected ']' after index");
                if (self.del_end(d)) {
                    self.emit(("DelIndex",));
                    return True;
                }
                if (can_assign and self.peek() in ASSIGN) {
                    let kind = ASSIGN[self.peek()];
                    self.advance();
                    if (kind != None) {
                        self.emit(("Dup2",));
                        self.emit(("GetIndex",));
                        self.expression();
                        self.emit((kind,));
                    } else {
                        self.expression();
                    }
                    self.emit(("SetIndex",));
                    return True;
                }
                self.emit(("GetIndex",));
            } else {
                break;
            }
        }
        return False;
    }

    # Call arguments after '(' up to and including ')': ("Plain", n),
    # ("Kw", n, const index of the names) or ("Splat",).
    def arguments(self) {
        if (self.args_have_splat()) { return self.splat_arguments(); }
        let n = 0;
        let names = [];
        if (not self.check(")") and self.paren_has_for()) {
            let elem_pos = self.pos;
            let code_len = self.label();
            self.expression();
            if (self.check("for")) {
                self.discard_since(code_len);
                self.comprehension(elem_pos, "List", ")");
                return ("Plain", 1);
            }
            self.discard_since(code_len);
            self.pos = elem_pos;
        }
        if (not self.check(")")) {
            while (True) {
                let is_kw = self.check("Ident") and self.kind_at(self.pos + 1) == "=";
                if (is_kw) {
                    let name = self.expect_ident("expected argument name");
                    if (name in names) { raise self.error_prev(f"keyword argument repeated: {name}"); }
                    self.advance();
                    self.expression();
                    names.append(name);
                } else {
                    if (len(names) > 0) { raise self.error("positional argument follows keyword argument"); }
                    self.expression();
                }
                n += 1;
                if (not self.matches(",") or self.check(")")) { break; }
            }
        }
        self.expect(")", "expected ')' after arguments");
        if (len(names) == 0) { return ("Plain", n); }
        let k = self.add_const(KwNames(names));
        return ("Kw", n, k);
    }

    def paren_has_for(self) {
        let depth = 0;
        let i = self.pos;
        while (True) {
            let k = self.toks[i][0];
            if (k == "(" or k == "[" or k == "{") {
                depth += 1;
            } elif (k == ")" or k == "]" or k == "}") {
                if (depth == 0) { return False; }
                depth -= 1;
            } elif (k == "for" and depth == 0) {
                return True;
            } elif (k == "EOF" or k == ";") {
                return False;
            }
            i += 1;
        }
    }

    def args_have_splat_after_paren(self) {
        self.pos += 1;
        let r = self.args_have_splat();
        self.pos -= 1;
        return r;
    }

    def args_have_splat(self) {
        let depth = 0;
        let i = self.pos;
        let at_start = True;
        while (True) {
            let k = self.toks[i][0];
            if (k == "(" or k == "[" or k == "{") {
                depth += 1;
                at_start = False;
            } elif (k == ")" or k == "]" or k == "}") {
                if (depth == 0) { return False; }
                depth -= 1;
            } elif (k == "," and depth == 0) {
                at_start = True;
                i += 1;
                continue;
            } elif ((k == "*" or k == "**") and depth == 0 and at_start) {
                return True;
            } elif (k == "EOF" or k == ";") {
                return False;
            } else {
                at_start = False;
            }
            i += 1;
        }
    }

    def splat_arguments(self) {
        self.emit(("BuildList", 0));
        let dict_started = False;
        while (not self.check(")")) {
            let is_kw = self.check("Ident") and self.kind_at(self.pos + 1) == "=";
            if (self.matches("**") or is_kw) {
                if (not dict_started) {
                    self.emit(("BuildDict", 0));
                    dict_started = True;
                }
                if (is_kw) {
                    let name = self.expect_ident("expected argument name");
                    self.advance();
                    let k = self.add_const(name);
                    self.emit(("Const", k));
                    self.expression();
                    self.emit(("ArgKw",));
                } else {
                    self.expression();
                    self.emit(("ArgMerge",));
                }
            } else {
                if (dict_started) { raise self.error("positional argument follows keyword argument"); }
                if (self.matches("*")) {
                    self.expression();
                    self.emit(("ArgExtend",));
                } else {
                    self.expression();
                    self.emit(("ArgPush",));
                }
            }
            if (not self.matches(",")) { break; }
        }
        self.expect(")", "expected ')' after arguments");
        if (not dict_started) { self.emit(("BuildDict", 0)); }
        return ("Splat",);
    }

    def primary(self, can_assign) {
        let t = self.advance();
        let k = t[0];
        if (k == "Int" or k == "Float" or k == "Str" or k == "Bytes") {
            let c = self.add_const(t[1]);
            self.emit(("Const", c));
        } elif (k == "True" or k == "False" or k == "None") {
            self.emit((k,));
        } elif (k == "Ident") {
            return self.variable(t[1], can_assign);
        } elif (k == "(") {
            if (self.matches(")")) {
                self.emit(("BuildTuple", 0));
                return False;
            }
            self.expression();
            if (self.matches(",")) {
                let n = 1;
                while (not self.check(")")) {
                    self.expression();
                    n += 1;
                    if (not self.matches(",")) { break; }
                }
                self.expect(")", "expected ')' after tuple items");
                self.emit(("BuildTuple", n));
            } else {
                self.expect(")", "expected ')' after expression");
            }
        } elif (k == "[") {
            self.list_literal();
        } elif (k == "{") {
            self.dict_literal();
        } elif (k == "lambda") {
            self.lambda_expr();
        } else {
            self.pos -= 1;
            raise self.error("expected an expression");
        }
        return False;
    }

    def variable(self, name, can_assign) {
        if (name == "super" and self.check("(") and self.resolve_local("super") == None) {
            return self.super_call();
        }
        let global_decl = name in self.cur().global_decls;
        let local = None if global_decl else self.resolve_local(name);
        if (can_assign and self.peek() in ASSIGN) {
            let kind = ASSIGN[self.peek()];
            self.advance();
            if (local == None) {
                let up = self.nonlocal_upvalue(name);
                if (up != None) {
                    if (up[1]) { raise self.const_err(name); }
                    if (kind != None) {
                        self.emit(("GetUpval", up[0]));
                        self.expression();
                        self.emit((kind,));
                    } else {
                        self.expression();
                    }
                    self.emit(("SetUpval", up[0]));
                    return True;
                }
            }
            if (local != None) {
                let slot = local[0];
                if (local[1]) { raise self.const_err(name); }
                if (kind == "Add") {
                    self.expression();
                    self.emit(("AddLocal", slot));
                } elif (kind != None) {
                    self.emit(("GetLocal", slot));
                    self.expression();
                    self.emit((kind,));
                    self.emit(("SetLocal", slot));
                } else {
                    self.expression();
                    self.emit(("SetLocal", slot));
                }
            } elif (self.cur().is_script or global_decl) {
                if (self.gconst.get(name, False)) { raise self.const_err(name); }
                if (kind != None) {
                    self.emit(("GetGlobal", name));
                    self.expression();
                    self.emit((kind,));
                } else {
                    self.expression();
                }
                self.emit(("SetGlobal", name));
            } else {
                if (kind != None) {
                    let hint = "";
                    if (self.resolve_upvalue(len(self.fns) - 1, name) != None) {
                        hint = f"add 'nonlocal {name};' to modify the enclosing function's variable";
                    } else {
                        hint = f"add 'global {name};' to modify the global";
                    }
                    raise self.error_prev(f"'{name}' is not a local variable; {hint}");
                }
                self.expression();
                let s = self.declare_local(name, False);
                self.emit(("SetLocal", s));
            }
            return True;
        }
        if (local != None) {
            self.emit(("GetLocal", local[0]));
            return False;
        }
        if (not global_decl) {
            let up = self.resolve_upvalue(len(self.fns) - 1, name);
            if (up != None) {
                self.emit(("GetUpval", up[0]));
                return False;
            }
        }
        self.emit(("GetGlobal", name));
        return False;
    }

    def list_literal(self) {
        if (self.matches("]")) {
            self.emit(("BuildList", 0));
            return;
        }
        let elem_pos = self.pos;
        let code_len = self.label();
        self.expression();
        if (self.check("for")) {
            self.discard_since(code_len);
            return self.comprehension(elem_pos, "List", "]");
        }
        let n = 1;
        while (self.matches(",")) {
            if (self.check("]")) { break; }
            self.expression();
            n += 1;
        }
        self.expect("]", "expected ']' after list items");
        self.emit(("BuildList", n));
    }

    def discard_since(self, code_len) {
        let f = self.cur();
        while (len(f.code) > code_len) {
            f.code.pop();
            f.lines.pop();
        }
        f.last_label = code_len;
    }

    def comprehension(self, elem_pos, kind, close) {
        self.advance();
        let names = self.loop_targets();
        self.expect("in", "expected 'in' in comprehension");
        self.begin_scope();
        let list_slot = self.declare_local("(list)", False);
        if (kind == "List") {
            self.emit(("BuildList", 0));
        } elif (kind == "Set") {
            self.emit(("BuildSet", 0));
        } else {
            self.emit(("BuildDict", 0));
        }
        self.emit(("SetLocal", list_slot));
        self.parse_or(False);
        let iter_slot = self.declare_iter_slots();
        self.emit(("GetIter", iter_slot));
        let tv = self.declare_loop_targets(names);
        let var_slot = tv[0];
        let exit = self.emit(("ForIter", iter_slot, var_slot, 0));
        let body = self.label();
        self.emit_unpack_targets(var_slot, tv[1]);
        let skip = None;
        if (self.matches("if")) {
            self.parse_or(False);
            skip = self.emit(("JumpIfFalse", 0));
        }
        self.expect(close, "expected the end of the comprehension");
        let end_pos = self.pos;
        self.pos = elem_pos;
        self.expression();
        if (kind == "List") {
            self.emit(("ListAppend", list_slot));
        } elif (kind == "Set") {
            self.emit(("SetAdd", list_slot));
        } else {
            self.expect(":", "expected ':' in dict comprehension");
            self.expression();
            self.emit(("DictSet", list_slot));
        }
        if (skip != None) {
            self.patch(skip);
        } else {
            self.label();
        }
        self.emit(("ForLoop", iter_slot, var_slot, body));
        self.patch(exit);
        self.emit(("GetLocal", list_slot));
        self.pos = end_pos;
        self.end_scope();
    }

    def dict_literal(self) {
        let n = 0;
        if (not self.check("}")) {
            let elem_pos = self.pos;
            let code_len = self.label();
            self.expression();
            if (self.check("for")) {
                self.discard_since(code_len);
                return self.comprehension(elem_pos, "Set", "}");
            }
            if (not self.check(":")) {
                n = 1;
                while (self.matches(",")) {
                    if (self.check("}")) { break; }
                    self.expression();
                    n += 1;
                }
                self.expect("}", "expected '}' after set items");
                self.emit(("BuildSet", n));
                return;
            }
            self.expect(":", "expected ':' between dict key and value");
            self.expression();
            if (self.check("for")) {
                self.discard_since(code_len);
                return self.comprehension(elem_pos, "Dict", "}");
            }
            n = 1;
            if (not self.matches(",") or self.check("}")) {
                self.expect("}", "expected '}' after dict items");
                self.emit(("BuildDict", n));
                return;
            }
            while (True) {
                self.expression();
                self.expect(":", "expected ':' between dict key and value");
                self.expression();
                n += 1;
                if (not self.matches(",") or self.check("}")) { break; }
            }
        }
        self.expect("}", "expected '}' after dict items");
        self.emit(("BuildDict", n));
    }
}

# ---------------------------------------------------------------------------- .fbc writer

const OPCODES = bytecode.opcodes;
const OPERANDS = bytecode.operands;

# IEEE 754 bytes of a float (little endian), exactly
def f64_bytes(x) {
    let bits = 0;
    let neg = False;
    if (x != x) {
        bits = 0x7ff8000000000000;
    } else {
        neg = x < 0 or (x == 0 and str(x).startswith("-"));
        let a = abs(x);
        if (a == float("inf")) {
            bits = 0x7ff0000000000000;
        } elif (a != 0) {
            let e = 0;
            let m = a;
            while (m >= 2.0) { m = m / 2.0; e += 1; }
            while (m < 1.0) { m = m * 2.0; e -= 1; }
            if (e >= -1022) {
                bits = ((e + 1023) << 52) | int((m - 1.0) * 4503599627370496.0);
            } else {
                bits = int(m * 2.0 ** (e + 1074));
            }
        }
    }
    let low = bits & 0xffffffffffffff;
    let high = (bits >> 56) | (0x80 if neg else 0);
    return low.to_bytes(7, "little") + bytes([high]);
}

class Writer {
    def init(self) {
        self.out = bytearray();
        self.sym_ids = {};
        self.sym_names = [];
        self.glob_ids = {};
        self.glob_names = [];
    }

    def u32(self, v) { self.out.extend(v.to_bytes(4, "little")); }

    def text(self, s) {
        let b = s.encode();
        self.u32(len(b));
        self.out.extend(b);
    }

    def sym(self, name) {
        let i = self.sym_ids.get(name);
        if (i != None) { return i; }
        i = len(self.sym_names);
        self.sym_names.append(name);
        self.sym_ids[name] = i;
        return i;
    }

    def glob(self, name) {
        let i = self.glob_ids.get(name);
        if (i != None) { return i; }
        i = len(self.glob_names);
        self.glob_names.append(name);
        self.glob_ids[name] = i;
        return i;
    }

    def konst(self, v) {
        let out = self.out;
        if (v is None) {
            out.append(0);
        } elif (isinstance(v, bool)) {
            out.append(2 if v else 1);
        } elif (isinstance(v, int)) {
            out.append(3);
            out.extend(v.to_bytes(8, "little", signed=True));
        } elif (isinstance(v, float)) {
            out.append(4);
            out.extend(f64_bytes(v));
        } elif (isinstance(v, str)) {
            out.append(5);
            self.text(v);
        } elif (isinstance(v, bytes)) {
            out.append(6);
            self.u32(len(v));
            out.extend(v);
        } elif (isinstance(v, FnProto)) {
            out.append(7);
            self.function(v);
        } else {
            out.append(8);
            self.u32(len(v.names));
            for (n in v.names) { self.u32(self.sym(n)); }
        }
    }

    def function(self, f) {
        let out = self.out;
        self.text(f.name);
        self.u32(f.arity);
        self.u32(f.num_locals);
        out.append((1 if f.varargs else 0) | (2 if f.varkw else 0) | (4 if f.dynamic else 0));
        self.u32(len(f.params));
        for (p in f.params) { self.u32(self.sym(p)); }
        self.u32(len(f.captures));
        for (c in f.captures) {
            out.append(1 if c[0] else 0);
            self.u32(c[1]);
        }
        self.u32(len(f.defaults));
        for (d in f.defaults) { self.konst(d); }
        self.u32(len(f.consts));
        for (c in f.consts) { self.konst(c); }
        self.u32(len(f.code));
        let lines = f.lines;
        for (i in range(len(f.code))) {
            let op = f.code[i];
            let name = op[0];
            out.append(OPCODES[name]);
            let kinds = OPERANDS[name];
            for (j in range(len(kinds))) {
                let a = op[j + 1];
                let kd = kinds[j];
                if (kd == "S") { a = self.sym(a); } elif (kd == "G") { a = self.glob(a); }
                out.extend(a.to_bytes(4, "little"));
            }
            out.extend(lines[i].to_bytes(4, "little"));
        }
    }
}

# A compiled module -> the contents of its .fbc file.
def encode(proto) {
    let w = Writer();
    w.function(proto);
    let body = w.out;
    let head = bytearray(bytecode.MAGIC);
    head.extend(bytecode.VERSION.to_bytes(2, "little"));
    w.out = head;
    for (table in [w.sym_names, w.glob_names]) {
        w.u32(len(table));
        for (s in table) { w.text(s); }
    }
    head.extend(body);
    return bytes(head);
}

# Source -> .fbc contents. Raises CompileError (with the message the VM
# prints) for mistakes. `consts`: names of global constants defined earlier.
def compile(src, name="<string>", consts=None) {
    let c = Compiler(lex(src), consts);
    return encode(c.compile());
}
