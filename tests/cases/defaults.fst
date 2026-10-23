const BASE = 10;
def f(a, b = BASE * 2, c = [1, 2], d = "lit") { return [a, b, c, d]; }
print(f(1), f(1, 2), f(1, 2, 3, 4));
def g(cb = lambda (x) x + 1) { return cb(1); }
print(g(), g(lambda (x) x * 100));
let k = 5;
def h(x = k) { return x; }
k = 6;
print(h());          # 5: evaluated when the def ran, like Python
def outer(n) { def inner(m = n * 2) { return m; } return inner; }
print(outer(4)(), outer(7)(), outer(7)(1));
class P { def init(self, x = BASE, y = BASE + 1) { self.x = x; self.y = y; } }
print(P().x, P(1).y, P(1, 2).y);
