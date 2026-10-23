# Game-style update loop: move particles, bounce off walls, sum energy.
class Particle {
    def init(self, x, y, vx, vy) {
        self.x = x; self.y = y; self.vx = vx; self.vy = vy;
    }
    def update(self, dt) {
        self.x += self.vx * dt;
        self.y += self.vy * dt;
        if (self.x < 0.0 or self.x > 800.0) { self.vx = -self.vx; }
        if (self.y < 0.0 or self.y > 600.0) { self.vy = -self.vy; }
    }
}

def main() {
    let ps = [];
    for (i in range(1000)) {
        ps.append(Particle(float(i % 800), float(i % 600), float(i % 7) - 3.0, float(i % 5) - 2.0));
    }
    let energy = 0.0;
    for (frame in range(1000)) {
        for (p in ps) {
            p.update(0.016);
        }
    }
    for (p in ps) { energy += p.vx * p.vx + p.vy * p.vy; }
    print(round(energy, 3));
}
main();
