# Game entities, imported by main.fst.
import math;

const ARENA = 100.0;

class Entity {
    def init(self, name, x, y, hp = 100) {
        self.name = name;
        self.x = x;
        self.y = y;
        self.hp = hp;
        self.listeners = [];
    }

    def on_death(self, fn) { self.listeners.append(fn); }

    def damage(self, amount) {
        self.hp -= amount;
        if (self.hp <= 0) {
            for (fn in self.listeners) { fn(self); }
        }
    }

    def dist(self, other) { return math.hypot(self.x - other.x, self.y - other.y); }
}

# A closure-based spawner: each call remembers how many it has made.
def make_spawner(prefix) {
    let made = 0;
    def spawn(x, y) {
        nonlocal made;
        made += 1;
        return Entity(f"{prefix}{made}", x, y, 30);
    }
    return spawn;
}
