# A small GUI app: controls in a side panel, an animated canvas in the middle.
# Click the canvas to add a ball. Run: faston examples/gui_demo.fst
import gui, math, random;

class Ball {
    def init(self, x, y) {
        self.x = x;
        self.y = y;
        let angle = random.uniform(0, math.tau);
        self.vx = math.cos(angle) * 150;
        self.vy = math.sin(angle) * 150;
        self.r = random.randint(6, 16);
    }

    def step(self, dt, w, h, speed) {
        self.x += self.vx * dt * speed;
        self.y += self.vy * dt * speed;
        if (self.x < self.r or self.x > w - self.r) { self.vx = -self.vx; }
        if (self.y < self.r or self.y > h - self.r) { self.vy = -self.vy; }
        self.x = min(max(self.x, self.r), w - self.r);
        self.y = min(max(self.y, self.r), h - self.r);
    }
}

let balls = [Ball(200, 150) for _ in range(12)];
let speed = 1.0;
let paused = False;
let color = "#ff8800";
let trail = True;
let theme = "dark";

def controls(ui) {
    global speed, paused, color, trail, theme;
    ui.heading("Balls");
    ui.label(f"{len(balls)} balls, {ui.fps()} fps", color="gray");
    ui.separator();
    speed = ui.slider("speed", speed, 0.0, 3.0);
    paused = ui.checkbox("paused", paused);
    trail = ui.checkbox("glow", trail);
    ui.horizontal(lambda (row) {
        global color;
        row.label("color");
        color = row.color(color);
    });
    let new_theme = ui.combo("theme", ["dark", "light"], theme);
    if (new_theme != theme) {
        theme = new_theme;
        ui.set_dark(theme == "dark");
    }
    ui.space(8);
    ui.horizontal(lambda (row) {
        if (row.button("+5")) {
            for (_ in range(5)) { balls.append(Ball(100, 100)); }
        }
        if (row.button("Clear", enabled=len(balls) > 0)) { balls.clear(); }
    });
    ui.space(8);
    ui.collapsing("Help", lambda (h) {
        h.label("Click the canvas to add a ball.");
        h.label("Press space to pause.");
    });
}

def draw(ui) {
    global paused;
    if (ui.key_pressed("space")) { paused = not paused; }
    let c = ui.canvas(background="#14181f" if theme == "dark" else "#f4f4f4");
    let w, h = c.width, c.height;
    if (not paused) {
        let dt = min(ui.dt(), 0.05);
        for (b in balls) { b.step(dt, w, h, speed); }
    }
    for (b in balls) {
        if (trail) { c.circle(b.x, b.y, b.r + 4, gui.rgb(255, 255, 255, 25)); }
        c.circle(b.x, b.y, b.r, color);
    }
    if (c.clicked()) {
        let x, y = c.mouse();
        balls.append(Ball(x, y));
    }
    if (paused) {
        c.text(w / 2, h / 2 - 12, "paused", "gray", size=24, align="center");
    }
}

gui.run(draw, title="Faston balls", width=900, height=600, animate=True, dark=True, left=controls);
