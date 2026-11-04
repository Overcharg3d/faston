# is / del / slice assignment / super() / decorators / properties / with / generators
# (expected output generated with CPython)

# ---- is, is not
let a = [1, 2];
let b = a;
print(a is b, a is [1, 2], a is not b, None is None, a is not None, True is True);

# ---- del
let d = {"x": 1, "y": 2, "z": 3};
del d["y"];
let xs = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
del xs[0], xs[-1];
print(d, xs);
del xs[1:3];
print(xs);
del xs[::2];
print(xs);
class Box {
    def init(self) { self.v = 1; self.w = 2; }
}
let bx = Box();
del bx.v;
print(hasattr(bx, "v"), hasattr(bx, "w"));
gone = 5;
del gone;
try {
    print(gone);
} except (NameError) {
    print("gone is gone");
}

# ---- slice assignment
let ys = [1, 2, 3, 4, 5];
ys[1:3] = ["a", "b", "c"];
print(ys);
ys[:2] = [];
print(ys);
ys[::2] = [10, 20];
print(ys);
ys[len(ys):] = [99];
print(ys);

# ---- super()
class Animal {
    def init(self, name) { self.name = name; }
    def speak(self) { return f"{self.name} makes a sound"; }
}
class Dog(Animal) {
    def init(self, name, trick = "sit") {
        super().init(name);
        self.trick = trick;
    }
    def speak(self) { return super().speak() + " (woof)"; }
}
let dog = Dog("Rex", trick="roll");
print(dog.speak(), dog.trick);

# ---- decorators
def shout(f) {
    def wrapper(*args) { return f(*args).upper() + "!"; }
    return wrapper;
}
def repeat(n) {
    def deco(f) {
        def wrapper(x) { return [f(x) for _ in range(n)]; }
        return wrapper;
    }
    return deco;
}
@shout
def greet(name) { return "hello " + name; }
@repeat(3)
def twice(x) { return x * 2; }
@shout
@shout
def hey() { return "hey"; }
print(greet("ann"), twice(4), hey());

# ---- class attributes, @staticmethod, @classmethod, @property
class Temp {
    unit = "C";
    count = 0;
    def init(self, c) { self._c = c; Temp.bump(); }
    @staticmethod
    def bump() { Temp.count = Temp.count + 1; }
    @classmethod
    def freezing(cls) { return cls(0); }
    @property
    def celsius(self) { return self._c; }
    @celsius.setter
    def celsius(self, v) {
        if (v < -273.15) { raise ValueError("too cold"); }
        self._c = v;
    }
    @property
    def fahrenheit(self) { return self._c * 9 / 5 + 32; }
}
let t = Temp(25);
t.celsius = 30;
print(t.celsius, t.fahrenheit, t.unit, Temp.unit);
let f = Temp.freezing();
print(f.celsius, Temp.count, type(f));
try {
    t.celsius = -300;
} except (ValueError as e) {
    print("ValueError:", e);
}
try {
    t.fahrenheit = 1;
} except (AttributeError) {
    print("no setter");
}

# ---- with
class Managed {
    def init(self, name, swallow = False) { self.name = name; self.swallow = swallow; }
    def __enter__(self) { print("enter", self.name); return self; }
    def __exit__(self, kind, exc, tb) {
        print("exit", self.name, kind is None, exc);
        return self.swallow;
    }
}
with (Managed("a") as m) {
    print("body", m.name);
}
with (Managed("outer"), Managed("inner") as inner) {
    print("nested body", inner.name);
}
with (Managed("swallower", True)) {
    raise ValueError("ignored");
}
try {
    with (Managed("raiser")) {
        raise ValueError("k");
    }
} except (ValueError as e) {
    print("propagated", e);
}
def early() {
    with (Managed("early")) {
        return "returned";
    }
}
print(early());
for (i in range(3)) {
    with (Managed(f"loop{i}")) {
        if (i == 1) { break; }
    }
}

# ---- generators
def count_up(n) {
    let i = 0;
    while (i < n) {
        yield i;
        i += 1;
    }
}
print(list(count_up(5)), sum(count_up(101)));
for (v in count_up(3)) {
    print("got", v);
}
def fib() {
    let x, y = 0, 1;
    while (True) {
        yield x;
        x, y = y, x + y;
    }
}
let fibs = [];
for (n in fib()) {
    if (n > 100) { break; }
    fibs.append(n);
}
print(fibs);
let g = count_up(2);
print(next(g), next(g), next(g, "done"));
try {
    next(g);
} except (StopIteration) {
    print("StopIteration");
}
def echo() {
    let total = 0;
    while (True) {
        let got = yield total;
        if (got is None) { return; }
        total += got;
    }
}
let e = echo();
print(next(e), e.send(5), e.send(10));
def chain(*its) {
    for (it in its) {
        yield from it;
    }
}
print(list(chain([1, 2], "ab", count_up(2))));
def guarded() {
    try {
        yield 1;
        yield 2;
    } finally {
        print("cleanup");
    }
}
print(list(guarded()));
def pairs(d) {
    for (k, v in d.items()) {
        yield k, v;
    }
}
print(dict(pairs({"a": 1, "b": 2})), 3 in count_up(10), type(count_up(1)));
let it = iter([1, 2, 3]);
print(next(it), list(it));
class Countdown {
    def init(self, n) { self.n = n; }
    def __iter__(self) { return self; }
    def __next__(self) {
        if (self.n <= 0) { raise StopIteration; }
        self.n -= 1;
        return self.n + 1;
    }
}
print(list(Countdown(3)), [x * 10 for x in Countdown(2)]);
let squares = sum(x * x for x in range(4));
print(sorted(count_up(4), reverse=True), max(count_up(7)), squares);
