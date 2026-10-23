# Cases aimed at the native compiler: typed locals, speculation, deoptimization,
# exceptions through compiled frames, and calls in every direction.

def sum_to(n) {
    let t = 0;
    for (i in range(n)) { t += i; }
    return t;
}
print(sum_to(10), sum_to(0), sum_to(-5));

def steps() {
    let out = [];
    for (i in range(10, 0, -3)) { out.append(i); }
    for (i in range(0, 10, 4)) { out.append(i); }
    return out;
}
print(steps());

# int overflow is an error, even in a register-only loop
def overflow() {
    let x = 9223372036854775800;
    let i = 0;
    try {
        while (True) { x += 1; i += 1; }
    } except (OverflowError as e) {
        print("overflow after", i, "steps:", e);
    }
    return x;
}
print(overflow());

# typed float and bool locals
def floats() {
    let x = 0.5;
    let flag = False;
    for (k in range(5)) { x = x * 2.0 + 0.25; flag = not flag; }
    return x, flag;
}
print(floats());

# int arithmetic edge cases inside compiled code
def arith(a, b) {
    return [a // b, a % b, a / b, -a, a & b, a | b, a ^ b, a >> 2, a << 3, a ** 2];
}
print(arith(17, 5), arith(-17, 5), arith(17, -5));
def ints() {
    let m = -9223372036854775807 - 1;
    let r = [];
    try { r.append(-m); } except (OverflowError) { r.append("neg overflow"); }
    try { r.append(m // -1); } except (OverflowError) { r.append("div overflow"); }
    try { r.append(1 // 0); } except (ZeroDivisionError as e) { r.append(str(e)); }
    try { r.append(1 % 0); } except (ZeroDivisionError as e) { r.append(str(e)); }
    try { r.append(1.0 / 0); } except (ZeroDivisionError as e) { r.append(str(e)); }
    r.append(7 >> 70);
    r.append(-7 >> 70);
    return r;
}
print(ints());

# rebinding `range` makes compiled loops fall back to the interpreter
def count3() {
    let t = 0;
    for (i in range(3)) { t += i; }
    return t;
}
print(count3());
def fake_range(n) { return [10, 20, 30]; }
let real_range = range;
range = fake_range;
print(count3());
range = real_range;
print(count3());

# exceptions unwinding through several compiled frames
def inner(x) {
    if (x > 2) { raise ValueError(f"too big: {x}"); }
    return x;
}
def middle(x) { return inner(x) * 10; }
def outer() {
    let got = [];
    for (i in range(5)) {
        try {
            got.append(middle(i));
        } except (ValueError as e) {
            got.append(str(e));
        } finally {
            got.append("f" + str(i));
        }
    }
    return got;
}
print(outer());

# loop counter seen by an exception handler
def find_bad(xs) {
    let i = 0;
    try {
        for (k in range(len(xs))) { i = k; xs[k] = 100 // xs[k]; }
    } except (ZeroDivisionError) {
        return "zero at " + str(i);
    }
    return xs;
}
print(find_bad([1, 2, 5]), find_bad([4, 0, 1]));

# closures, upvalues and calls through built-ins
def counter() {
    let n = 0;
    def bump(by = 1) { nonlocal n; n += by; return n; }
    return bump;
}
let c = counter();
c(); c(5);
print(c(), sorted([3, 1, 2], key=lambda (v) -v), list(map(lambda (v) v * v, range(4))));

# methods, inheritance, operator methods, instances created in hot loops
class V {
    def init(self, x, y) { self.x = x; self.y = y; }
    def __add__(self, o) { return V(self.x + o.x, self.y + o.y); }
    def __eq__(self, o) { return isinstance(o, V) and self.x == o.x and self.y == o.y; }
    def len2(self) { return self.x * self.x + self.y * self.y; }
}
class W(V) {
    def len2(self) { return 2 * super_len(self); }
}
def super_len(v) { return v.x * v.x + v.y * v.y; }
def vsum(n) {
    let p = V(0, 0);
    for (i in range(n)) { p = p + V(i, 1); }
    return p;
}
let p = vsum(100);
print(p.x, p.y, p == V(4950, 100), p != None, None == p, W(1, 2).len2(), V(3, 4).len2());

# attribute caches seeing different classes at one site
def total_x(objs) {
    let t = 0;
    for (o in objs) { t += o.x; }
    return t;
}
class A { def init(self) { self.x = 1; } }
class B { def init(self) { self.pad = 0; self.x = 2; } }
print(total_x([A(), B(), A(), B(), B()]));

# list fast paths: indexing, assignment, append, negative indices
def lists() {
    let xs = [];
    for (i in range(6)) { xs.append(i * i); }
    xs[0] = -1;
    xs[-1] = 99;
    let t = (1, 2, 3);
    return xs, xs[2], xs[-2], t[1], [x for x in xs if x % 2 == 0];
}
print(lists());

# deep recursion works natively
def depth(n) {
    if (n == 0) { return 0; }
    return 1 + depth(n - 1);
}
print(depth(5000));
