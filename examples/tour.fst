# A tour of Faston: Python semantics, with () + {} + ; syntax.
import math, random;

# ---- variables
let speed = 10;
const NAME = "Faston";
score = 0;                         # plain assignment works too
let x, y = 3, 4;                   # multiple assignment
x, y = y, x;                       # swap
print(NAME, "speed", speed, "x/y", x, y);

# ---- functions (default arguments, multiple return values)
def add(a, b = 1) {
    return a + b;
}
def divmod2(a, b) {
    return a // b, a % b;
}
let q, r = divmod2(17, 5);
print("add:", add(2, 3), add(2), "divmod:", q, r);

# ---- control flow
if (speed > 5) {
    print("fast");
} elif (speed > 0) {
    print("slow");
} else {
    print("stopped");
}

for (i in range(3)) {
    print("i =", i);
}

while (speed > 7) {
    speed -= 1;
}
print("speed now", speed, "| in range:", 0 < speed < 10, "| parity:", "even" if speed % 2 == 0 else "odd");

# ---- classes & inheritance
class Entity {
    def init(self, name, hp = 100) {
        self.name = name;
        self.hp = hp;
    }

    def hit(self, dmg) {
        self.hp -= dmg;
        return self.hp;
    }

    def describe(self) {
        return f"{self.name} ({self.hp} hp)";
    }
}

class Player(Entity) {
    def init(self, name, lives) {
        Entity.init(self, name);   # call the parent's init
        self.lives = lives;
    }

    def describe(self) {
        return f"{self.name} ({self.hp} hp, {self.lives} lives)";
    }
}

let p = Player("Overcharg3d", 3);
p.hit(25);
print(p.describe());
print(isinstance(p, Entity));

# ---- collections
let nums = [1, 2, 3, 4, 5, 6];
let squares = [n * n for n in nums];
let evens = [n for n in nums if n % 2 == 0];
let stats = {"hp": 100, "mp": 50};
stats["mp"] += 25;
print(squares, evens, nums[1:4], nums[::-1], stats);

for (key, value in stats.items()) {
    print(f"{key} -> {value}");
}

# ---- strings & f-string formatting
let words = "the quick brown fox".split();
print(", ".join([w.upper() for w in words]));
print(f"pi ~ {math.pi:.3f} | {0.875:.0%} | [{42:>5}] | {1234567:,}");
print(len(words), "the" in words, 7 // 2, 7 / 2, 2 ** 10, -7 % 3);

# ---- modules
random.seed(7);
let roll = random.randint(1, 6);
print("rolled a", roll, "| hypot:", math.hypot(3, 4));
