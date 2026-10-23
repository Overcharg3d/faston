let xs = [3, 1, 4, 1, 5, 9, 2, 6];
print({x: x * x for x in xs if x > 2}, {x % 3 for x in xs}, sum(x for x in xs), max(x * 2 for x in xs), sorted(x for x in xs if x % 2));
let pairs = [("a", 1), ("b", 2)];
print({k: v for k, v in pairs}, {v: k for k, v in {"x": 1, "y": 2}.items()}, list(x for x in "ab"), {}, {1: 2}, {1, 2});
print(any(x > 8 for x in xs), all(x > 0 for x in xs), ",".join(str(x) for x in xs[:3]));
