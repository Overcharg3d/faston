class Entity {
    def init(self, name, hp = 100) {
        self.name = name;
        self.hp = hp;
    }
    def hit(self, dmg) {
        self.hp -= dmg;
        return self;
    }
    def alive(self) { return self.hp > 0; }
    def describe(self) { return f"{self.name}:{self.hp}"; }
}

class Player(Entity) {
    def init(self, name, lives) {
        Entity.init(self, name, 120);
        self.lives = lives;
    }
    def describe(self) {
        return Entity.describe(self) + f" x{self.lives}";
    }
}

class Boss(Entity) {
    def describe(self) { return "BOSS " + self.name; }
}

let p = Player("hero", 3);
let e = Entity("slime", 10);
let b = Boss("dragon", 500);
p.hit(20).hit(5);
e.hit(15);
print(p.describe(), e.describe(), b.describe());
print(p.alive(), e.alive(), b.hp);
print(isinstance(p, Entity), isinstance(p, Player), isinstance(e, Player), isinstance(b, Boss));
print(type(p), p.name, Player);

# the same call site seeing different classes (inline cache must re-check)
let mixed = [p, e, b, Entity("rat", 1), Player("ally", 1)];
print([m.describe() for m in mixed]);
let hps = [];
for (m in mixed) { hps.append(m.hp); }
print(hps);

# bound methods are values
let f = p.describe;
print(f());

# fields can be added later and can hold functions
class Box { pass; }
let bx = Box();
bx.value = 41;
bx.value += 1;
bx.fn = len;
print(bx.value, bx.fn("abc"));

# a field that shadows a method after calls were cached
class Shape {
    def init(self) { self.sides = 0; }
    def area(self) { return "method"; }
}
def area_of(s) { return s.area(); }
let s1 = Shape();
print(area_of(s1), area_of(s1));
def lambda_like() { return "field"; }
let s2 = Shape();
s2.area = lambda_like;
print(area_of(s2), area_of(s1));

# objects referencing each other (cycle) + equality is identity
class Node { def init(self, v) { self.v = v; self.next = None; } }
let n1 = Node(1);
let n2 = Node(2);
n1.next = n2;
n2.next = n1;
print(n1.next.next.v, n1 == n1, n1 == n2);
