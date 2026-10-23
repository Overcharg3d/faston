let xs = [5, 3, 8, 1];
xs.append(9);
xs.insert(0, 0);
xs.extend([7, 7]);
print(xs, len(xs), xs[0], xs[-1], xs.index(8), xs.count(7));
print(xs.pop(), xs.pop(0), xs);
xs.remove(8);
xs.sort();
print(xs, sorted([3, 1, 2]), sorted("cab"));
xs.reverse();
print(xs, min(xs), max(xs), sum(xs), max(4, 9, 2), min([2.5, -1]));
let ys = xs.copy();
ys[0] = 100;
ys[1] += 1;
print(xs[0], ys[0], ys[1]);
print([1, 2] + [3], [0] * 3, 3 in xs, 42 in xs, 42 not in xs);
xs.clear();
print(xs, len(xs));

# slicing
let nums = list(range(10));
print(nums[2:5], nums[:3], nums[7:], nums[::3], nums[::-1], nums[-3:], nums[1:-1:2], nums[8:2:-2]);

# comprehensions
print([x * x for x in range(6)], [x for x in range(20) if x % 7 == 0]);
print([k + "!" for k, v in [["a", 1], ["b", 2]] if v > 1]);
print([n if n % 2 == 0 else -n for n in range(5)]);

# dicts
let d = {"one": 1, "two": 2};
d["three"] = 3;
d["one"] += 10;
print(d, len(d), d["two"], d.get("four"), d.get("four", 4), "one" in d, "zzz" in d);
print(d.keys(), d.values(), d.items());
print(d.pop("two"), d);
let e = d.copy();
e.update({"x": 0, "one": -1});
print(e, d);
let keyed = {1: "int", 1.5: "float", True: "bool-is-1", None: "none"};
print(keyed[1], keyed[1.0], keyed[1.5], keyed[None], len(keyed));
d.clear();
print(d);

# nested structures
let grid = [[0] * 3 for _ in range(2)];
grid[1][2] = 5;
print(grid);
let inv = {"items": ["sword"], "gold": 10};
inv["items"].append("shield");
print(inv["items"], len(inv["items"]));
