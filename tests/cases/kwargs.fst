def spawn(name, x = 0, y = 0, hp = 100) { return f"{name}@({x},{y}) hp={hp}"; }
print(spawn("slime"), spawn("bat", 3), spawn("orc", y=5), spawn(hp=1, name="boss", x=9));
def total(*nums) { return sum(nums); }
print(total(), total(1, 2, 3), total(*[4, 5], 6));
def config(title, *items, **opts) { return [title, items, opts]; }
print(config("a"), config("b", 1, 2, debug=True, level=3));
let args = [10, 20];
let kw = {"hp": 7, "y": 2};
print(spawn("x", *args), spawn("z", **kw), spawn(*["w"], **{"x": 1}));
def wrapper(*a, **k) { return spawn(*a, **k); }
print(wrapper("wrapped", hp=55));
class Enemy {
    def init(self, name, hp = 10, *tags, **extra) {
        self.name = name; self.hp = hp; self.tags = tags; self.extra = extra;
    }
    def heal(self, amount = 1) { self.hp += amount; return self.hp; }
}
let e = Enemy("rat", hp=3);
let f = Enemy("elite", 50, "fast", "fire", loot="gold");
print(e.name, e.hp, e.tags, e.extra, f.hp, f.tags, f.extra, e.heal(amount=4), e.heal());
print(sorted([3, 1, 2], reverse=True), sorted(["bb", "a", "ccc"], key=len), sorted(["bb", "a", "ccc"], key=len, reverse=True));
let words = ["kiwi", "fig", "apple", "date"];
words.sort(key=lambda (w) w[-1]);
print(words, max(words, key=len), min(words, key=len), max([], default="none"));
print(1, 2, 3, sep="-", end="!\n");
print("a", "b", sep="");
print(list(enumerate("ab", start=10)));
let add = lambda (a, b = 2) a + b;
print(add(b=5, a=1), add(1));
def errs() {
    let out = [];
    try { spawn(); } except (TypeError as ex) { out.append(str(ex)); }
    try { spawn("a", nope=1); } except (TypeError as ex) { out.append(str(ex)); }
    try { spawn("a", name="b"); } except (TypeError as ex) { out.append(str(ex)); }
    try { print(1, color=2); } except (TypeError as ex) { out.append(str(ex)); }
    return out;
}
for (m in errs()) { print(m); }
