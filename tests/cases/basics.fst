# numbers
print(7 + 2, 7 - 2, 7 * 2, 7 / 2, 7 // 2, 7 % 2, 2 ** 10);
print(-7 // 2, -7 % 3, 7 % -3, -7.5 // 2, 7.5 % 2, 2 ** -1);
print(1 + 2.5, 3 * 1.5, 10 / 4, 1e3, 0.1 + 0.2, 0x1F, 1_000_000);
print(-2 ** 2, (-2) ** 2, 2 ** 3 ** 2);
print(abs(-5), abs(-2.5), round(2.5), round(3.5), round(2.675, 2), int(3.9), int(-3.9), float(2));
print(int("42"), float("1.5"), int(True), str(12) + "!", bool(0), bool("x"), bool([]));
# comparisons
print(1 < 2, 2 <= 2, 3 > 4, 4 >= 4, 1 == 1.0, 1 != 2, "a" < "b", [1, 2] < [1, 3], [1, 2] == [1, 2]);
print(None == None, None != 0, True == 1, 0.5 < 1);
# logic returns operands (like Python)
print(0 or "default", "x" and "y", None or 0, not None, not 5);
print(1 && 2, 0 || 3, !True);
# truthiness
let things = [0, 1, 0.0, "", "a", [], [0], {}, {"k": 1}, None, True, False];
print([bool(t) for t in things]);
# ternary
let n = 5;
print("big" if n > 3 else "small", "even" if n % 2 == 0 else "odd");
print(1 if False else 2 if False else 3);
# types
print(type(1), type(1.5), type("s"), type([]), type({}), type(None), type(True), type(print));
# multiple assignment
let a, b = 1, 2;
a, b = b, a;
print(a, b);
x, y, z = [10, 20, 30];
print(x + y + z);
# constants
const LIMIT = 100;
print(LIMIT * 2);
# big ints and overflow guard
print(2 ** 62, 9223372036854775807);
