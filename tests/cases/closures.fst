# counters: each closure has its own captured variable
def make_counter(start = 0) {
    let n = start;
    def inc(step = 1) {
        nonlocal n;
        n += step;
        return n;
    }
    return inc;
}
let a = make_counter();
let b = make_counter(100);
print(a(), a(), a(5), b(), a());

# two closures sharing one variable
def make_account() {
    let balance = 0;
    def deposit(x) { nonlocal balance; balance += x; return balance; }
    def get() { return balance; }
    return [deposit, get];
}
let dep, get = make_account();
dep(10); dep(5);
print(get());

# lambdas
let sq = lambda (x) x * x;
let add = lambda (x, y = 10) x + y;
print(sq(7), add(1), add(1, 2), (lambda () "now")());
print(sorted(["ccc", "a", "bb"], lambda (s) len(s)));
print(map(lambda (x) x * 2, [1, 2, 3]), filter(lambda (x) x % 2, range(10)));
let pts = [[3, 1], [1, 2], [2, 0]];
pts.sort(lambda (p) p[1]);
print(pts);

# block-bodied lambda
let classify = lambda (n) {
    if (n < 0) { return "neg"; }
    return "pos";
};
print(classify(-3), classify(3));

# closure capturing a parameter, returned and called later
def adder(k) { return lambda (x) x + k; }
let add5 = adder(5);
let add9 = adder(9);
print(add5(1), add9(1), [adder(i)(0) for i in range(4)]);

# nested three levels (upvalue of an upvalue)
def outer() {
    let x = "outer";
    def middle() {
        def inner() { return x + "!"; }
        return inner;
    }
    return middle()();
}
print(outer());

# recursion through a captured local
def run() {
    def fact(n) { if (n <= 1) { return 1; } return n * fact(n - 1); }
    return fact(10);
}
print(run());

# late binding in loops (like Python): all see the final value
def late() {
    let fs = [];
    for (i in range(3)) { fs.append(lambda () i); }
    return [f() for f in fs];
}
print(late());

# capture per iteration via a default-free helper
def per_iter() {
    let fs = [];
    for (i in range(3)) {
        let j = i * 10;
        fs.append(lambda () j);
    }
    return [f() for f in fs];
}
print(per_iter());

# break/continue out of a loop whose body has captured variables
def with_break() {
    let fs = [];
    for (i in range(10)) {
        let v = i;
        fs.append(lambda () v);
        if (i == 1) { continue; }
        if (i == 3) { break; }
    }
    let w = 0;
    while (True) {
        let local = w;
        fs.append(lambda () local);
        w += 1;
        if (w > 1) { break; }
    }
    return [f() for f in fs];
}
print(with_break());

# closures as methods / stored in objects
class Button {
    def init(self, label) {
        self.label = label;
        self.clicks = 0;
        self.on_click = lambda () self.label + " clicked";
    }
}
let btn = Button("OK");
print(btn.on_click());

# event-handler style registry
let handlers = {};
def on(event, fn) { handlers[event] = fn; }
let total = 0;
on("hit", lambda (dmg) dmg * 2);
on("heal", lambda (amt) -amt);
for (ev, amt in [["hit", 5], ["heal", 3], ["hit", 1]]) {
    total += handlers[ev](amt);
}
print(total);

# closures surviving the garbage collector while their frames are gone
def make_many(n) { return [adder(i) for i in range(n)]; }
let many = make_many(2000);
let junk = [[i] * 10 for i in range(20000)];
print(sum([f(1) for f in many]));

# builtins that take functions
print(any([0, 0, 1]), all([1, 2, 0]), list(enumerate(["a", "b"], 1)), zip([1, 2, 3], "ab"));
print(callable(sq), callable(5), sorted([3, 1, 2]), type(sq));
for (i, ch in enumerate("hi")) { print(i, ch); }
