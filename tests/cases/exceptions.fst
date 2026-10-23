# basic catch
try {
    let d = {"a": 1};
    print(d["b"]);
} except (KeyError as e) {
    print("missing key", e, repr(e), type(e));
}

# several types, else, finally
def parse(s) {
    try {
        n = int(s);
    } except (ValueError, TypeError as e) {
        print("bad:", e);
        return None;
    } else {
        print("parsed", n);
        return n;
    } finally {
        print("finally for", repr(s));
    }
}
print(parse("12"), parse("x"));

# hierarchy: LookupError catches IndexError, Exception catches everything
try { [1][5]; } except (LookupError as e) { print("lookup:", e); }
try { 1 / 0; } except (Exception as e) { print(type(e), isinstance(e, ArithmeticError)); }

# user-defined exceptions, raise with class or instance, custom __str__
class GameError(Exception) { pass; }
class OutOfMana(GameError) {
    def init(self, need, have) {
        GameError.init(self, "not enough mana");
        self.need = need;
        self.have = have;
    }
    def __str__(self) { return f"need {self.need}, have {self.have}"; }
}
def cast(mana) {
    if (mana < 10) { raise OutOfMana(10, mana); }
    return "fireball";
}
try { cast(3); } except (GameError as e) { print("caught", type(e), e, e.need); }
try { raise GameError; } except (GameError as e) { print("class raise:", repr(e)); }

# re-raise, nested try, finally ordering
def risky() {
    try {
        try {
            raise ValueError("inner");
        } finally {
            print("inner finally");
        }
    } except (ValueError as e) {
        print("outer caught", e);
        raise;
    }
}
try { risky(); } except (ValueError as e) { print("re-raised:", e); }

# exceptions through function calls and callbacks (sorted key)
def bad_key(x) { if (x == 3) { raise RuntimeError("bad item"); } return x; }
try { sorted([5, 3, 1], bad_key); } except (RuntimeError as e) { print("from callback:", e); }

# break / continue / return through finally
def loop_test() {
    let log = [];
    for (i in range(5)) {
        try {
            if (i == 1) { continue; }
            if (i == 3) { break; }
            log.append(i);
        } finally {
            log.append(f"f{i}");
        }
    }
    return log;
}
print(loop_test());

def ret_test() {
    try { return "from try"; } finally { print("cleanup runs"); }
}
print(ret_test());

# exception inside except handler still runs finally
def handler_raises() {
    try {
        try { raise KeyError("k"); }
        except (KeyError) { raise ValueError("from handler"); }
        finally { print("finally after handler error"); }
    } except (ValueError as e) { print("got", e); }
}
handler_raises();

# bare except, assertion, recursion errors are catchable
try { assert 1 == 2, "nope"; } except { print("bare except caught assertion"); }
def deep(n) { return deep(n + 1); }
try { deep(0); } except (RecursionError as e) { print("recursion caught:", e); }
print("still running", deep != None);

# exceptions as values
let errs = [];
for (v in ["1", "two", "3"]) {
    try { errs.append(int(v)); } except (ValueError as e) { errs.append(type(e)); }
}
print(errs);

# loop with try is still correct many times (handler stack balance)
let total = 0;
for (i in range(10000)) {
    try {
        if (i % 1000 == 0) { raise ValueError(str(i)); }
        total += 1;
    } except (ValueError) { total += 100; }
}
print(total);
