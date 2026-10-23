class Vec2 {
    def init(self, x, y) { self.x = x; self.y = y; }
    def __add__(self, o) { return Vec2(self.x + o.x, self.y + o.y); }
    def __sub__(self, o) { return Vec2(self.x - o.x, self.y - o.y); }
    def __mul__(self, k) { return Vec2(self.x * k, self.y * k); }
    def __rmul__(self, k) { return Vec2(self.x * k, self.y * k); }
    def __truediv__(self, k) { return Vec2(self.x / k, self.y / k); }
    def __neg__(self) { return Vec2(-self.x, -self.y); }
    def __eq__(self, o) { return isinstance(o, Vec2) and self.x == o.x and self.y == o.y; }
    def __lt__(self, o) { return self.length() < o.length(); }
    def __len__(self) { return 2; }
    def __getitem__(self, i) { return [self.x, self.y][i]; }
    def __iter__(self) { return [self.x, self.y]; }
    def __repr__(self) { return f"Vec2({self.x}, {self.y})"; }
    def length(self) { return (self.x ** 2 + self.y ** 2) ** 0.5; }
}
let a = Vec2(1, 2);
let b = Vec2(3, 4);
print(a + b, a - b, a * 3, 2 * a, b / 2, -a);
print(a == Vec2(1, 2), a != Vec2(1, 2), a == b, a != b, a < b, b > a);
print(len(a), a[0], a[-1], list(a), str(b), repr(b), f"pos={a}");
let x, y = b;
print(x, y, [v for v in b]);
print(sorted([b, a, Vec2(0, 1)]), min([b, a]), max(b, a));
print(Vec2(1, 2) in [b, Vec2(1, 2)], a in [b]);
let total = Vec2(0, 0);
for (v in [a, b, a]) { total = total + v; }
total += Vec2(10, 10);
print(total, [a, b], {"pos": a}, (a,));

class Counter {
    def init(self) { self.n = 0; }
    def __call__(self, step = 1) { self.n += step; return self.n; }
    def __bool__(self) { return self.n > 0; }
    def __contains__(self, v) { return v <= self.n; }
    def __str__(self) { return "Counter(" + str(self.n) + ")"; }
}
let c = Counter();
print(bool(c), "zero" if not c else "nonzero");
c(); c(5);
print(c, c.n, bool(c), 3 in c, 99 in c, "yes" if c else "no");

class Grid {
    def init(self) { self.cells = {}; }
    def __getitem__(self, key) { return self.cells.get(key, "."); }
    def __setitem__(self, key, v) { self.cells[key] = v; }
}
let g = Grid();
g[(1, 2)] = "#";
print(g[(1, 2)], g[(0, 0)]);

class Money {
    def init(self, c) { self.c = c; }
    def __add__(self, o) { return Money(self.c + (o.c if isinstance(o, Money) else o)); }
    def __radd__(self, o) { return Money(self.c + o); }
    def __str__(self) { return "$" + str(self.c); }
}
print(sum([Money(1), Money(2), Money(3)]), 10 + Money(5), Money(5) + 10);
try { print(a + 1); } except (AttributeError as e) { print("error:", e); }
try { print(Counter() < Counter()); } except (TypeError as e) { print("error:", e); }
