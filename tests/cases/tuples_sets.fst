print((1, 2), (1,), (), {1, 2, 2, 3}, {}, 5 & 3, 5 | 3, 5 ^ 3, ~5, 1 << 4, 256 >> 2, -9 >> 1);
let t = (3, "a", 4.5);
print(t[0], t[-1], t[1:], len(t), "a" in t, t + (9,), t * 2, (1, 2) < (1, 3), (1, 2) == (1, 2));
let d = {(0, 0): "origin", (1, 2): "p"};
print(d[(1, 2)], (0, 0) in d);
let s = {1, 2, 3};
print(s | {4}, s & {2, 3, 9}, s - {1}, s ^ {3, 4}, {1, 2} <= s, 2 in s);
def mm(xs) { return min(xs), max(xs); }
let r = mm([4, 1, 7]);
print(r, type(r));
for (a, b in [(1, 2), (3, 4)]) { print(a + b); }
flags = 0;
flags |= 4; flags |= 1; flags &= ~1;
print(flags);
