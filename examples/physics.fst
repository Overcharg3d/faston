# A tiny physics step: operator overloading, tuples, sets, keyword arguments
# and exceptions working together.
import math;

class Vec2 {
    def init(self, x = 0.0, y = 0.0) { self.x = x; self.y = y; }
    def __add__(self, o) { return Vec2(self.x + o.x, self.y + o.y); }
    def __sub__(self, o) { return Vec2(self.x - o.x, self.y - o.y); }
    def __mul__(self, k) { return Vec2(self.x * k, self.y * k); }
    def __rmul__(self, k) { return self * k; }
    def __neg__(self) { return Vec2(-self.x, -self.y); }
    def __eq__(self, o) { return isinstance(o, Vec2) and self.x == o.x and self.y == o.y; }
    def __iter__(self) { return (self.x, self.y); }
    def __repr__(self) { return f"Vec2({self.x:.2f}, {self.y:.2f})"; }
    def length(self) { return math.hypot(self.x, self.y); }
}

class PhysicsError(Exception) { pass; }

class Body {
    def init(self, name, pos, vel = None, mass = 1.0, *tags, **props) {
        if (mass <= 0) { raise PhysicsError(f"{name}: mass must be positive"); }
        self.name = name;
        self.pos = pos;
        self.vel = vel if vel != None else Vec2();
        self.mass = mass;
        self.tags = set(tags);
        self.props = props;
    }
    def __repr__(self) { return f"<{self.name} at {self.pos}>"; }
}

const GRAVITY = Vec2(0.0, -9.8);
const CELL = 5.0;

def step(bodies, dt = 1 / 60) {
    for (b in bodies) {
        if ("static" in b.tags) { continue; }
        b.vel = b.vel + GRAVITY * dt;
        b.pos = b.pos + dt * b.vel;
        if (b.pos.y < 0) {
            b.pos = Vec2(b.pos.x, 0.0);
            b.vel = Vec2(b.vel.x, -b.vel.y * b.props.get("bounce", 0.5));
        }
    }
}

def cell_of(b) {
    let x, y = b.pos;                      # unpacking uses __iter__
    return (int(x // CELL), int(y // CELL));   # tuples are hashable: grid keys
}

let world = [
    Body("ball", Vec2(1.0, 10.0), bounce=0.8),
    Body("crate", Vec2(6.0, 4.0), Vec2(2.0, 0.0), 3.0, "heavy"),
    Body("floor", Vec2(0.0, 0.0), None, 1000.0, "static"),
];

try {
    world.append(Body("ghost", Vec2(), mass=0));
} except (PhysicsError as e) {
    print("rejected:", e);
}

for (frame in range(120)) { step(world); }

let grid = {};
for (b in world) {
    let cell = cell_of(b);
    if (cell not in grid) { grid[cell] = []; }
    grid[cell].append(b.name);
}

print("after 2s:", world);
print("speeds:", {b.name: round(b.vel.length(), 2) for b in world});
print("grid:", grid);
print("tags:", world[1].tags | world[2].tags | {"dynamic"});
print("heaviest:", max(world, key=lambda (b) b.mass).name);
print("by height:", sorted(world, key=lambda (b) b.pos.y, reverse=True));
let total = sum([b.vel for b in world], Vec2());
print("total velocity:", total, "| moving:", sum(1 for b in world if b.vel.length() > 0.01));
