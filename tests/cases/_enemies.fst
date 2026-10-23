print("loading _enemies");
let count = 0;
const MAX_HP = 50;
let name = "_enemies-module";      # must not clash with main's `name`

class Slime {
    def init(self, hp = MAX_HP) { self.hp = hp; }
    def hit(self, d) { self.hp -= d; return self.hp; }
}

def spawn(n) {
    global count;
    count += n;
    return [Slime() for _ in range(n)];
}

def helper_uses_util() {
    import _util;                  # nested import, relative to this file
    return _util.double(count);
}
